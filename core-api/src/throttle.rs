//! Limits how many times one event can be sent in a tick, so a module that sends the same event
//! over and over cannot fill the event queue and the log.
//!
//! A tick is one cycle of the executive (one minor frame). Commands run inside ticks, so events
//! sent from a module's `command()` count too.
//!
//! There is one counter for each event a source can send, found by the event's `index`. Every
//! source's throttle is the same size, `EV_MAX_CODES` counters, and a source uses the first few
//! (its number of events). So there is no table to fill up and no lookup: sending the same event
//! 100 times, or 33 different events once each, can never run anything out. A source with more
//! events than `EV_MAX_CODES` is a compile error (`#[derive(event::Code)]` checks it).
//!
//! The counters are `u8`: a count that reaches 255 stays there. That is enough, because it only
//! has to say "far over the limit".
//!
//! Who calls what: the handle behind `ctx.ev` calls `throttle` before it sends an event, and the
//! executive makes it call `new_tick` at the start of each tick. The core's own events do not go
//! through a throttle, so the event that reports suppressed events cannot be suppressed itself.

use crate::config;
use crate::event;

config::setting! {
    /// How many times each event can be sent in one tick. Sends past it are suppressed, and counted.
    /// At most 255: the counters are `u8`.
    pub EV_MAX_PER_TICK: u8 = 5, 1..=255;
}

config::setting! {
    /// The most events one source (one module, or the core) can define. It is the number of
    /// counters in every throttle, a byte each.
    pub EV_MAX_CODES: usize = 256, 1..=65535;
}

/// The counters for one source's events.
pub struct Throttle {
    /// How many times each event was sent this tick, by `index`. Each stops at 255.
    sent: [u8; EV_MAX_CODES],
}

impl Throttle {
    pub const fn new() -> Self {
        Throttle {
            sent: [0; EV_MAX_CODES],
        }
    }

    /// Counts one send of `code`, and says whether to let it through:
    /// - `None`: it is within the limit. Send it.
    /// - `Some(over_by)`: it is past the limit. Do not send it. `over_by` is how many sends of
    ///   this event are past the limit so far this tick (1 for the first one suppressed). It stops
    ///   at 255 minus the limit.
    ///
    /// An event whose `index` is past the counters is let through and not counted: it is better
    /// to send an event than to lose it. (The derive makes this impossible for its enums.)
    pub fn throttle<C: event::Code>(&mut self, code: &C) -> Option<u8> {
        let count = self.sent.get_mut(code.index())?;
        *count = count.saturating_add(1);
        count
            .checked_sub(EV_MAX_PER_TICK)
            .filter(|over_by| *over_by > 0)
    }

    /// Starts a new tick. For each event of the enum `C` that was suppressed in the tick that just
    /// ended, calls `report` with the event and how many sends were suppressed, then starts every
    /// counter again from 0. The caller turns each report into an event for the ground.
    pub fn new_tick<C: event::Code, F: FnMut(C, u8)>(&mut self, mut report: F) {
        for (index, count) in self.sent.iter_mut().enumerate() {
            let suppressed = count.saturating_sub(EV_MAX_PER_TICK);
            if suppressed > 0
                && let Some(code) = C::from_index(index)
            {
                report(code, suppressed);
            }
            *count = 0;
        }
    }
}

impl Default for Throttle {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super as throttle;
    use crate::event;

    /// An enum of codes written out by hand, as the derive would write it. (The derive's own
    /// output is tested in `tests/code_derive.rs`: inside this crate its `::vee_core_api` paths
    /// do not resolve.)
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Sample {
        A,
        B,
        C,
    }

    impl event::Code for Sample {
        fn code(&self) -> u16 {
            *self as u16
        }
        fn severity(&self) -> event::Severity {
            event::Severity::Info
        }
        fn index(&self) -> usize {
            *self as usize
        }
        fn from_index(index: usize) -> Option<Self> {
            [Sample::A, Sample::B, Sample::C].get(index).copied()
        }
    }

    /// A code whose index is past the end of the counters, as a mistake would make it.
    struct Wild;

    impl event::Code for Wild {
        fn code(&self) -> u16 {
            0
        }
        fn severity(&self) -> event::Severity {
            event::Severity::Info
        }
        fn index(&self) -> usize {
            throttle::EV_MAX_CODES
        }
        fn from_index(_index: usize) -> Option<Self> {
            None
        }
    }

    const LIMIT: u8 = throttle::EV_MAX_PER_TICK;

