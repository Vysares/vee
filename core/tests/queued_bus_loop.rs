//! A queued topic in the loop: a hand written executive that owns one queued topic (one writer,
//! two readers) and three modules: a `Producer` that publishes to it, and two `Reader`s (the SAME
//! module type, reused, since their behaviour is identical) with different per-reader policies.
//! It shows what `exec!` will have to generate for a queued topic, and checks what each reader
//! sees.
//!
//! What it shows, beyond what `bus_loop.rs` already shows for state topics:
//! - Fan-out: every reader gets its OWN queue and its own copy of every message (`Producer`'s one
//!   `publish` call reaches both readers' queues).
//! - A queue is a real FIFO, unlike a state topic's "latest only": a reader that falls behind
//!   builds a backlog, up to its queue's depth, and sees the oldest messages first.
//! - The per reader `Policy` (chosen when the topic is made, in `Exec::new`) decides what a full
//!   queue does to the WRITER: `Refuse` makes `publish` report `Err(Full)` (that value is lost for
//!   that reader only); `Drop` lets `publish` succeed and drops the value for that reader instead,
//!   counted on the queue itself. One full queue never stops delivery to the others.
//!
//! The readers report what they saw as events, the same way `bus_loop.rs`'s controller does.

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

/// The time between the start of one tick and the start of the next. Tick `n` starts at
/// `(n + 1) * TICK_US`.
const TICK_US: u64 = 10_000;
/// How much the fake clock moves after each module's call, so each call sees its own time.
const MODULE_COST_US: u64 = 100;

const PRODUCER: u8 = 1;
const READER_A: u8 = 2;
const READER_B: u8 = 3;

/// The topic's reader count and queue depth. Reader 0 is `READER_A`'s, reader 1 is `READER_B`'s.
const READER_COUNT: usize = 2;
const QUEUE_DEPTH: usize = 2;

/// The producer publishes this many times the number of its steps (so the first value is 10).
const VALUE_PER_STEP: u32 = 10;

// ---- Module 1: the writer ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, event::Code)]
#[repr(u16)]
enum ProducerEv {
    #[severity(Info)]
    Published,
    #[severity(Warn)]
    Refused,
}

/// What the producer writes: one queued topic of `u32`.
struct ProducerOut<'a> {
    feed: bus::QueueWriter<'a, u32, READER_COUNT, QUEUE_DEPTH>,
}

struct Producer {
    steps: u32,
}

impl module::Module for Producer {
    type Event = ProducerEv;
    type In<'a> = ();
    type Out<'a> = ProducerOut<'a>;
    type Cmd = ();

    fn init(&mut self, _ctx: &mut ctx::Ctx<'_, Self>) {}

    fn step(&mut self, ctx: &mut ctx::Ctx<'_, Self>) {
        self.steps = self.steps.saturating_add(1);
        let value = self.steps.saturating_mul(VALUE_PER_STEP);
        match ctx.out.feed.publish(value) {
            Ok(()) => ctx
                .ev
                .send(ProducerEv::Published, format_args!("published {value}")),
            Err(bus::Full) => ctx
                .ev
                .send(ProducerEv::Refused, format_args!("refused {value}")),
        }
    }
}

// ---- Module 2: a reader. READER_A and READER_B are both this SAME type: they differ only in
// which of the topic's queues the executive points their handle at, and in that queue's policy,
// not in behaviour, so one implementation does for both. ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, event::Code)]
#[repr(u16)]
enum ReaderEv {
    #[severity(Info)]
    Saw,
}

/// What a reader reads: its own queue on the topic.
struct ReaderIn<'a> {
    feed: bus::QueueReader<'a, u32, READER_COUNT, QUEUE_DEPTH>,
}

struct Reader;

impl module::Module for Reader {
    type Event = ReaderEv;
    type In<'a> = ReaderIn<'a>;
    type Out<'a> = ();
    type Cmd = ();

    fn init(&mut self, _ctx: &mut ctx::Ctx<'_, Self>) {}

    fn step(&mut self, ctx: &mut ctx::Ctx<'_, Self>) {
        let mut count = 0_u32;
        let mut last = None;
        while let Some(value) = ctx.inputs.feed.pop() {
            count = count.saturating_add(1);
            last = Some(value);
        }
        match last {
            Some(value) => ctx
                .ev
                .send(ReaderEv::Saw, format_args!("{count} waiting, last {value}")),
            None => ctx.ev.send(ReaderEv::Saw, format_args!("nothing waiting")),
        }
    }
}

// ---- The executive ----

/// What `exec!` will generate: the topic, a slot for each module, and the calls in order.
struct Exec {
    clock: fake_clock::FakeClock,
    core: vee_core::Core<fake_platform::FakePlatform>,
    tick: u64,
    /// The queued topic. The executive owns it, and lends it to the modules one call at a time.
    feed: bus::QueuedTopic<u32, READER_COUNT, QUEUE_DEPTH>,
    producer: Slot<Producer>,
    reader_a: Slot<Reader>,
    reader_b: Slot<Reader>,
}

impl Exec {
    /// The producer steps every tick. `reader_a_period`/`reader_b_period` are each reader's own
    /// rate. Reader A's queue (index 0) refuses when full; reader B's (index 1) drops.
    fn new(reader_a_period: u64, reader_b_period: u64) -> Self {
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
            feed: bus::QueuedTopic::new([bus::Policy::Refuse, bus::Policy::Drop]),
            producer: Slot::new(Producer { steps: 0 }, PRODUCER, 1),
            reader_a: Slot::new(Reader, READER_A, reader_a_period),
            reader_b: Slot::new(Reader, READER_B, reader_b_period),
        }
    }

