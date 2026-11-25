//! The sample deployment: a Vee executable, written by hand. It is what `exec!` will one day
//! generate, so it is kept plain and complete, and it grows with Vee.
//!
//! Run it from this directory, so that `.cargo/config.toml` (the build-time settings) applies:
//! `cd sample-deployment && cargo run`. It writes its logs to the current directory.
//!
//! It starts Vee (see `start`) and its three modules (`vee-example`, which publishes a bus topic,
//! `vee-watcher`, which reads it, and the `pinger`, which sends the core a `Noop` command once a
//! second through a command sink), and then runs the loop (see
//! `Exec::run`) at 100 Hz until it is killed. Set `VEE_SAMPLE_TICKS=250` (any number) to run that
//! many ticks and then stop cleanly, which is how to see a clean shutdown (and the next boot
//! report it).
#![forbid(unsafe_code)]

mod pinger;

use vee_core::Core;
use vee_core::slot;
use vee_core_api::bus;
use vee_core_api::cmd;
use vee_core_api::reset;
use vee_core_types::core_event;
use vee_example as example;
use vee_osal::clock::Clock as _;
use vee_osal::linux;
use vee_osal::linux::std_alloc_guard;
use vee_osal::linux::std_clock;
use vee_osal::linux::std_files;
use vee_osal::linux::std_panic;
use vee_osal::linux::std_syslog;
use vee_osal::linux::std_watchdog;
use vee_watcher as watcher;

/// The allocation guard, in builds that ask for it (`--features alloc-guard`): every allocation is
/// counted, and once `start` has called `std_alloc_guard::arm()`, any allocation aborts. A program has
/// one global allocator, so this is the one place it is chosen.
#[cfg(feature = "alloc-guard")]
#[global_allocator]
static GUARD: std_alloc_guard::GuardAlloc = std_alloc_guard::GuardAlloc;

/// Where the reset log and the event log are: the current directory, for a sample.
const RESET_LOG: &str = "reset.log";
const EVENT_LOG: &str = "events.log";

/// The modules' ids, which go into every event they send. 0 is the core's.
const EXAMPLE_SOURCE: u8 = 1;
const WATCHER_SOURCE: u8 = 2;
const PINGER_SOURCE: u8 = 3;

/// The pinger's command sink. A sink's id says where commands come from (ground, an RTS, ...),
/// and is not a module's id: this one stands in for the ground link.
const PINGER_SINK: u8 = 1;

/// The time between the start of one tick and the start of the next: a tick rate of 100 Hz. Ticks
/// start at multiples of this on the clock, so a tick has a number: its start time divided by this.
const TICK_US: u64 = 10_000;

/// The example module steps on every this many ticks. With a tick rate of 100 Hz that is once a
/// second.
const EXAMPLE_PERIOD_TICKS: u64 = 100;

/// The watcher steps on every this many ticks: once every two seconds, half as often as the
/// example publishes, so it misses every other value (a state topic keeps only the latest).
const WATCHER_PERIOD_TICKS: u64 = 200;

/// The pinger steps on every this many ticks: once a second, like the example.
const PINGER_PERIOD_TICKS: u64 = 100;

/// The system log keeps messages at this level and more severe.
const LOG_LEVEL: log::LevelFilter = log::LevelFilter::Info;

/// Why Vee could not start. Only for failures that are bugs in this start function, because
/// startup does not fail for anything else: the core and `start` carry on and report. `main`
/// returns it, and Rust then prints it to stderr and exits with an error.
#[derive(Debug)]
enum StartError {
    /// A system log is installed already, or the system log was not installed.
    SystemLog(log::SetLoggerError),
    /// A panic hook is installed already.
    PanicHook(std_panic::InstallError),
    /// `VEE_SAMPLE_TICKS` is set and is not a whole number.
    InvalidTickLimit,
}

impl From<log::SetLoggerError> for StartError {
    fn from(error: log::SetLoggerError) -> Self {
        StartError::SystemLog(error)
    }
}

impl From<std_panic::InstallError> for StartError {
    fn from(error: std_panic::InstallError) -> Self {
        StartError::PanicHook(error)
    }
}

