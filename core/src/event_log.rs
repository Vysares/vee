//! The event log on disk: the file side of the event logger.
//!
//! It is an append-only file of fixed-size records, one per event (the record format is in
//! `vee_core_types::event_log`). `start` runs once at startup and `append` writes one event.
//!
//! Both work through the OSAL's `Files`, and block. That is right for startup. In the tick loop
//! nothing may block, so the main thread will not call `append` itself: it will encode events and
//! hand them to the file service, and `append` is what the file service's thread will run. That
//! service and the main-thread half (taking events out of `Ev`) are not written yet.

use core::fmt;
use vee_core_api::config;
use vee_core_api::event;
use vee_core_types::core_event;
use vee_core_types::event_log as log_types;
use vee_osal::files;

config::setting! {
    /// How many records the event log may hold before `start` rotates it.
    pub EVENT_LOG_MAX_RECORDS: u64 = 10000, 1..=1000000;
}

/// What `start` found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Started {
    /// The old log ended part way through a record (a crash while appending). The caller should
    /// report it: see `report`.
    pub partial_record: bool,
}

/// Runs once at startup, before any event is appended. It makes sure appending is safe.
///
/// If the old log is full (it has at least `max_records` whole records) or ends in a partial
/// record, it is renamed to `<path>.1` (replacing any older `.1`), so the next `append` starts a
/// new file. A partial record has to be rotated away: appending after it would shift every later
/// record out of line, and `Files` has no way to cut a file short. The whole records in the old
/// log are kept in `.1`. Nothing is read: the log is only ever appended to, and whoever reads it
/// checks each record's CRC.
pub fn start<F: files::Files>(
    fs: &F,
    path: &str,
    max_records: u64,
) -> Result<Started, files::FileError> {
    // A missing file and an empty file mean the same thing here: there are no records.
    let len = fs.len(path)?.unwrap_or(0);
    let record_len = log_types::RECORD_LEN as u64;
    let whole_records = len / record_len;
    let partial_record = len % record_len != 0;

    let full = whole_records >= max_records;
    if full || partial_record {
        fs.rotate(path)?;
    }
    Ok(Started { partial_record })
}