    /// One call into the producer, with a writer handle for the topic made for this call.
    fn step_producer(&mut self) {
        let reset = self.core.reset();
        let now = self.clock.monotonic_us();
        let out = ProducerOut {
            feed: self.feed.writer(),
        };
        self.producer.step(
            self.tick,
            &mut self.core.ev,
            now,
            reset,
            (),
            out,
            cmd::Inbox::empty(),
        );
        self.clock.advance(MODULE_COST_US);
    }

    /// One call into reader A, with a reader handle for queue 0 made for this call.
    fn step_reader_a(&mut self) {
        let reset = self.core.reset();
        let now = self.clock.monotonic_us();
        let inputs = ReaderIn {
            feed: self.feed.reader(0),
        };
        self.reader_a.step(
            self.tick,
            &mut self.core.ev,
            now,
            reset,
            inputs,
            (),
            cmd::Inbox::empty(),
        );
        self.clock.advance(MODULE_COST_US);
    }

    /// One call into reader B, with a reader handle for queue 1 made for this call.
    fn step_reader_b(&mut self) {
        let reset = self.core.reset();
        let now = self.clock.monotonic_us();
        let inputs = ReaderIn {
            feed: self.feed.reader(1),
        };
        self.reader_b.step(
            self.tick,
            &mut self.core.ev,
            now,
            reset,
            inputs,
            (),
            cmd::Inbox::empty(),
        );
        self.clock.advance(MODULE_COST_US);
    }

    /// One tick: each module's report of what it had suppressed (which also starts its new tick
    /// of throttle counting: without this, a code sent every tick hits `EV_MAX_PER_TICK` after a
    /// few ticks, not per tick), then the core's tick, then the producer and both readers.
    fn run_tick(&mut self) {
        let now = self.tick.saturating_add(1).saturating_mul(TICK_US);
        self.clock.set(now);
        self.producer.report_suppressed(&mut self.core.ev, now);
        self.reader_a.report_suppressed(&mut self.core.ev, now);
        self.reader_b.report_suppressed(&mut self.core.ev, now);
        self.core.tick(now);
        self.step_producer();
        self.step_reader_a();
        self.step_reader_b();
        self.tick = self.tick.saturating_add(1);
    }

    /// What one source reported, oldest first, after one more core tick has written the last
    /// tick's events to the event log.
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

/// An executive that has run `ticks` ticks.
fn run(reader_a_period: u64, reader_b_period: u64, ticks: u64) -> Exec {
    let mut exec = Exec::new(reader_a_period, reader_b_period);
    for _ in 0..ticks {
        exec.run_tick();
    }
    exec
}

// ---- Tests ----

#[test]
fn each_reader_gets_its_own_copy_in_order() {
    let mut exec = run(1, 1, 3);
    let expected = [
        "1 waiting, last 10",
        "1 waiting, last 20",
        "1 waiting, last 30",
    ];
    assert_eq!(exec.reported(READER_A), expected);
    assert_eq!(exec.reported(READER_B), expected);
}

#[test]
fn a_refuse_reader_that_falls_behind_builds_a_backlog_and_blocks_the_writer_when_full() {
    // Reader B (period 1, Drop) always drains, so it never interferes: this isolates reader A.
    let mut exec = run(3, 1, 6);
    assert_eq!(
        exec.reported(PRODUCER),
        [
            "published 10",
            "published 20",
            "published 30",
            // A's queue (depth 2) is full with [20, 30] by here: 40 is refused and lost for A.
            "refused 40",
            "published 50",
            "published 60",
        ]
    );
    assert_eq!(
        exec.reported(READER_A),
        ["1 waiting, last 10", "2 waiting, last 30"]
    );
}

#[test]
fn a_drop_reader_that_falls_behind_loses_data_silently_without_blocking_the_writer() {
    // Reader A (period 1, Refuse) always drains, so it never interferes: this isolates reader B.
    let mut exec = run(1, 3, 6);
    assert_eq!(
        exec.reported(PRODUCER),
        [
            "published 10",
            "published 20",
            "published 30",
            // B's queue (depth 2) is full with [20, 30] by here: 40 is dropped for B, but, unlike
            // reader A's Refuse policy above, the writer is never told.
            "published 40",
            "published 50",
            "published 60",
        ]
    );
    assert_eq!(
        exec.reported(READER_B),
        ["1 waiting, last 10", "2 waiting, last 30"]
    );
    // The drop was still counted, directly on the topic the executive owns: no module needed.
    assert_eq!(exec.feed.take_refused(1), 1);
}
