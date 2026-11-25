//! A module that writes BOTH kinds of topic in one call, and a flow controlled writer: the parts
//! of the bus that `bus_loop.rs` (state topics) and `queued_bus_loop.rs` (queued topics, with a
//! writer that does not look first) do not show.
//!
//! - One module's `Out` can hold a state writer AND a queue writer, and the two topics do not
//!   disturb each other: the state topic keeps only the latest value (a slow reader misses some),
//!   the queued topic keeps every value (a slow reader sees them all, in order).
//! - A writer that checks `space()` before it publishes is NEVER refused and loses NOTHING, even
//!   with a reader that is much slower than it is: it holds the value until there is room. (The
//!   writer in `queued_bus_loop.rs` publishes without looking, so a value is lost.)

#![allow(
    clippy::unwrap_used,
    reason = "a test file: a failed unwrap is a failed test (the helpers are not `#[test]` fns, \
              so clippy.toml's exemption does not cover them)"
)]

use vee_core::slot::Slot;
use vee_core_api::bus;
use vee_core_api::cmd;
use vee_core_api::ctx;
use vee_core_api::event;
use vee_core_api::module;
use vee_osal::clock::Clock as _;
use vee_osal::fake_clock;
use vee_osal::fake_files;
use vee_osal::fake_platform;
use vee_osal::fake_watchdog;

const EVENT_LOG: &str = "events.log";
const RESET_LOG: &str = "reset.log";
const TICK_US: u64 = 10_000;
const MODULE_COST_US: u64 = 100;

const WRITER: u8 = 1;
const READER: u8 = 2;

/// The queued topic's depth, small so a slow reader fills it.
const DEPTH: usize = 2;

// ---- The writer ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, event::Code)]
#[repr(u16)]
enum WriterEv {
    /// The queue had no room, so the writer kept the value for a later step.
    #[severity(Info)]
    Held,
    /// `publish` said `Full`. A writer that checks `space()` must never see this.
    #[severity(Err)]
    Refused,
}

struct WriterOut<'a> {
    /// The latest value of how many times the writer has stepped.
    steps: bus::StateWriter<'a, u32>,
    /// The values 1, 2, 3, ... in order, each sent once.
    feed: bus::QueueWriter<'a, u32, 1, DEPTH>,
}

struct Writer {
    steps: u32,
    /// The next value for `feed`: it moves on only when a value has gone.
    next: u32,
}

impl module::Module for Writer {
    type Event = WriterEv;
    type In<'a> = ();
    type Out<'a> = WriterOut<'a>;
    type Cmd = ();

    fn init(&mut self, _ctx: &mut ctx::Ctx<'_, Self>) {}

    fn step(&mut self, ctx: &mut ctx::Ctx<'_, Self>) {
        self.steps = self.steps.saturating_add(1);
        ctx.out.steps.publish(self.steps);

        if ctx.out.feed.space() == 0 {
            let next = self.next;
            ctx.ev.send(WriterEv::Held, format_args!("held {next}"));
            return;
        }
        match ctx.out.feed.publish(self.next) {
            Ok(()) => self.next = self.next.saturating_add(1),
            Err(bus::Full) => ctx.ev.send(WriterEv::Refused, format_args!("refused")),
        }
    }
}

// ---- The reader ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, event::Code)]
#[repr(u16)]
enum ReaderEv {
    #[severity(Info)]
    State,
    #[severity(Info)]
    Fed,
}

struct ReaderIn<'a> {
    steps: bus::StateReader<'a, u32>,
    feed: bus::QueueReader<'a, u32, 1, DEPTH>,
}

struct Reader;

impl module::Module for Reader {
    type Event = ReaderEv;
    type In<'a> = ReaderIn<'a>;
    type Out<'a> = ();
    type Cmd = ();

    fn init(&mut self, _ctx: &mut ctx::Ctx<'_, Self>) {}

    fn step(&mut self, ctx: &mut ctx::Ctx<'_, Self>) {
        match ctx.inputs.steps.latest() {
            Some(sample) => ctx.ev.send(
                ReaderEv::State,
                format_args!("state {} seq {}", sample.value, sample.seq),
            ),
            None => ctx.ev.send(ReaderEv::State, format_args!("state none")),
        }
        while let Some(value) = ctx.inputs.feed.pop() {
            ctx.ev.send(ReaderEv::Fed, format_args!("fed {value}"));
        }
    }
}

// ---- The executive ----

struct Exec {
    clock: fake_clock::FakeClock,
    core: vee_core::Core<fake_platform::FakePlatform>,
    tick: u64,
    steps: bus::StateTopic<u32>,
    feed: bus::QueuedTopic<u32, 1, DEPTH>,
    writer: Slot<Writer>,
    reader: Slot<Reader>,
}

impl Exec {
    fn new(reader_period: u64) -> Self {
        let clock = fake_clock::FakeClock::new();
        let core = vee_core::Core::<fake_platform::FakePlatform>::start(
            fake_files::FakeFiles::new(),
            fake_watchdog::FakeWatchdog::new(),
            RESET_LOG,
            EVENT_LOG,
            clock.monotonic_us(),
        );
        Exec {
            clock,
            core,
            tick: 0,
            steps: bus::StateTopic::new(),
            feed: bus::QueuedTopic::new([bus::Policy::Refuse]),
            writer: Slot::new(Writer { steps: 0, next: 1 }, WRITER, 1),
            reader: Slot::new(Reader, READER, reader_period),
        }
    }

