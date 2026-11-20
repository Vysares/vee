//! A hand written executive: the loop that `exec!` will one day generate. It runs three test
//! modules and the real `Core` against a fake clock and a fake file system, and the tests read the
//! events from the event log the core writes.
//!
//! What it shows about `exec!`:
//! - Everything one module needs is the same for every module, so it is one generic type,
//!   `Slot<M>`: the module, its throttle, its source id and its rate. `exec!` does not have to
//!   generate that logic, only the list of slots and their order.
//! - `Exec` is the part `exec!` generates: one field for each module, and a `tick` that does the
//!   same three steps for every module, in order.
//! - `core.tick(now)` is the core's step: it takes the events out of the queue and writes them to
//!   the event log (for now with a blocking write, until the file service exists).
//! - `Exec::new` is the core's startup, `vee_core::Core::start` (the reset log and the event log),
//!   so the first event of the run is the core's reset report.
//! - Not here yet: commands, the bus, the other core services, and the end of the real startup
//!   (`std_syslog::start_queue` followed by `started`). The system log stays in direct mode here,
//!   so its lines come out in order.

#![allow(
    clippy::unwrap_used,
    reason = "a test file: a failed unwrap is a failed test (the helpers are not `#[test]` fns, \
              so clippy.toml's exemption does not cover them)"
)]

use std::sync;
use vee_core::suppressed;
use vee_core_api::ctx;
use vee_core_api::ev;
use vee_core_api::event;
use vee_core_api::event::Code as _;
use vee_core_api::module;
use vee_core_api::reset;
use vee_core_api::throttle;
use vee_core_types::core_event;
use vee_core_types::event_log as log_types;
use vee_osal::clock;
use vee_osal::clock::Clock as _;
use vee_osal::fake_clock;
use vee_osal::fake_files;
use vee_osal::std_clock;
use vee_osal::std_syslog;

const LIMIT: u8 = throttle::EV_MAX_PER_TICK;

/// Where the core writes the event log, and keeps the reset log.
const EVENT_LOG: &str = "events.log";
const RESET_LOG: &str = "reset.log";

/// The time between the start of one tick and the start of the next.
const TICK_US: u64 = 10_000;
/// How much the fake clock moves while one module's call runs, so each call sees its own time.
const MODULE_COST_US: u64 = 100;

const HEARTBEAT: u8 = 1;
const SLOW: u8 = 2;
const NOISY: u8 = 3;

/// How many sends of `Spam` the noisy module is over the limit by, every step.
const SPAM_OVER_LIMIT: u8 = 3;

// ---- Module 1: sends one event every step ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, event::Code)]
#[repr(u16)]
enum HeartbeatEv {
    #[severity(Info)]
    Started,
    #[severity(Info)]
    Beat,
}

struct Heartbeat {
    beats: u32,
}

impl module::Module for Heartbeat {
    type Event = HeartbeatEv;

    fn init(&mut self, ctx: &mut ctx::Ctx<'_, Self>) {
        ctx.ev.send(HeartbeatEv::Started, format_args!("started"));
    }

    fn step(&mut self, ctx: &mut ctx::Ctx<'_, Self>) {
        self.beats = self.beats.saturating_add(1);
        let beats = self.beats;
        ctx.ev.send(HeartbeatEv::Beat, format_args!("beat {beats}"));
    }
}

// ---- Module 2: runs at a quarter of the rate, and reports the time it sees ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, event::Code)]
#[repr(u16)]
enum SlowEv {
    #[severity(Info)]
    Started,
    #[severity(Info)]
    Report,
}

struct Slow;

impl module::Module for Slow {
    type Event = SlowEv;

    fn init(&mut self, ctx: &mut ctx::Ctx<'_, Self>) {
        ctx.ev.send(SlowEv::Started, format_args!("started"));
    }

    fn step(&mut self, ctx: &mut ctx::Ctx<'_, Self>) {
        let now = ctx.monotonic();
        ctx.ev.send(SlowEv::Report, format_args!("report at {now}"));
    }
}

// ---- Module 3: sends the same event too often, every step ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, event::Code)]
#[repr(u16)]
enum NoisyEv {
    #[severity(Info)]
    Started,
    #[severity(Err)]
    Spam,
    #[severity(Info)]
    Quiet,
}

struct Noisy;

impl module::Module for Noisy {
    type Event = NoisyEv;

    fn init(&mut self, ctx: &mut ctx::Ctx<'_, Self>) {
        ctx.ev.send(NoisyEv::Started, format_args!("started"));
    }

