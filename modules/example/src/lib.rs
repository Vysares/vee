//! `Example`: a module that exists to be read. It is the sample for writing a Vee module, and it
//! grows as Vee does: each thing a module can do (commands, tables, the bus, files) gets a small
//! working use here, with a comment on what it shows.
//!
//! What a module is:
//! - A crate of its own, which depends only on `vee-core-api`.
//! - A struct that holds the module's own state.
//! - An enum of the events it can send, with `#[derive(event::Code)]`.
//! - An `impl module::Module` for the struct: `init` runs once at startup, `step` runs at the
//!   module's rate. Each is given a `Ctx`, which is how the module reaches anything outside
//!   itself.
//! - The bus topics it writes, as a struct of writers (`ExampleOut`) that `Module::Out` names.
//!   The type of what it publishes (`Progress`) is declared here, in this crate: a module that
//!   reads it depends on this crate for the type.
//!
//! The deployment decides the rest: the module's source id, its rate, and where it sits in the
//! order. The module never knows them.
#![forbid(unsafe_code)]
#![no_std]

use vee_core_api::bus;
use vee_core_api::ctx;
use vee_core_api::event;
use vee_core_api::module;

/// What `Example` publishes on the bus, as a state topic: how far it has got. A state topic holds
/// only the latest value, so a reader that looks less often than the module steps misses some
/// (`bus::Sample::seq` shows how many).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// How many times `step` has run since startup.
    pub steps: u32,
}

/// The topics `Example` writes: what `Module::Out` names. The executive makes one for each call,
/// from the topic it owns (`StateTopic::writer`), and the module can only publish through it.
pub struct ExampleOut<'a> {
    pub progress: bus::StateWriter<'a, Progress>,
}

/// The events `Example` can send.
///
/// `#[derive(event::Code)]` writes `impl event::Code for ExampleEv`, which gives each variant its
/// code and severity. The code is the variant's value (0, 1, 2, ... in the order written, so new
/// events go at the end), and `#[repr(u16)]` is what makes that value a `u16`. Every variant
/// needs a `#[severity(..)]`: `Info`, `Warn` or `Err`.
///
/// The text of an event is not here. It is written where the event is sent, with the details
/// that matter at that moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, event::Code)]
#[repr(u16)]
pub enum ExampleEv {
    /// `init` ran. The text says how the previous run ended.
    #[severity(Info)]
    Started,
    /// One `step` ran. The text has the step's number.
    #[severity(Info)]
    Stepped,
}

/// The module's state: whatever it must remember from one call to the next. A module keeps
/// all of its state in its own struct, and nothing in a global.
#[derive(Debug, Default)]
pub struct Example {
    /// How many times `step` has run since startup.
    steps: u32,
}

impl Example {
    pub fn new() -> Self {
        Self::default()
    }
}

impl module::Module for Example {
    type Event = ExampleEv;
    type In<'a> = ();
    type Out<'a> = ExampleOut<'a>;
    type Cmd = ();

    /// Runs once, before the first tick. A module reads `ctx.reset` here to learn how the last
    /// run ended, and does its setup.
    fn init(&mut self, ctx: &mut ctx::Ctx<'_, Self>) {
        let previous = ctx.reset.previous;
        ctx.ev.send(
            ExampleEv::Started,
            format_args!("started, previous end: {previous}"),
        );
    }

    /// Runs at the module's rate. It does one step of work and returns: the executive cannot
    /// interrupt a step, so a step never waits and never loops until something happens.
    fn step(&mut self, ctx: &mut ctx::Ctx<'_, Self>) {
        self.steps = self.steps.saturating_add(1);
        let steps = self.steps;
        let now = ctx.monotonic();
        ctx.ev
            .send(ExampleEv::Stepped, format_args!("step {steps} at {now} us"));
        // Publishing replaces the topic's value. Its time is this call's, `now`.
        ctx.out.progress.publish(Progress { steps });
    }
}