    fn run_tick(&mut self) {
        let now = self.tick.saturating_add(1).saturating_mul(TICK_US);
        self.clock.set(now);
        self.writer.report_suppressed(&mut self.core.ev, now);
        self.reader.report_suppressed(&mut self.core.ev, now);
        self.core.tick(now);

        let reset = self.core.reset();
        let time = self.clock.monotonic_us();
        let out = WriterOut {
            steps: self.steps.writer(time),
            feed: self.feed.writer(),
        };
        self.writer.step(
            self.tick,
            &mut self.core.ev,
            time,
            reset,
            (),
            out,
            cmd::Inbox::empty(),
        );
        self.clock.advance(MODULE_COST_US);

        let time = self.clock.monotonic_us();
        let inputs = ReaderIn {
            steps: self.steps.reader(),
            feed: self.feed.reader(0),
        };
        self.reader.step(
            self.tick,
            &mut self.core.ev,
            time,
            reset,
            inputs,
            (),
            cmd::Inbox::empty(),
        );
        self.clock.advance(MODULE_COST_US);
        self.tick = self.tick.saturating_add(1);
    }

    /// What one source reported, oldest first.
    fn reported(&mut self, source: u8) -> Vec<String> {
        let now = self.tick.saturating_add(1).saturating_mul(TICK_US);
        self.core.tick(now);
        self.core.flush_event_log(now);
        let bytes = self.core.files().get(EVENT_LOG).unwrap_or_default();
        bytes
            .chunks(vee_core_types::event_log::RECORD_LEN)
            .map(|record| vee_core_types::event_log::decode(record).unwrap())
            .filter(|event| event.source == source)
            .map(|event| {
                std::str::from_utf8(event.text.as_bytes())
                    .unwrap()
                    .to_owned()
            })
            .collect()
    }
}

fn run(reader_period: u64, ticks: u64) -> Exec {
    let mut exec = Exec::new(reader_period);
    for _ in 0..ticks {
        exec.run_tick();
    }
    exec
}

// ---- Tests ----

#[test]
fn a_writer_that_checks_space_is_never_refused_and_a_slow_reader_loses_nothing() {
    // The reader steps every 3rd tick (0, 3, 6, 9), and the queue holds 2: the writer fills it in
    // two steps, holds for one, and carries on once the reader has emptied it.
    let mut exec = run(3, 12);

    let writer = exec.reported(WRITER);
    assert!(
        writer.iter().all(|text| text.starts_with("held")),
        "the writer was refused: {writer:?}"
    );
    assert!(
        !writer.is_empty(),
        "the reader was slow, so the writer had to hold"
    );

    // Every value the writer sent reached the reader, in order, with none missing.
    let fed: Vec<u32> = exec
        .reported(READER)
        .iter()
        .filter_map(|text| text.strip_prefix("fed "))
        .map(|value| value.parse().unwrap())
        .collect();
    assert!(fed.len() >= 6, "{fed:?}");
    let expected: Vec<u32> = (1..=u32::try_from(fed.len()).unwrap()).collect();
    assert_eq!(fed, expected);
}

#[test]
fn the_held_value_is_the_one_sent_next_not_skipped() {
    let mut exec = run(3, 5);
    // The reader steps at ticks 0 and 3, after the writer. Tick 0: the writer sends 1, the reader
    // takes it. Ticks 1 and 2: the writer sends 2 and 3, and the queue (depth 2) is full. Tick 3:
    // the writer has no room and holds 4; then the reader takes 2 and 3. Tick 4: 4 goes out.
    assert_eq!(exec.reported(WRITER), ["held 4"]);
    let fed: Vec<String> = exec
        .reported(READER)
        .into_iter()
        .filter(|text| text.starts_with("fed"))
        .collect();
    assert_eq!(fed, ["fed 1", "fed 2", "fed 3"]);
}

#[test]
fn one_modules_two_topics_do_not_disturb_each_other() {
    // The state topic keeps only the latest value, so a reader that steps every 3rd tick sees
    // 1, 4, 7, 10 (the writer's step count at that moment) with `seq` equal to the count. The
    // queued topic gives it every value. Both from the same two calls per tick.
    let mut exec = run(3, 10);
    let state: Vec<String> = exec
        .reported(READER)
        .into_iter()
        .filter(|text| text.starts_with("state"))
        .collect();
    assert_eq!(
        state,
        [
            "state 1 seq 1",
            "state 4 seq 4",
            "state 7 seq 7",
            "state 10 seq 10"
        ]
    );
}

#[test]
fn a_reader_that_steps_every_tick_keeps_the_queue_empty_so_the_writer_never_holds() {
    let mut exec = run(1, 10);
    assert!(exec.reported(WRITER).is_empty());
    let fed: Vec<String> = exec
        .reported(READER)
        .into_iter()
        .filter(|text| text.starts_with("fed"))
        .collect();
    assert_eq!(fed.len(), 10);
    assert_eq!(fed.first().unwrap(), "fed 1");
    assert_eq!(fed.last().unwrap(), "fed 10");
}
