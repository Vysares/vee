//! The bus in the loop: a hand written executive that owns one state topic and two modules, a
//! `Sensor` that publishes to it and a `Controller` that reads it. It shows what `exec!` will
//! have to generate for the bus, and checks what a reader sees.
//!
//! What it shows about `exec!`:
//! - The executive OWNS the topic (`Exec::reading`). A module never holds it.
//! - For each call into a module the executive makes the handles that module declared (`Out` for
//!   a writer, `In` for a reader) from the topic, and passes them to `Slot::step`. Making them is
//!   a struct literal, `SensorOut { reading: self.reading.writer(now) }`. Because the types are
//!   the topic's `u32` and the handles' `u32`, giving a module a topic of the wrong type does not
//!   compile.
//! - The order the modules are called in is the data flow. A reader that runs after its writer
//!   sees this tick's value, and one that runs before it sees the last tick's (`Order`).
//! - The executive reads the clock ONCE for each call, just before it, and uses that `now` for
//!   the writer's handle (`writer(now)`) and passes it to `Slot::step`, which gives it to the
//!   module's `Ctx`. So a sample's `updated_at` is exactly the time the writer's
//!   `ctx.monotonic()` shows.
//!
//! The controller reports what it sees as events, and the tests read them from the event log, the
//! same way `exec_loop.rs` does.

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

const SENSOR: u8 = 1;
const CONTROLLER: u8 = 2;

/// The sensor publishes this many times the number of its steps (so the first value is 10).
const VALUE_PER_STEP: u32 = 10;

// ---- Module 1: the writer ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, event::Code)]
#[repr(u16)]
enum SensorEv {
    #[severity(Info)]
    Published,
}

/// What the sensor writes: one state topic of `u32`.
struct SensorOut<'a> {
    reading: bus::StateWriter<'a, u32>,
}

struct Sensor {
    steps: u32,
}

impl module::Module for Sensor {
    type Event = SensorEv;
    type In<'a> = ();
    type Out<'a> = SensorOut<'a>;
    type Cmd = ();

    fn init(&mut self, _ctx: &mut ctx::Ctx<'_, Self>) {}

    fn step(&mut self, ctx: &mut ctx::Ctx<'_, Self>) {
        self.steps = self.steps.saturating_add(1);
        let value = self.steps.saturating_mul(VALUE_PER_STEP);
        ctx.out.reading.publish(value);
        ctx.ev
            .send(SensorEv::Published, format_args!("published {value}"));
    }
}

// ---- Module 2: the reader ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, event::Code)]
#[repr(u16)]
enum ControllerEv {
    #[severity(Info)]
    Saw,
}

/// What the controller reads: the same topic, as a reader.
struct ControllerIn<'a> {
    reading: bus::StateReader<'a, u32>,
}

struct Controller;

impl module::Module for Controller {
    type Event = ControllerEv;
    type In<'a> = ControllerIn<'a>;
    type Out<'a> = ();
    type Cmd = ();

    fn init(&mut self, _ctx: &mut ctx::Ctx<'_, Self>) {}

    fn step(&mut self, ctx: &mut ctx::Ctx<'_, Self>) {
        match ctx.inputs.reading.latest() {
            Some(sample) => ctx.ev.send(
                ControllerEv::Saw,
                format_args!(
                    "value {} seq {} updated {}",
                    sample.value, sample.seq, sample.updated_at
                ),
            ),
            None => ctx.ev.send(ControllerEv::Saw, format_args!("no reading")),
        }
    }
}

// ---- The executive ----

/// Which module is called first in a tick.
#[derive(Clone, Copy)]
enum Order {
    SensorFirst,
    ControllerFirst,
}

/// What `exec!` will generate: the topic, a slot for each module, and the calls in order.
struct Exec {
    clock: fake_clock::FakeClock,
    core: vee_core::Core<fake_platform::FakePlatform>,
    tick: u64,
    order: Order,
    /// The state topic. The executive owns it, and lends it to the modules one call at a time.
    reading: bus::StateTopic<u32>,
    sensor: Slot<Sensor>,
    controller: Slot<Controller>,
}

impl Exec {
    /// The sensor steps every `sensor_period` ticks, and the controller every `controller_period`.
    fn new(order: Order, sensor_period: u64, controller_period: u64) -> Self {
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
            order,
            reading: bus::StateTopic::new(),
            sensor: Slot::new(Sensor { steps: 0 }, SENSOR, sensor_period),
            controller: Slot::new(Controller, CONTROLLER, controller_period),
        }
    }