#[cfg(test)]
mod tests {
    use super as example;
    use vee_core_api::bus;
    use vee_core_api::cmd;
    use vee_core_api::ctx;
    use vee_core_api::ev;
    use vee_core_api::event;
    use vee_core_api::module::Module as _;
    use vee_core_api::reset;
    use vee_core_api::throttle;

    /// The id the test gives the module: the executive picks it in a real deployment.
    const SOURCE: u8 = 7;

    /// What the executive does for one call into a module, on its own pieces.
    struct Harness {
        ev: ev::Ev,
        throttle: throttle::Throttle,
        reset: reset::ResetInfo,
        /// The topic the module publishes to. The harness owns it, as the executive does.
        progress: bus::StateTopic<example::Progress>,
        example: example::Example,
    }

    impl Harness {
        fn new(reset: reset::ResetInfo) -> Self {
            Harness {
                ev: ev::Ev::new(),
                throttle: throttle::Throttle::new(),
                reset,
                progress: bus::StateTopic::new(),
                example: example::Example::new(),
            }
        }

        fn init(&mut self, time_us: u64) {
            let mut ctx = ctx::Ctx::<example::Example>::new(
                &mut self.ev,
                &mut self.throttle,
                SOURCE,
                time_us,
                self.reset,
                (),
                example::ExampleOut {
                    progress: self.progress.writer(time_us),
                },
                cmd::Inbox::empty(),
            );
            self.example.init(&mut ctx);
        }

        fn step(&mut self, time_us: u64) {
            let mut ctx = ctx::Ctx::<example::Example>::new(
                &mut self.ev,
                &mut self.throttle,
                SOURCE,
                time_us,
                self.reset,
                (),
                example::ExampleOut {
                    progress: self.progress.writer(time_us),
                },
                cmd::Inbox::empty(),
            );
            self.example.step(&mut ctx);
        }
    }

    #[test]
    fn init_says_how_the_previous_run_ended() {
        let mut harness = Harness::new(reset::ResetInfo {
            count: 2,
            previous: reset::PreviousEnd::Clean(reset::ShutdownReason::Commanded),
            consecutive_unclean: 0,
        });
        harness.init(100);

        let event = harness.ev.log.pop().unwrap();
        assert_eq!(event.source, SOURCE);
        assert_eq!(event.monotonic_us, 100);
        assert_eq!(event.severity, event::Severity::Info);
        assert_eq!(event.code, event::Code::code(&example::ExampleEv::Started));
        assert_eq!(event.text.as_bytes(), b"started, previous end: commanded");
        assert!(harness.ev.log.is_empty());
    }

    #[test]
    fn each_step_sends_one_event_with_its_number_and_the_time() {
        let mut harness = Harness::new(reset::ResetInfo::first_boot());
        harness.step(1_000);
        harness.step(2_000);

        let first = harness.ev.log.pop().unwrap();
        assert_eq!(first.text.as_bytes(), b"step 1 at 1000 us");
        let second = harness.ev.log.pop().unwrap();
        assert_eq!(second.text.as_bytes(), b"step 2 at 2000 us");
        assert_eq!(second.monotonic_us, 2_000);
        assert_eq!(second.code, event::Code::code(&example::ExampleEv::Stepped));
        assert!(harness.ev.log.is_empty());
    }

    #[test]
    fn nothing_is_published_until_the_first_step() {
        let mut harness = Harness::new(reset::ResetInfo::first_boot());
        harness.init(100);
        assert_eq!(harness.progress.reader().latest(), None);
    }

    #[test]
    fn each_step_publishes_its_number_at_the_time_of_the_call() {
        let mut harness = Harness::new(reset::ResetInfo::first_boot());
        harness.step(1_000);
        harness.step(2_000);

        let sample = harness.progress.reader().latest().copied().unwrap();
        assert_eq!(sample.value, example::Progress { steps: 2 });
        assert_eq!(sample.seq, 2);
        assert_eq!(sample.updated_at, 2_000);
    }
}