    fn step(&mut self, ctx: &mut ctx::Ctx<'_, Self>) {
        for _ in 0..LIMIT.saturating_add(SPAM_OVER_LIMIT) {
            ctx.ev.send(NoisyEv::Spam, format_args!("spam"));
        }
        ctx.ev.send(NoisyEv::Quiet, format_args!("quiet"));
    }
}

// ---- The executive ----

/// Everything the executive keeps for one module. The same for every module, so it is generic.
struct Slot<M: module::Module> {
    module: M,
    throttle: throttle::Throttle,
    /// The module's id. It goes into every event the module sends.
    source: u8,
    /// The module steps on every `period`th tick. 1 is every tick.
    period: u64,
}

impl<M: module::Module> Slot<M> {
    fn new(module: M, source: u8, period: u64) -> Self {
        Slot {
            module,
            throttle: throttle::Throttle::new(),
            source,
            period,
        }
    }

    /// Startup: reads the clock, builds the module's `Ctx` and calls `init`.
    fn init(&mut self, ev: &mut ev::Ev, clock: &impl clock::Clock, reset: reset::ResetInfo) {
        let time = clock.monotonic_us();
        log::debug!("source {} init at {time} us", self.source);
        let mut ctx = ctx::Ctx::<M>::new(ev, &mut self.throttle, self.source, time, reset);
        self.module.init(&mut ctx);
    }

    /// Top of a tick: sends the core's report of what this module had suppressed in the tick
    /// before, as events from the core at `now`. It also starts the module's new tick of counting.
    fn report_suppressed(&mut self, ev: &mut ev::Ev, now: u64) {
        suppressed::report::<M::Event, _>(&mut self.throttle, self.source, |code, text| {
            ev.send(core_event::SOURCE, now, code, text);
        });
    }

    /// Calls `step` if this tick is one of the module's. The clock is read just before the call.
    fn step(
        &mut self,
        tick: u64,
        ev: &mut ev::Ev,
        clock: &impl clock::Clock,
        reset: reset::ResetInfo,
    ) {
        if tick.checked_rem(self.period) != Some(0) {
            return;
        }
        let time = clock.monotonic_us();
        log::debug!("source {} steps at {time} us", self.source);
        let mut ctx = ctx::Ctx::<M>::new(ev, &mut self.throttle, self.source, time, reset);
        self.module.step(&mut ctx);
    }
}

/// What `exec!` will generate: a field for each module, and the same steps for each, in order.
struct Exec {
    clock: fake_clock::FakeClock,
    core: vee_core::Core<fake_files::FakeFiles>,
    /// How many ticks have run.
    tick: u64,
    heartbeat: Slot<Heartbeat>,
    slow: Slot<Slow>,
    noisy: Slot<Noisy>,
}

impl Exec {
    /// The core's startup (the reset log and the event log), before any module's `init`.
    fn new() -> Self {
        let clock = fake_clock::FakeClock::new();
        let core = vee_core::Core::start(
            fake_files::FakeFiles::new(),
            RESET_LOG,
            EVENT_LOG,
            clock.monotonic_us(),
        );
        Exec {
            clock,
            core,
            tick: 0,
            heartbeat: Slot::new(Heartbeat { beats: 0 }, HEARTBEAT, 1),
            slow: Slot::new(Slow, SLOW, 4),
            noisy: Slot::new(Noisy, NOISY, 1),
        }
    }

    /// Startup: `init` for each module once, in order. Each call takes `MODULE_COST_US`.
    fn init(&mut self) {
        let reset = self.core.reset();
        self.heartbeat.init(&mut self.core.ev, &self.clock, reset);
        self.clock.advance(MODULE_COST_US);
        self.slow.init(&mut self.core.ev, &self.clock, reset);
        self.clock.advance(MODULE_COST_US);
        self.noisy.init(&mut self.core.ev, &self.clock, reset);
        self.clock.advance(MODULE_COST_US);
    }