    /// Everything `new_tick` reports, in order, and how many reports there were.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "test helper: at most 3 reports"
    )]
    fn ended(throttle: &mut throttle::Throttle) -> ([Option<(Sample, u8)>; 3], usize) {
        let mut reports = [None; 3];
        let mut count = 0;
        throttle.new_tick(|code: Sample, suppressed| {
            reports[count] = Some((code, suppressed));
            count += 1;
        });
        (reports, count)
    }

    #[test]
    fn the_defaults_are_a_limit_of_five_and_two_hundred_and_fifty_six_events() {
        assert_eq!(throttle::EV_MAX_PER_TICK, 5);
        assert_eq!(throttle::EV_MAX_CODES, 256);
    }

    #[test]
    fn sends_within_the_limit_are_let_through() {
        let mut throttle = throttle::Throttle::new();
        for _ in 0..LIMIT {
            assert_eq!(throttle.throttle(&Sample::A), None);
        }
    }

    #[test]
    fn sends_past_the_limit_are_suppressed_and_say_how_far_over() {
        let mut throttle = throttle::Throttle::new();
        for _ in 0..LIMIT {
            throttle.throttle(&Sample::A);
        }
        assert_eq!(throttle.throttle(&Sample::A), Some(1));
        assert_eq!(throttle.throttle(&Sample::A), Some(2));
        assert_eq!(throttle.throttle(&Sample::A), Some(3));
    }

    #[test]
    fn each_event_has_its_own_counter() {
        let mut throttle = throttle::Throttle::new();
        for _ in 0..LIMIT + 3 {
            throttle.throttle(&Sample::A);
        }
        // B and C are not affected by A being noisy.
        assert_eq!(throttle.throttle(&Sample::B), None);
        assert_eq!(throttle.throttle(&Sample::C), None);
    }

    #[test]
    fn every_event_can_be_sent_once_however_many_events_there_are() {
        // The point of one counter per event: nothing to fill up.
        let mut throttle = throttle::Throttle::new();
        for code in [Sample::A, Sample::B, Sample::C] {
            assert_eq!(throttle.throttle(&code), None);
        }
    }

    #[test]
    fn a_new_tick_reports_each_suppressed_event_with_its_count_and_starts_again() {
        let mut throttle = throttle::Throttle::new();
        for _ in 0..LIMIT + 4 {
            throttle.throttle(&Sample::A);
        }
        for _ in 0..LIMIT + 1 {
            throttle.throttle(&Sample::C);
        }
        let (reports, count) = ended(&mut throttle);
        assert_eq!(count, 2);
        assert_eq!(reports[0], Some((Sample::A, 4)));
        assert_eq!(reports[1], Some((Sample::C, 1)));
        // Everything starts again: the full limit is available.
        for _ in 0..LIMIT {
            assert_eq!(throttle.throttle(&Sample::A), None);
        }
        assert_eq!(throttle.throttle(&Sample::A), Some(1));
    }

    #[test]
    fn nothing_is_reported_when_nothing_was_suppressed() {
        let mut throttle = throttle::Throttle::new();
        for _ in 0..LIMIT {
            throttle.throttle(&Sample::A);
        }
        throttle.throttle(&Sample::B);
        assert_eq!(ended(&mut throttle).1, 0);
    }

    #[test]
    fn a_second_new_tick_with_no_sends_reports_nothing() {
        let mut throttle = throttle::Throttle::new();
        for _ in 0..LIMIT + 2 {
            throttle.throttle(&Sample::B);
        }
        assert_eq!(ended(&mut throttle).1, 1);
        assert_eq!(ended(&mut throttle).1, 0);
    }

    #[test]
    fn the_counts_stop_at_255_and_do_not_wrap() {
        let mut throttle = throttle::Throttle::new();
        for _ in 0..1000 {
            throttle.throttle(&Sample::A);
        }
        let (reports, _) = ended(&mut throttle);
        assert_eq!(reports[0], Some((Sample::A, u8::MAX - LIMIT)));
    }

    #[test]
    fn an_event_whose_index_is_past_the_counters_is_let_through_and_does_not_panic() {
        let mut throttle = throttle::Throttle::new();
        for _ in 0..u16::from(LIMIT) + 5 {
            assert_eq!(throttle.throttle(&Wild), None);
        }
    }

    #[test]
    fn a_throttle_made_with_default_is_empty() {
        let mut throttle: throttle::Throttle = throttle::Throttle::default();
        assert_eq!(throttle.throttle(&Sample::A), None);
    }
}