/// Turns what `start` found into the events to send, in the same way as `reset_log::report`: it
/// calls `send` once per event, with the event's code and its text.
pub fn report<F>(started: &Started, mut send: F)
where
    F: FnMut(core_event::CoreEvent, fmt::Arguments<'_>),
{
    if started.partial_record {
        send(
            core_event::CoreEvent::EventLogPartialRecord,
            format_args!("event log ends in a partial record"),
        );
    }
}

/// Adds one event to the end of the log, as one record. The record has been flushed to the disk
/// when this returns.
pub fn append<F: files::Files>(
    fs: &F,
    path: &str,
    event: &event::Event,
) -> Result<(), files::FileError> {
    fs.append(path, &log_types::encode(event))
}

#[cfg(test)]
mod tests {
    use super as event_log;
    use vee_core_api::event;
    use vee_core_api::fixed_str;
    use vee_core_types::core_event;
    use vee_core_types::event_log as log_types;
    use vee_osal::fake_files;
    use vee_osal::files::Files as _;

    const PATH: &str = "events.log";
    const RECORD_LEN: usize = log_types::RECORD_LEN;

    fn event(time: u64) -> event::Event {
        event::Event {
            source: 1,
            code: 2,
            severity: event::Severity::Info,
            monotonic_us: time,
            text: fixed_str::FixedStr::truncated("e").unwrap(),
        }
    }

    /// A fake file system holding a log of `count` whole records, with times 0, 1, 2, ...
    fn files_with(count: u64) -> fake_files::FakeFiles {
        let files = fake_files::FakeFiles::new();
        for time in 0..count {
            event_log::append(&files, PATH, &event(time)).unwrap();
        }
        files
    }

    #[test]
    fn a_missing_log_needs_nothing() {
        let files = fake_files::FakeFiles::new();
        let started = event_log::start(&files, PATH, 10).unwrap();
        assert_eq!(
            started,
            event_log::Started {
                partial_record: false
            }
        );
        assert_eq!(files.get(PATH), None);
        assert_eq!(files.get("events.log.1"), None);
    }

    #[test]
    fn a_log_under_the_cap_is_left_alone() {
        let files = files_with(3);
        let before = files.get(PATH);
        let started = event_log::start(&files, PATH, 4).unwrap();
        assert_eq!(
            started,
            event_log::Started {
                partial_record: false
            }
        );
        assert_eq!(files.get(PATH), before);
        assert_eq!(files.get("events.log.1"), None);
    }

    #[test]
    fn a_log_at_the_cap_is_rotated() {
        let files = files_with(4);
        let before = files.get(PATH);
        let started = event_log::start(&files, PATH, 4).unwrap();
        assert_eq!(
            started,
            event_log::Started {
                partial_record: false
            }
        );
        assert_eq!(files.get(PATH), None);
        assert_eq!(files.get("events.log.1"), before);
    }

    #[test]
    fn rotating_replaces_an_older_dot_one() {
        let files = files_with(2);
        files.put("events.log.1", b"old");
        let before = files.get(PATH);
        event_log::start(&files, PATH, 2).unwrap();
        assert_eq!(files.get("events.log.1"), before);
    }

    #[test]
    fn a_partial_record_is_reported_and_the_log_is_rotated_with_its_whole_records_kept() {
        let files = files_with(2);
        files.append(PATH, &[1, 2, 3]).unwrap();
        let before = files.get(PATH);
        let started = event_log::start(&files, PATH, 100).unwrap();
        assert_eq!(
            started,
            event_log::Started {
                partial_record: true
            }
        );
        assert_eq!(files.get(PATH), None);
        assert_eq!(files.get("events.log.1"), before);
        assert_eq!(before.unwrap().len(), 2 * RECORD_LEN + 3);
    }

    #[test]
    fn a_log_that_is_only_a_partial_record_is_rotated_too() {
        let files = fake_files::FakeFiles::new();
        files.put(PATH, &[7; 5]);
        let started = event_log::start(&files, PATH, 100).unwrap();
        assert!(started.partial_record);
        assert_eq!(files.get("events.log.1").as_deref(), Some(&[7u8; 5][..]));
    }

    #[test]
    fn append_writes_one_record_that_decodes_to_the_event() {
        let files = fake_files::FakeFiles::new();
        event_log::append(&files, PATH, &event(9)).unwrap();
        let bytes = files.get(PATH).unwrap();
        assert_eq!(bytes.len(), RECORD_LEN);
        assert_eq!(log_types::decode(&bytes), Ok(event(9)));
    }

    #[test]
    fn appended_events_are_in_order_one_record_each() {
        let files = files_with(3);
        let bytes = files.get(PATH).unwrap();
        assert_eq!(bytes.len(), 3 * RECORD_LEN);
        for (time, record) in bytes.chunks(RECORD_LEN).enumerate() {
            assert_eq!(log_types::decode(record), Ok(event(time as u64)));
        }
    }

    #[test]
    fn after_a_rotation_the_next_append_starts_a_new_log() {
        let files = files_with(2);
        event_log::start(&files, PATH, 2).unwrap();
        event_log::append(&files, PATH, &event(50)).unwrap();
        assert_eq!(files.get(PATH).unwrap().len(), RECORD_LEN);
        assert_eq!(files.get("events.log.1").unwrap().len(), 2 * RECORD_LEN);
    }

    /// The first event `report` sends (as its code), and how many it sent.
    fn reported(started: &event_log::Started) -> (Option<core_event::CoreEvent>, usize) {
        let mut first = None;
        let mut count = 0;
        event_log::report(started, |code, _text| {
            first.get_or_insert(code);
            count += 1;
        });
        (first, count)
    }

    #[test]
    fn nothing_is_reported_when_nothing_is_wrong() {
        let started = event_log::Started {
            partial_record: false,
        };
        assert_eq!(reported(&started), (None, 0));
    }

    #[test]
    fn a_partial_record_is_reported_as_a_warning_with_its_text() {
        use core::fmt::Write as _;
        use vee_core_api::event::Code as _;
        let started = event_log::Started {
            partial_record: true,
        };
        let mut text = fixed_str::FixedStr::<64>::EMPTY;
        let mut sent = None;
        event_log::report(&started, |code, args| {
            write!(text, "{args}").unwrap();
            sent = Some(code);
        });
        let code = sent.unwrap();
        assert_eq!(code, core_event::CoreEvent::EventLogPartialRecord);
        assert_eq!(code.severity(), event::Severity::Warn);
        assert_eq!(text.as_bytes(), b"event log ends in a partial record");
    }

    #[test]
    fn the_default_cap_is_ten_thousand_records() {
        assert_eq!(event_log::EVENT_LOG_MAX_RECORDS, 10000);
    }
}