    /// One tick. Tick `n` starts at `(n + 1) * TICK_US`, after the startup calls.
    fn tick(&mut self) {
        let reset = self.core.reset();
        let now = self.tick.saturating_add(1).saturating_mul(TICK_US);
        self.clock.set(now);
        // The line's own time is real time, not the fake clock, so the fake time is in the text.
        log::info!("tick {} starts at {now} us", self.tick);

        // 1. Each module's suppressed events from the tick before, and a new count for each.
        self.heartbeat.report_suppressed(&mut self.core.ev, now);
        self.slow.report_suppressed(&mut self.core.ev, now);
        self.noisy.report_suppressed(&mut self.core.ev, now);

        // 2. The core takes the events out of the queue (the reports above, and the modules'
        //    events from the tick before) and writes them to the event log.
        self.core.tick(now);

        // 3. Each module that is due steps, in order. Each call takes `MODULE_COST_US`, due or not,
        //    so a module always sees the same time within its tick.
        self.heartbeat
            .step(self.tick, &mut self.core.ev, &self.clock, reset);
        self.clock.advance(MODULE_COST_US);
        self.slow
            .step(self.tick, &mut self.core.ev, &self.clock, reset);
        self.clock.advance(MODULE_COST_US);
        self.noisy
            .step(self.tick, &mut self.core.ev, &self.clock, reset);
        self.clock.advance(MODULE_COST_US);

        self.tick = self.tick.saturating_add(1);
    }

    /// Shutdown, after the last tick: the core's `stop`, with the time now.
    fn stop(&mut self) {
        let now = self.clock.monotonic_us();
        self.core.stop(now, reset::ShutdownReason::Commanded);
    }

    /// The events in the event log so far, oldest first. The events of the last tick's modules
    /// are not in it yet: they are still in the queue until the core's next tick.
    fn event_log(&self) -> Vec<event::Event> {
        let bytes = self.core.files().get(EVENT_LOG).unwrap_or_default();
        bytes
            .chunks(log_types::RECORD_LEN)
            .map(|record| log_types::decode(record).unwrap())
            .collect()
    }

    /// The events in the event log, oldest first, after one more core tick has written the last
    /// tick's module events. (A test that stops the executive reads `event_log` after `stop`.)
    fn logged(&mut self) -> Vec<event::Event> {
        let now = self.tick.saturating_add(1).saturating_mul(TICK_US);
        self.core.tick(now);
        self.event_log()
    }
}

/// Installs the system log for the whole test program, but only if `VEE_TEST_LOG` names a level,
/// for example `VEE_TEST_LOG=debug cargo test -p vee-core --test exec_loop`. Otherwise the tests
/// are silent. (The logger writes straight to stderr, which `cargo test` does not capture, so
/// installing it always would print lines for every passing test.) Every test calls this, and
/// only the first call does anything.
fn init_log() {
    static ONCE: sync::Once = sync::Once::new();
    ONCE.call_once(|| {
        let Ok(level) = std::env::var("VEE_TEST_LOG") else {
            return;
        };
        // A level that does not parse fails the test, rather than quietly logging nothing.
        let level: log::LevelFilter = level.parse().unwrap();
        std_syslog::init(std_clock::StdClock::new(), level).unwrap();
    });
}

/// A started executive that has run `ticks` ticks.
fn run(ticks: u64) -> Exec {
    init_log();
    let mut exec = Exec::new();
    exec.init();
    for _ in 0..ticks {
        exec.tick();
    }
    exec
}

fn text(event: &event::Event) -> &str {
    std::str::from_utf8(event.text.as_bytes()).unwrap()
}

fn from_source(events: &[event::Event], source: u8) -> Vec<&event::Event> {
    events.iter().filter(|e| e.source == source).collect()
}

// ---- Tests ----

#[test]
fn init_runs_once_for_each_module_in_order_with_its_own_source_and_time() {
    let events = run(0).logged();
    let seen: Vec<(u8, u64, &str)> = events
        .iter()
        .map(|e| (e.source, e.monotonic_us, text(e)))
        .collect();
    assert_eq!(
        seen,
        [
            (
                core_event::SOURCE,
                0,
                "reset 1: prev end unknown, 0 unclean in a row"
            ),
            (HEARTBEAT, 0, "started"),
            (SLOW, MODULE_COST_US, "started"),
            (NOISY, 2 * MODULE_COST_US, "started"),
        ]
    );
}

#[test]
fn modules_step_at_their_own_rates() {
    // Ticks 0 to 7. The slow module steps on ticks 0 and 4.
    let events = run(8).logged();

    let beats = from_source(&events, HEARTBEAT);
    assert_eq!(beats.len(), 1 + 8); // the init event, then one a tick
    assert_eq!(text(beats[8]), "beat 8");

    let slow = from_source(&events, SLOW);
    assert_eq!(slow.len(), 1 + 2);
    // Tick 0 starts at TICK_US and the slow module is the second call. Tick 4 starts at 5 ticks.
    assert_eq!(
        text(slow[1]),
        format!("report at {}", TICK_US + MODULE_COST_US)
    );
    assert_eq!(
        text(slow[2]),
        format!("report at {}", 5 * TICK_US + MODULE_COST_US)
    );
}

