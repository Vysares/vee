//! The core's own events.
//!
//! They are made the same way as any module's: an enum of codes with `#[derive(event::Code)]`.
//! The derive writes `code()` and `severity()` for it (see `vee_core_api::event::Code`). An
//! event's details are not part of the code: they are written into its text where the core sends
//! it, for example "reset 7: previous run ended clean (signal), 3 unclean in a row".
//!
//! Who calls what: the core calls `code()` and `severity()` on a `CoreEvent` when it builds the
//! `Event` for it, with `SOURCE` and the text. The methods are the `Code` trait's. To call them,
//! import it with `use vee_core_api::event::Code as _;`.

use vee_core_api::event;

/// The core's source id.
pub const SOURCE: u8 = 0;

/// An event the core sends about itself. Each variant's value is its event code: they count up
/// from 0 in the order written, so new events go at the end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, event::Code)]
#[repr(u16)]
pub enum CoreEvent {
    /// Sent once at startup, after the reset log is read. It is the first event of every run,
    /// so a reader of the event log can split the log into runs at these. The text has this
    /// run's boot count, how the previous run ended, and how many unclean runs came before.
    #[severity(Info)]
    ResetReport,

    /// The reset log ends part way through a record.
    #[severity(Warn)]
    ResetLogPartialRecord,

    /// A record in the reset log could not be decoded. The text says why.
    #[severity(Err)]
    ResetLogCorruptRecord,

    /// The reset log's records decode, but are in an order a real log never has.
    #[severity(Err)]
    ResetLogMisorderedRecords,

    /// The reset log's last boot count is the largest a `u32` can hold.
    #[severity(Err)]
    ResetLogCountOverflow,

    /// The event log ends part way through a record (a crash while appending). The core renames
    /// the log to `.1` and starts a new one.
    #[severity(Warn)]
    EventLogPartialRecord,

    /// A source sent the same event more times in one tick than the limit
    /// (`throttle::EV_MAX_PER_TICK`), so the extra sends were dropped. The text has the source,
    /// the event's code and how many were dropped. One of these is sent per event per tick, no
    /// matter how many were dropped.
    #[severity(Err)]
    EventsSuppressed,

    /// The event queue was full, so events were dropped. The text has how many. The events
    /// themselves are not kept, only this count: the newest events of the burst are the lost
    /// ones (the queue keeps the oldest). They were still written to the system log, which has
    /// every event as it was sent. See `EventsSuppressed` for the limit on each source, which
    /// is meant to make this rare.
    #[severity(Err)]
    EventsDropped,

    /// The reset log could not be read or written, at startup or at shutdown. The text has the
    /// file error. At startup the core carries on with no history (`ResetInfo::first_boot()`), so
    /// modules that react to `consecutive_unclean` cannot see a reset loop while this lasts.
    #[severity(Err)]
    ResetLogUnavailable,

    /// The event log could not be checked at startup. The text has the file error. The core
    /// carries on. Events are still sent to the system log and the downlink, and each tick's failed
    /// writes to the event log are reported to the system log.
    #[severity(Err)]
    EventLogUnavailable,
}

#[cfg(test)]
mod tests {
    use super as core_event;
    use vee_core_api::event;
    use vee_core_api::event::Code as _;

    #[test]
    fn the_core_is_source_zero() {
        assert_eq!(core_event::SOURCE, 0);
    }

    #[test]
    fn codes_count_up_from_zero_in_the_order_written() {
        assert_eq!(core_event::CoreEvent::ResetReport.code(), 0);
        assert_eq!(core_event::CoreEvent::ResetLogPartialRecord.code(), 1);
        assert_eq!(core_event::CoreEvent::ResetLogCorruptRecord.code(), 2);
        assert_eq!(core_event::CoreEvent::ResetLogMisorderedRecords.code(), 3);
        assert_eq!(core_event::CoreEvent::ResetLogCountOverflow.code(), 4);
        assert_eq!(core_event::CoreEvent::EventLogPartialRecord.code(), 5);
        assert_eq!(core_event::CoreEvent::EventsSuppressed.code(), 6);
        assert_eq!(core_event::CoreEvent::EventsDropped.code(), 7);
        assert_eq!(core_event::CoreEvent::ResetLogUnavailable.code(), 8);
        assert_eq!(core_event::CoreEvent::EventLogUnavailable.code(), 9);
    }

    #[test]
    fn severities_are_info_for_the_report_warn_for_a_partial_record_and_err_otherwise() {
        assert_eq!(
            core_event::CoreEvent::EventLogPartialRecord.severity(),
            event::Severity::Warn
        );
        assert_eq!(
            core_event::CoreEvent::EventsSuppressed.severity(),
            event::Severity::Err
        );
        assert_eq!(
            core_event::CoreEvent::ResetReport.severity(),
            event::Severity::Info
        );
        assert_eq!(
            core_event::CoreEvent::ResetLogPartialRecord.severity(),
            event::Severity::Warn
        );
        for ev in [
            core_event::CoreEvent::ResetLogCorruptRecord,
            core_event::CoreEvent::ResetLogMisorderedRecords,
            core_event::CoreEvent::ResetLogCountOverflow,
            core_event::CoreEvent::EventsDropped,
            core_event::CoreEvent::ResetLogUnavailable,
            core_event::CoreEvent::EventLogUnavailable,
        ] {
            assert_eq!(ev.severity(), event::Severity::Err);
        }
    }
}
