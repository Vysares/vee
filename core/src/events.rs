//! The core's side of the event service. The queue itself is `vee_core_api::ev::Ev`; this is
//! what the core does with it each tick. So far that is `report_dropped`. Taking events out of the
//! queue for the event log and the downlink is not written yet: those need the file service and
//! the bus.
//!
//! Each event is also written to the system log, but not here: `Ev::send` does that when the
//! event is sent, before the queue, so an event the full queue drops is still seen there.

use vee_core_api::ev;
use vee_core_types::core_event;

/// Sends `CoreEvent::EventsDropped` if the queue dropped events since this was last called. It
/// does nothing if it dropped none. The event has how many, for example
/// `37 events dropped: the queue was full`: at most 45 bytes (the count is a `u32`, at most 10
/// digits), so it fits the default `EV_TEXT_MAX_BYTES` of 64.
///
/// Who calls it: the core, in its tick, after it has taken every event out of the queue. That
/// order matters. The queue was full when the events were dropped, and the event is sent into the
/// same queue, so with room made it is sure to fit. If the queue were still full, the event would
/// be dropped too and counted for the next call, so a loss is not silent, but this tick's report
/// would be late.
pub fn report_dropped(ev: &mut ev::Ev, now: u64) {
    let dropped = ev.take_dropped();
    if dropped > 0 {
        ev.send(
            core_event::SOURCE,
            now,
            core_event::CoreEvent::EventsDropped,
            format_args!("{dropped} events dropped: the queue was full"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super as events;
    use vee_core_api::ev;
    use vee_core_api::event;
    use vee_core_types::core_event;

    /// Sends one event, which one does not matter.
    fn send(queue: &mut ev::Ev) {
        queue.send(1, 0, core_event::CoreEvent::ResetReport, format_args!("e"));
    }

    /// Takes every event out of the queue, as the core will.
    fn empty(queue: &mut ev::Ev) {
        while queue.pop().is_some() {}
    }

    /// A queue that was full when `extra` more events were sent, so `extra` were dropped, and has
    /// then been emptied.
    fn queue_that_dropped(extra: usize) -> ev::Ev {
        let mut queue = ev::Ev::new();
        for _ in 0..ev::EV_QUEUE_DEPTH.saturating_add(extra) {
            send(&mut queue);
        }
        empty(&mut queue);
        queue
    }

    #[test]
    fn nothing_is_sent_when_nothing_was_dropped() {
        let mut queue = ev::Ev::new();
        events::report_dropped(&mut queue, 5);
        assert!(queue.is_empty());
    }

    #[test]
    fn the_event_has_the_core_as_its_source_the_time_and_how_many_were_dropped() {
        let mut queue = queue_that_dropped(3);
        events::report_dropped(&mut queue, 77);
        let event = queue.pop().unwrap();
        assert_eq!(event.source, core_event::SOURCE);
        assert_eq!(event.code, 7);
        assert_eq!(event.severity, event::Severity::Err);
        assert_eq!(event.monotonic_us, 77);
        assert_eq!(
            event.text.as_bytes(),
            b"3 events dropped: the queue was full"
        );
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn the_count_starts_again_after_it_is_reported() {
        let mut queue = queue_that_dropped(3);
        events::report_dropped(&mut queue, 1);
        empty(&mut queue);
        events::report_dropped(&mut queue, 2);
        assert!(queue.is_empty());
    }

    #[test]
    fn if_the_queue_is_still_full_the_report_is_dropped_and_counted_for_the_next_call() {
        let mut queue = queue_that_dropped(3);
        for _ in 0..ev::EV_QUEUE_DEPTH {
            send(&mut queue); // full again, and not emptied
        }
        events::report_dropped(&mut queue, 1);
        assert_eq!(queue.take_dropped(), 1); // the report itself, lost
    }
}