#[test]
fn each_call_sees_the_time_read_just_before_it() {
    // Tick 0: starts at TICK_US, and each call before this one took MODULE_COST_US.
    let events = run(1).logged();
    let init_events = 4; // the core's reset report, and the three modules' `started`
    let tick_events: Vec<_> = events.iter().skip(init_events).collect();

    assert_eq!(tick_events[0].source, HEARTBEAT);
    assert_eq!(tick_events[0].monotonic_us, TICK_US);
    assert_eq!(tick_events[1].source, SLOW);
    assert_eq!(tick_events[1].monotonic_us, TICK_US + MODULE_COST_US);
    // Every event of one call has that call's time, however many it sends.
    for event in &tick_events[2..] {
        assert_eq!(event.source, NOISY);
        assert_eq!(event.monotonic_us, TICK_US + 2 * MODULE_COST_US);
    }
}

#[test]
fn a_noisy_module_is_limited_in_each_tick_and_reported_at_the_top_of_the_next() {
    let events = run(3).logged();

    let noisy = from_source(&events, NOISY);
    let spam = noisy.iter().filter(|e| text(e) == "spam").count();
    let quiet = noisy.iter().filter(|e| text(e) == "quiet").count();
    assert_eq!(spam, 3 * usize::from(LIMIT)); // the limit, in each of 3 ticks
    assert_eq!(quiet, 3); // a different event has its own count

    // The third tick's suppressed sends are reported at the top of the fourth, which has not run.
    let reports: Vec<_> = from_source(&events, core_event::SOURCE)
        .into_iter()
        .filter(|e| e.code == core_event::CoreEvent::EventsSuppressed.code())
        .collect();
    assert_eq!(reports.len(), 2);
    let expected = format!("suppressed: source {NOISY} code 1, {SPAM_OVER_LIMIT} dropped");
    assert_eq!(text(reports[0]), expected);
    assert_eq!(reports[0].monotonic_us, 2 * TICK_US); // the start of tick 1
    assert_eq!(text(reports[1]), expected);
    assert_eq!(reports[1].monotonic_us, 3 * TICK_US); // the start of tick 2
}

#[test]
fn each_tick_has_the_core_report_first_then_the_modules_in_order() {
    let events = run(2).logged();
    let sources: Vec<u8> = events.iter().map(|e| e.source).collect();

    let noisy_tick = vec![NOISY; usize::from(LIMIT) + 1]; // the allowed Spam, and Quiet
    let mut expected = vec![core_event::SOURCE, HEARTBEAT, SLOW, NOISY]; // startup, then init
    // Tick 0: no report yet. Heartbeat, slow (it steps on tick 0), noisy.
    expected.extend([HEARTBEAT, SLOW]);
    expected.extend(&noisy_tick);
    // Tick 1: the core's report of the noisy module first. Slow does not step.
    expected.push(core_event::SOURCE);
    expected.push(HEARTBEAT);
    expected.extend(&noisy_tick);
    assert_eq!(sources, expected);
}

#[test]
fn a_clean_stop_writes_the_last_ticks_events_and_the_next_start_sees_a_clean_end() {
    let mut exec = run(2);
    let before = exec.event_log();

    exec.stop();

    // The last tick's module events were still queued. Now they are all in the log, and the
    // queue is empty.
    let after = exec.event_log();
    assert!(exec.core.ev.is_empty());
    assert!(after.len() > before.len());
    let last = after.last().unwrap();
    assert_eq!((last.source, text(last)), (NOISY, "quiet"));
    assert_eq!(last.monotonic_us, 2 * TICK_US + 2 * MODULE_COST_US); // tick 1, the third call
    assert!(before.last().unwrap().monotonic_us < last.monotonic_us);

    // The next boot, on a copy of what this run left in the reset log, sees a clean end.
    let files = fake_files::FakeFiles::new();
    files.put(RESET_LOG, &exec.core.files().get(RESET_LOG).unwrap());
    let next = vee_core::Core::start(files, RESET_LOG, EVENT_LOG, 0);
    assert_eq!(
        next.reset(),
        reset::ResetInfo {
            count: 2,
            previous: reset::PreviousEnd::Clean(reset::ShutdownReason::Commanded),
            consecutive_unclean: 0,
        }
    );
}