/// The start of the first tick after `time_us`: the next multiple of `TICK_US` that is later.
/// (If the sum could overflow, which a clock in microseconds will not do in 584,000 years, it is
/// the largest time.)
fn next_boundary_after(time_us: u64) -> u64 {
    time_us
        .checked_div(TICK_US)
        .and_then(|ticks| ticks.checked_add(1))
        .and_then(|ticks| ticks.checked_mul(TICK_US))
        .unwrap_or(u64::MAX)
}

/// How many tick starts were missed: a tick that began at `start_us` should be followed by one at
/// `start_us + TICK_US`, and the next one began at `next_start_us`.
fn ticks_skipped(start_us: u64, next_start_us: u64) -> u64 {
    next_start_us
        .saturating_sub(start_us.saturating_add(TICK_US))
        .checked_div(TICK_US)
        .unwrap_or(0)
}

/// The executive: the core, the clock, the bus topics and one `Slot` for each module. This is
/// the part `exec!` will generate (a field for each topic and module, and the calls in order).
/// `Slot` does the work that is the same for every module (see `vee_core::slot`).
///
/// The executive OWNS the topics. For each call into a module it makes the handles the module
/// declared (`Module::In` and `Module::Out`) from them, and the module gets them in its `Ctx`.
struct Exec {
    clock: std_clock::StdClock,
    core: Core<linux::LinuxPlatform>,
    /// `Example`'s progress: written by `example`, read by `watcher`.
    progress: bus::StateTopic<example::Progress>,
    /// The pinger's command sink: the core's router empties it at the start of every tick.
    pinger_sink: cmd::Sink,
    example: slot::Slot<example::Example>,
    watcher: slot::Slot<watcher::Watcher>,
    pinger: slot::Slot<pinger::Pinger>,
}

impl Exec {
    /// Tick number `tick`, in the order the design gives (see "Executive Requirements"):
    /// 1. Each module's suppressed events from the tick before, and a new count for each.
    /// 2. The router (`Core::route_commands`): every command the sinks hold goes to its target's
    ///    queue and is acked. No module has commands yet, so the only target is the core's own.
    /// 3. The core's tick: it runs its own queued commands, the event queue is written to the
    ///    event log, and the watchdog is fed.
    /// 4. Each module that is due steps, in order: `example` (which writes `progress`), then
    ///    `watcher` (which reads it), so the watcher sees the value from this tick, then
    ///    `pinger` (which submits a command, routed at the start of the next tick).
    fn tick(&mut self, tick: u64) {
        let now = self.clock.monotonic_us();

        self.example.report_suppressed(&mut self.core.ev, now);
        self.watcher.report_suppressed(&mut self.core.ev, now);
        self.pinger.report_suppressed(&mut self.core.ev, now);
        self.core
            .route_commands(std::slice::from_mut(&mut self.pinger_sink), &mut [], now);
        self.core.tick(now);
        self.step_example(tick);
        self.step_watcher(tick);
        self.step_pinger(tick);
    }

    /// One call into `example`, with a writer for `progress`. The clock is read once, and that
    /// time is the call's: the writer stamps the value with it and `example` sees it as
    /// `ctx.monotonic()`.
    fn step_example(&mut self, tick: u64) {
        let reset = self.core.reset();
        let now = self.clock.monotonic_us();
        let out = example::ExampleOut {
            progress: self.progress.writer(now),
        };
        self.example.step(
            tick,
            &mut self.core.ev,
            now,
            reset,
            (),
            out,
            cmd::Inbox::empty(),
        );
    }

    /// One call into `watcher`, with a reader for `progress`.
    fn step_watcher(&mut self, tick: u64) {
        let reset = self.core.reset();
        let now = self.clock.monotonic_us();
        let inputs = watcher::WatcherIn {
            progress: self.progress.reader(),
        };
        self.watcher.step(
            tick,
            &mut self.core.ev,
            now,
            reset,
            inputs,
            (),
            cmd::Inbox::empty(),
        );
    }

