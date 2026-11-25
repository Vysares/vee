//! `Watcher`: the sample module that READS a bus topic. It looks at `Example`'s `Progress` topic
//! and sends an event each time it finds a value it has not seen before.
//!
//! What it shows:
//! - A module's inputs are a struct of readers (`WatcherIn`) that `Module::In` names. The
//!   executive makes it for each call, from the topic it owns, and the module gets it as
//!   `ctx.inputs`. The type of the reader (`StateReader<Progress>`) is the topic's type, so the
//!   compiler will not let the executive connect it to a topic of another type.
//! - The type `Progress` comes from the crate of the module that writes it (`vee-example`).
//! - A state topic holds only the latest value and the reader is not told when it changes. Each
//!   reader keeps the `seq` of the last sample it saw, in its own state, and a different `seq`
//!   means a new value. The difference also says how many values were published without this
//!   module looking: it is the topic's lossy behaviour, in numbers.
//! - Whether a module sees a value from this tick or the last depends on the order the executive
//!   calls them in (writer first, and it sees this tick's).
#![forbid(unsafe_code)]
#![no_std]

use vee_core_api::bus;
use vee_core_api::ctx;
use vee_core_api::event;
use vee_core_api::module;
use vee_example as example;

/// The events `Watcher` can send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, event::Code)]
#[repr(u16)]
pub enum WatcherEv {
    /// A new value was found. The text has the step, the `seq`, how many publishes were missed
    /// since the last look, and how old the value is.
    #[severity(Info)]
    Saw,
}

/// The topics `Watcher` reads: what `Module::In` names.
pub struct WatcherIn<'a> {
    pub progress: bus::StateReader<'a, example::Progress>,
}

/// The module's state.
#[derive(Debug, Default)]
pub struct Watcher {
    /// The `seq` of the last sample it reported, or `None` before the first. (`None`, and not a
    /// number such as 0 for "nothing yet", because `seq` wraps round and can be 0 again.)
    last_seen: Option<u32>,
}

impl Watcher {
    pub fn new() -> Self {
        Self::default()
    }
}

impl module::Module for Watcher {
    type Event = WatcherEv;
    type In<'a> = WatcherIn<'a>;
    type Out<'a> = ();
    type Cmd = ();

    fn init(&mut self, _ctx: &mut ctx::Ctx<'_, Self>) {}

    /// Looks at the topic. Nothing published yet, or nothing new since the last look, sends
    /// nothing.
    fn step(&mut self, ctx: &mut ctx::Ctx<'_, Self>) {
        let Some(sample) = ctx.inputs.progress.latest() else {
            return;
        };
        if self.last_seen == Some(sample.seq) {
            return;
        }
        // Each publish raises `seq` by one, so the ones in between were published and replaced
        // before this module looked. The first look has none to count.
        let missed = sample
            .seq
            .wrapping_sub(self.last_seen.unwrap_or(0))
            .saturating_sub(1);
        self.last_seen = Some(sample.seq);

        let age = ctx.monotonic().saturating_sub(sample.updated_at);
        ctx.ev.send(
            WatcherEv::Saw,
            format_args!(
                "example at step {}, seq {}, {missed} missed, {age} us old",
                sample.value.steps, sample.seq
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super as watcher;
    use vee_core_api::bus;
    use vee_core_api::cmd;
    use vee_core_api::ctx;
    use vee_core_api::ev;
    use vee_core_api::event;
    use vee_core_api::module::Module as _;
    use vee_core_api::reset;
    use vee_core_api::throttle;
    use vee_example as example;

    const SOURCE: u8 = 9;

    /// What the executive does for one call into the module, on its own pieces. It owns the topic
    /// and plays the writer, so a test can publish whatever it likes.
    struct Harness {
        ev: ev::Ev,
        throttle: throttle::Throttle,
        progress: bus::StateTopic<example::Progress>,
        watcher: watcher::Watcher,
    }

    impl Harness {
        fn new() -> Self {
            Harness {
                ev: ev::Ev::new(),
                throttle: throttle::Throttle::new(),
                progress: bus::StateTopic::new(),
                watcher: watcher::Watcher::new(),
            }
        }

        /// The writer publishes `steps` at `time_us`.
        fn publish(&mut self, time_us: u64, steps: u32) {
            self.progress
                .writer(time_us)
                .publish(example::Progress { steps });
        }

        fn step(&mut self, time_us: u64) {
            let mut ctx = ctx::Ctx::<watcher::Watcher>::new(
                &mut self.ev,
                &mut self.throttle,
                SOURCE,
                time_us,
                reset::ResetInfo::first_boot(),
                watcher::WatcherIn {
                    progress: self.progress.reader(),
                },
                (),
                cmd::Inbox::empty(),
            );
            self.watcher.step(&mut ctx);
        }

        /// The text of the next event, if there is one.
        fn next_text(&mut self) -> Option<event::Text> {
            self.ev.log.pop().map(|event| event.text)
        }
    }

    #[test]
    fn nothing_published_sends_nothing() {
        let mut harness = Harness::new();
        harness.step(100);
        assert!(harness.next_text().is_none());
    }

    #[test]
    fn a_new_value_is_reported_with_its_age() {
        let mut harness = Harness::new();
        harness.publish(1_000, 4);
        harness.step(1_250);
        assert_eq!(
            harness.next_text().unwrap().as_bytes(),
            "example at step 4, seq 1, 0 missed, 250 us old".as_bytes()
        );
    }

    #[test]
    fn the_same_value_is_reported_once() {
        let mut harness = Harness::new();
        harness.publish(1_000, 1);
        harness.step(1_000);
        harness.step(2_000);
        harness.step(3_000);
        assert!(harness.next_text().is_some());
        assert!(harness.next_text().is_none());
    }

    #[test]
    fn publishes_between_looks_are_counted_as_missed() {
        let mut harness = Harness::new();
        harness.publish(1_000, 1);
        harness.step(1_000);
        // Three more publishes before the next look: only the last is there.
        harness.publish(2_000, 2);
        harness.publish(3_000, 3);
        harness.publish(4_000, 4);
        harness.step(4_000);

        assert_eq!(
            harness.next_text().unwrap().as_bytes(),
            "example at step 1, seq 1, 0 missed, 0 us old".as_bytes()
        );
        assert_eq!(
            harness.next_text().unwrap().as_bytes(),
            "example at step 4, seq 4, 2 missed, 0 us old".as_bytes()
        );
    }

    #[test]
    fn a_first_look_after_several_publishes_counts_the_ones_before_it() {
        let mut harness = Harness::new();
        harness.publish(1_000, 1);
        harness.publish(2_000, 2);
        harness.publish(3_000, 3);
        harness.step(3_000);
        assert_eq!(
            harness.next_text().unwrap().as_bytes(),
            "example at step 3, seq 3, 2 missed, 0 us old".as_bytes()
        );
    }
}