    /// One call into the sensor, with a writer handle for the topic made for this call.
    fn step_sensor(&mut self) {
        let reset = self.core.reset();
        let now = self.clock.monotonic_us();
        let out = SensorOut {
            reading: self.reading.writer(now),
        };
        self.sensor.step(
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

    /// One call into the controller, with a reader handle for the topic made for this call.
    fn step_controller(&mut self) {
        let reset = self.core.reset();
        let now = self.clock.monotonic_us();
        let inputs = ControllerIn {
            reading: self.reading.reader(),
        };
        self.controller.step(
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

    /// One tick: the core's tick, then the two modules in this executive's order.
    fn run_tick(&mut self) {
        let now = self.tick.saturating_add(1).saturating_mul(TICK_US);
        self.clock.set(now);
        self.core.tick(now);
        match self.order {
            Order::SensorFirst => {
                self.step_sensor();
                self.step_controller();
            }
            Order::ControllerFirst => {
                self.step_controller();
                self.step_sensor();
            }
        }
        self.tick = self.tick.saturating_add(1);
    }

    /// What the controller reported, oldest first, after one more core tick has written the last
    /// tick's events to the event log.
    fn controller_saw(&mut self) -> Vec<String> {
        let now = self.tick.saturating_add(1).saturating_mul(TICK_US);
        self.core.tick(now);
        self.core.flush_event_log(now);
        let bytes = self.core.files().get(EVENT_LOG).unwrap_or_default();
        bytes
            .chunks(vee_core_types::event_log::RECORD_LEN)
            .map(|record| vee_core_types::event_log::decode(record).unwrap())
            .filter(|event| event.source == CONTROLLER)
            .map(|event| {
                std::str::from_utf8(event.text.as_bytes())
                    .unwrap()
                    .to_owned()
            })
            .collect()
    }
}

/// An executive that has run `ticks` ticks.
fn run(order: Order, sensor_period: u64, controller_period: u64, ticks: u64) -> Exec {
    let mut exec = Exec::new(order, sensor_period, controller_period);
    for _ in 0..ticks {
        exec.run_tick();
    }
    exec
}

// ---- Tests ----

#[test]
fn a_reader_after_its_writer_sees_this_ticks_value() {
    let saw = run(Order::SensorFirst, 1, 1, 3).controller_saw();
    // Tick n starts at (n + 1) * 10_000 and the sensor is called first, so it publishes at that
    // time. The controller is called 100 us later and already sees it.
    assert_eq!(
        saw,
        [
            "value 10 seq 1 updated 10000",
            "value 20 seq 2 updated 20000",
            "value 30 seq 3 updated 30000",
        ]
    );
}

#[test]
fn a_reader_before_its_writer_sees_the_last_ticks_value() {
    let saw = run(Order::ControllerFirst, 1, 1, 3).controller_saw();
    // The controller is called first, so in tick 0 nothing was published yet. The sensor is
    // called 100 us after the start of each tick, and the controller sees that a tick later.
    assert_eq!(
        saw,
        [
            "no reading",
            "value 10 seq 1 updated 10100",
            "value 20 seq 2 updated 20100",
        ]
    );
}

#[test]
fn a_slow_reader_sees_only_the_latest_and_seq_shows_what_it_missed() {
    // The sensor publishes every tick, and the controller looks every 4th tick (0 and 4).
    let saw = run(Order::SensorFirst, 1, 4, 5).controller_saw();
    assert_eq!(
        saw,
        [
            "value 10 seq 1 updated 10000",
            // Four publishes since the last look (values 20, 30, 40, 50): only the last is
            // there, and seq went from 1 to 5.
            "value 50 seq 5 updated 50000",
        ]
    );
}

#[test]
fn a_fast_reader_sees_the_same_sample_until_the_writer_publishes_again() {
    // The sensor publishes every 4th tick (0 and 4), and the controller looks every tick.
    let saw = run(Order::SensorFirst, 4, 1, 5).controller_saw();
    // Reading does not use the value up: ticks 1 to 3 see the same sample, with the same seq.
    assert_eq!(
        saw,
        [
            "value 10 seq 1 updated 10000",
            "value 10 seq 1 updated 10000",
            "value 10 seq 1 updated 10000",
            "value 10 seq 1 updated 10000",
            "value 20 seq 2 updated 50000",
        ]
    );
}