    /// One call into `pinger`, with a handle on its sink.
    fn step_pinger(&mut self, tick: u64) {
        let reset = self.core.reset();
        let now = self.clock.monotonic_us();
        let out = pinger::PingerOut {
            cmd: self.pinger_sink.handle(),
        };
        self.pinger.step(
            tick,
            &mut self.core.ev,
            now,
            reset,
            (),
            out,
            cmd::Inbox::empty(),
        );
    }

    /// The loop: runs a tick at the start of each period, for `tick_limit` ticks, or for ever if
    /// there is no limit.
    ///
    /// A tick starts at the first boundary after the clock was read, and the next one at the
    /// first boundary after this one is done. A tick that takes longer than its period (an
    /// overrun) is not made up for: the boundaries it missed are skipped, nothing runs twice to
    /// catch up, and a module that was due on a skipped tick does not step. The skipped ticks are
    /// logged, with how late the loop woke and how long the tick's work took, because a late
    /// wake-up (seen on WSL) looks the same as a slow tick otherwise. (They will become an event
    /// when the executive's overrun events are designed.)
    fn run(&mut self, tick_limit: Option<u64>) {
        let mut start_us = next_boundary_after(self.clock.monotonic_us());
        let mut ticks_run: u64 = 0;
        loop {
            self.clock.sleep_until_us(start_us);
            let woke_us = self.clock.monotonic_us();
            self.tick(start_us.checked_div(TICK_US).unwrap_or(0));
            ticks_run = ticks_run.saturating_add(1);
            if tick_limit.is_some_and(|limit| ticks_run >= limit) {
                return;
            }

            let done_us = self.clock.monotonic_us();
            let next_start_us = next_boundary_after(done_us);
            let skipped = ticks_skipped(start_us, next_start_us);
            if skipped > 0 {
                // Two possible causes: the operating system woke the loop late (`late_us`), or
                // the tick's own work was long (`work_us`). The log says which.
                let late_us = woke_us.saturating_sub(start_us);
                let work_us = done_us.saturating_sub(woke_us);
                log::warn!(
                    "overrun: the tick at {start_us} us woke {late_us} us late and its work took {work_us} us, skipped {skipped}"
                );
            }
            start_us = next_start_us;
        }
    }

    /// Shutdown, after the last tick.
    fn stop(&mut self) {
        let now = self.clock.monotonic_us();
        self.core.stop(now, reset::ShutdownReason::Commanded);
    }
}

/// Vee's startup, in the order the design gives (see "Executive Requirements"). `clock` times
/// everything, so every time in the logs and the events counts from the same start.
///
/// 1. The system log, first, so that everything after it can be seen.
/// 2. The panic hook, in builds that abort on a panic (the release build).
/// 3. The watchdog, from the environment. If that fails, a watchdog that does nothing takes its
///    place and the error is reported once the core exists, because startup does not fail.
/// 4. The core (`Core::start`): the reset log and the event log.
/// 5. Each module's `init`, in order, with the handles it declared (bus topics, command sink).
/// 6. The system log's queue, and then the allocation guard (`arm`). The queue's thread
///    allocates, so it comes before `arm`.
fn start(clock: std_clock::StdClock) -> Result<Exec, StartError> {
    let began_us = clock.monotonic_us();

    std_syslog::init(clock, LOG_LEVEL)?;
    if cfg!(panic = "abort") {
        std_panic::install(clock)?;
    }

    let (watchdog, watchdog_error) = match std_watchdog::StdWatchdog::from_env() {
        Ok(watchdog) => (watchdog, None),
        Err(error) => (std_watchdog::StdWatchdog::disabled(), Some(error)),
    };

    let mut core = Core::<linux::LinuxPlatform>::start(
        std_files::StdFiles,
        watchdog,
        RESET_LOG,
        EVENT_LOG,
        clock.monotonic_us(),
    );
    if let Some(error) = watchdog_error {
        log::error!("the watchdog is off: {error}");
        core.ev.send(
            core_event::SOURCE,
            clock.monotonic_us(),
            core_event::CoreEvent::WatchdogUnavailable,
            format_args!("watchdog: {error}"),
        );
    }

    let mut example = slot::Slot::new(
        example::Example::new(),
        EXAMPLE_SOURCE,
        EXAMPLE_PERIOD_TICKS,
    );
    let mut watcher = slot::Slot::new(
        watcher::Watcher::new(),
        WATCHER_SOURCE,
        WATCHER_PERIOD_TICKS,
    );
    let mut pinger = slot::Slot::new(pinger::Pinger::new(), PINGER_SOURCE, PINGER_PERIOD_TICKS);
    let mut pinger_sink = cmd::Sink::new(PINGER_SINK);
    // The bus topic, which `Exec` takes over. Each `init` is given the handle its module declared.
    let mut progress = bus::StateTopic::new();
    let reset = core.reset();
    let now = clock.monotonic_us();
    let out = example::ExampleOut {
        progress: progress.writer(now),
    };
    example.init(&mut core.ev, now, reset, (), out, cmd::Inbox::empty());
    let now = clock.monotonic_us();
    let inputs = watcher::WatcherIn {
        progress: progress.reader(),
    };
    watcher.init(&mut core.ev, now, reset, inputs, (), cmd::Inbox::empty());
    let now = clock.monotonic_us();
    let out = pinger::PingerOut {
        cmd: pinger_sink.handle(),
    };
    pinger.init(&mut core.ev, now, reset, (), out, cmd::Inbox::empty());

    if let Err(error) = std_syslog::start_queue() {
        log::error!("the system log stays in direct mode: {error}");
        core.ev.send(
            core_event::SOURCE,
            clock.monotonic_us(),
            core_event::CoreEvent::SystemLogQueueUnavailable,
            format_args!("system log queue: {error}"),
        );
    }
    std_alloc_guard::arm();

    let startup_us = clock.monotonic_us().saturating_sub(began_us);
    log::info!("started in {startup_us} us");
    Ok(Exec {
        clock,
        core,
        progress,
        pinger_sink,
        example,
        watcher,
        pinger,
    })
}

