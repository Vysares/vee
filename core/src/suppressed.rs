//! Reports the events a source was not allowed to send, because it sent them too often in one
//! tick (see `vee_core_api::throttle`).
//!
//! `report` is generic over the source's event enum (`C`), because the throttle only keeps a
//! count for each event's position, and the enum is what turns a position back into a code.
//! So the executive calls it once for each module, with that module's enum, and once for the
//! core's own source if the core ever needs it (today the core's events skip the throttle).

use core::fmt;
use vee_core_api::event;
use vee_core_api::throttle;
use vee_core_types::core_event;

/// Starts a new tick for one source's `throttle`, and sends one `EventsSuppressed` for each of
/// its events that was suppressed in the tick that just ended. It calls `send` with the event's
/// code and its text, like `reset_log::report`, and it does no I/O.
///
/// The text has the source, the event's code and how many sends were dropped, for example
/// `suppressed: source 3 code 7, 12 dropped`. At most 46 bytes, so it fits the default
/// `EV_TEXT_MAX_BYTES` of 64. The count stops at 255 minus the limit
/// (`throttle::EV_MAX_PER_TICK`), so it can be "at least this many".
///
/// The executive calls this at the top of every tick, for every module, before the core's own
/// tick and before any module's `step`. It also zeroes the counters, which starts the new tick.
pub fn report<C, F>(throttle: &mut throttle::Throttle, source: u8, mut send: F)
where
    C: event::Code,
    F: FnMut(core_event::CoreEvent, fmt::Arguments<'_>),
{
    throttle.new_tick(|code: C, dropped| {
        send(
            core_event::CoreEvent::EventsSuppressed,
            format_args!(
                "suppressed: source {source} code {}, {dropped} dropped",
                code.code()
            ),
        );
    });
}

#[cfg(test)]
mod tests {
    use super as suppressed;
    use core::fmt::Write as _;
    use vee_core_api::event;
    use vee_core_api::fixed_str;
    use vee_core_api::throttle;
    use vee_core_types::core_event;

    const LIMIT: u8 = throttle::EV_MAX_PER_TICK;

    /// A module's events. `Second` and `Largest` have explicit codes, so the text shows the code,
    /// not the position.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, event::Code)]
    #[repr(u16)]
    enum Sample {
        #[severity(Info)]
        First,
        #[severity(Warn)]
        Second = 7,
        #[severity(Err)]
        Largest = 65535,
    }

    /// The text of one event, written the way the event service will write it. Longer than any
    /// event's text, so these tests do not depend on `EV_TEXT_MAX_BYTES`.
    type Sent = (core_event::CoreEvent, fixed_str::FixedStr<80>);

    /// Everything `report` sends, in order, and how many events that was.
    fn reported(throttle: &mut throttle::Throttle, source: u8) -> ([Option<Sent>; 3], usize) {
        let mut sent = [None; 3];
        let mut count: usize = 0;
        suppressed::report::<Sample, _>(throttle, source, |code, text| {
            let mut written = fixed_str::FixedStr::EMPTY;
            write!(written, "{text}").unwrap();
            sent[count] = Some((code, written));
            count = count.saturating_add(1);
        });
        (sent, count)
    }

    /// Sends `code` `times` times through the throttle, as `ctx.ev.send` would.
    fn send_times(throttle: &mut throttle::Throttle, code: Sample, times: u8) {
        for _ in 0..times {
            throttle.throttle(&code);
        }
    }

    fn text_of(sent: &Option<Sent>) -> &[u8] {
        sent.as_ref().unwrap().1.as_bytes()
    }

    #[test]
    fn nothing_is_sent_when_nothing_was_suppressed() {
        let mut throttle = throttle::Throttle::new();
        assert_eq!(reported(&mut throttle, 3).1, 0);
    }

    #[test]
    fn sending_exactly_the_limit_is_not_suppression() {
        let mut throttle = throttle::Throttle::new();
        send_times(&mut throttle, Sample::First, LIMIT);
        assert_eq!(reported(&mut throttle, 3).1, 0);
    }

    #[test]
    fn one_send_over_the_limit_gives_one_event_with_the_source_the_code_and_the_count() {
        let mut throttle = throttle::Throttle::new();
        send_times(&mut throttle, Sample::First, LIMIT.saturating_add(1));
        let (sent, count) = reported(&mut throttle, 3);
        assert_eq!(count, 1);
        assert_eq!(sent[0].unwrap().0, core_event::CoreEvent::EventsSuppressed);
        assert_eq!(text_of(&sent[0]), b"suppressed: source 3 code 0, 1 dropped");
    }

    #[test]
    fn many_sends_over_the_limit_are_still_one_event() {
        let mut throttle = throttle::Throttle::new();
        send_times(&mut throttle, Sample::Second, LIMIT.saturating_add(12));
        let (sent, count) = reported(&mut throttle, 3);
        assert_eq!(count, 1);
        assert_eq!(
            text_of(&sent[0]),
            b"suppressed: source 3 code 7, 12 dropped"
        );
    }

    #[test]
    fn each_suppressed_event_is_reported_once_in_the_order_of_the_enum() {
        let mut throttle = throttle::Throttle::new();
        // Suppress the second event first, so the order comes from the enum, not from the sends.
        send_times(&mut throttle, Sample::Second, LIMIT.saturating_add(2));
        send_times(&mut throttle, Sample::First, LIMIT.saturating_add(1));
        let (sent, count) = reported(&mut throttle, 3);
        assert_eq!(count, 2);
        assert_eq!(text_of(&sent[0]), b"suppressed: source 3 code 0, 1 dropped");
        assert_eq!(text_of(&sent[1]), b"suppressed: source 3 code 7, 2 dropped");
    }

    #[test]
    fn a_report_starts_a_new_tick() {
        let mut throttle = throttle::Throttle::new();
        send_times(&mut throttle, Sample::First, LIMIT.saturating_add(1));
        assert_eq!(reported(&mut throttle, 3).1, 1);
        // The counters were zeroed, so the same event is allowed again, and nothing is left over.
        assert_eq!(reported(&mut throttle, 3).1, 0);
        send_times(&mut throttle, Sample::First, LIMIT);
        assert_eq!(reported(&mut throttle, 3).1, 0);
    }

    #[test]
    fn the_longest_text_is_at_most_the_length_the_docs_promise() {
        let mut throttle = throttle::Throttle::new();
        // The largest source, the largest code, and as many sends as the counter can hold.
        send_times(&mut throttle, Sample::Largest, u8::MAX);
        let (sent, count) = reported(&mut throttle, u8::MAX);
        assert_eq!(count, 1);
        assert!(text_of(&sent[0]).len() <= MAX_TEXT_LEN);
    }

    /// What `report`'s doc says: it fits the default `EV_TEXT_MAX_BYTES` of 64.
    const MAX_TEXT_LEN: usize = 46;
}