/// How many ticks to run, from `VEE_SAMPLE_TICKS`: `None` (run until killed) if it is not set.
fn tick_limit() -> Result<Option<u64>, StartError> {
    match std::env::var("VEE_SAMPLE_TICKS") {
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(StartError::InvalidTickLimit),
        Ok(text) => text
            .parse()
            .map(Some)
            .map_err(|_| StartError::InvalidTickLimit),
    }
}

fn main() -> Result<(), StartError> {
    let tick_limit = tick_limit()?;
    let clock = std_clock::StdClock::new();
    let mut exec = start(clock)?;
    exec.run(tick_limit);
    exec.stop();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super as sample;

    #[test]
    fn the_next_boundary_is_the_next_multiple_of_the_tick_after_the_time() {
        assert_eq!(sample::next_boundary_after(0), sample::TICK_US);
        assert_eq!(sample::next_boundary_after(1), sample::TICK_US);
        assert_eq!(
            sample::next_boundary_after(sample::TICK_US - 1),
            sample::TICK_US
        );
        // Exactly on a boundary is not "after" it: the next tick is the one after.
        assert_eq!(
            sample::next_boundary_after(sample::TICK_US),
            2 * sample::TICK_US
        );
    }

    #[test]
    fn a_tick_that_finishes_in_time_skips_nothing() {
        let start = 3 * sample::TICK_US;
        let done = start + sample::TICK_US - 1;
        let next = sample::next_boundary_after(done);
        assert_eq!(next, 4 * sample::TICK_US);
        assert_eq!(sample::ticks_skipped(start, next), 0);
    }

    #[test]
    fn a_tick_that_overruns_skips_the_boundaries_it_missed() {
        let start = 3 * sample::TICK_US;
        // Done a little after the boundary at 4 and at 5: the tick after it starts at 6, so 4 and
        // 5 are skipped.
        let done = 5 * sample::TICK_US + 1;
        let next = sample::next_boundary_after(done);
        assert_eq!(next, 6 * sample::TICK_US);
        assert_eq!(sample::ticks_skipped(start, next), 2);
    }

    #[test]
    fn a_clock_at_the_end_of_time_does_not_overflow() {
        assert_eq!(sample::next_boundary_after(u64::MAX), u64::MAX);
        assert_eq!(sample::ticks_skipped(u64::MAX, u64::MAX), 0);
    }
}
