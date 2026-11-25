//! The event log on disk: the file side of the event logger.
//!
//! It is an append-only file of fixed-size records, one per event (the record format is in
//! `vee_core_types::event_log`). `start` runs once at startup and `append` writes one event.
//!
//! Both work through the OSAL's `Files`, and block. That is right for startup. In the tick loop
//! nothing may block, so the main thread will not call `append` itself: it will encode events and
//! hand them to the file service, and `append` is what the file service's thread will run. That
//! service and the main-thread half (taking events out of `Ev`) are not written yet.

use crate::records;
use core::fmt;
use vee_core_api::config;
use vee_core_api::ev;
use vee_core_api::file;
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
    let shape = records::shape(len, log_types::RECORD_LEN);

    // A log with no records is never full, whatever the cap: a missing file has nothing to rotate
    // (rotating it would be `NotFound`).
    let full = shape.whole > 0 && shape.whole >= max_records;
    if full || shape.has_partial {
        fs.rotate(path)?;
    }
    Ok(Started {
        partial_record: shape.has_partial,
    })
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

/// Takes the events waiting in the log queue and makes one batch of them: their records, one after
/// the other, in order, as the bytes of one request to append to the log. It takes as many as fit
/// in the request's data (`file::FILE_DATA_MAX_BYTES`, so the number is whatever that allows, and
/// the batch is never too big), and leaves the rest in the queue for the next batch. An empty
/// queue gives an empty batch.
///
/// An event is taken out of the queue only after its record has been added to the batch, so
/// nothing is lost between the two. The record is looked at first (`peek`), and added with
/// `add_bytes`: the first that does not fit ends the batch.
pub fn batch(ev: &mut ev::Ev) -> file::Data {
    let mut data = file::Data::EMPTY;
    while let Some(event) = ev.log.peek() {
        let record = log_types::encode(event);
        if data.add_bytes(&record).is_err() {
            break;
        }
        ev.log.pop();
    }
    data
}

#[cfg(test)]
mod tests {
    use super as event_log;
    use vee_core_api::ev;
    use vee_core_api::event;
    use vee_core_api::file;
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
            files
                .append(PATH, &log_types::encode(&event(time)))
                .unwrap();
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
    fn a_log_one_record_over_the_cap_is_rotated() {
        let files = files_with(5);
        let before = files.get(PATH);
        event_log::start(&files, PATH, 4).unwrap();
        assert_eq!(files.get(PATH), None);
        assert_eq!(files.get("events.log.1"), before);
    }

    #[test]
    fn at_each_side_of_the_cap_only_a_full_log_is_rotated() {
        // The cap itself, one under, and one over, for a cap of 1 (the smallest the setting
        // allows), 2 and 10.
        for cap in [1_u64, 2, 10] {
            for (records, rotated) in [(cap - 1, false), (cap, true), (cap + 1, true)] {
                let files = files_with(records);
                event_log::start(&files, PATH, cap).unwrap();
                assert_eq!(
                    files.get(PATH).is_none() && records > 0,
                    rotated,
                    "cap {cap}, {records} records"
                );
            }
        }
    }

    #[test]
    fn a_partial_record_of_one_byte_or_of_all_but_one_is_found_and_the_whole_ones_are_kept() {
        for extra in [1, RECORD_LEN - 1] {
            let files = files_with(2);
            let mut bytes = files.get(PATH).unwrap();
            bytes.resize(bytes.len() + extra, 0xEE);
            files.write_file(PATH, &bytes).unwrap();

            let started = event_log::start(&files, PATH, 100).unwrap();
            assert!(started.partial_record, "{extra} extra bytes");
            assert_eq!(files.get(PATH), None);
            assert_eq!(
                files.get("events.log.1"),
                Some(bytes),
                "{extra} extra bytes"
            );
        }
    }

    #[test]
    fn a_file_of_exactly_whole_records_has_no_partial_record() {
        let files = files_with(2);
        let started = event_log::start(&files, PATH, 100).unwrap();
        assert!(!started.partial_record);
    }

    #[test]
    fn a_cap_of_zero_is_not_reachable_from_the_setting_but_does_not_fail_on_a_missing_log() {
        // The setting's smallest value is 1, so this is not a real input. A cap of 0 makes every
        // log full, and a missing one has nothing to rotate: it must not become an error.
        let files = fake_files::FakeFiles::new();
        assert_eq!(
            event_log::start(&files, PATH, 0),
            Ok(event_log::Started {
                partial_record: false
            })
        );
        assert_eq!(files.get("events.log.1"), None);
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
    fn a_log_built_a_record_at_a_time_has_them_in_order() {
        let files = files_with(3);
        let bytes = files.get(PATH).unwrap();
        assert_eq!(bytes.len(), 3 * RECORD_LEN);
        for (time, record) in bytes.chunks(RECORD_LEN).enumerate() {
            assert_eq!(log_types::decode(record), Ok(event(time as u64)));
        }
    }

    /// An event queue with `count` events, with times 0, 1, 2, ...
    fn queue_of(count: u64) -> ev::Ev {
        let mut queue = ev::Ev::new();
        for time in 0..count {
            queue.send(
                1,
                time,
                core_event::CoreEvent::ResetReport,
                format_args!("e"),
            );
        }
        queue
    }

    /// The times of the records in a batch, in order, and how many there are.
    fn times_in(data: &file::Data) -> (u64, u64) {
        let mut first = None;
        let mut next_expected = 0;
        for record in data.as_slice().chunks(RECORD_LEN) {
            let time = log_types::decode(record).unwrap().monotonic_us;
            assert_eq!(time, first.unwrap_or(time).saturating_add(next_expected));
            first.get_or_insert(time);
            next_expected = next_expected.saturating_add(1);
        }
        (first.unwrap_or(0), next_expected)
    }

    #[test]
    fn a_batch_of_an_empty_queue_is_empty() {
        let mut queue = ev::Ev::new();
        assert!(event_log::batch(&mut queue).is_empty());
    }

    #[test]
    fn a_batch_has_every_waiting_event_as_one_record_in_order_and_empties_the_queue() {
        let mut queue = queue_of(3);
        let data = event_log::batch(&mut queue);
        assert_eq!(data.len(), 3 * RECORD_LEN);
        assert_eq!(times_in(&data), (0, 3));
        assert!(queue.log.is_empty());
    }

    #[test]
    fn a_batch_stops_at_the_record_that_does_not_fit_and_leaves_the_rest_in_order() {
        let fits = file::FILE_DATA_MAX_BYTES / RECORD_LEN;
        let mut queue = queue_of((fits + 5) as u64);

        let first = event_log::batch(&mut queue);
        // As many records as the data holds, and no more: no number of its own.
        assert_eq!(first.len(), fits * RECORD_LEN);
        assert_eq!(times_in(&first), (0, fits as u64));
        assert_eq!(queue.log.len(), 5); // the rest are still queued

        // The next batch carries on exactly where this one stopped.
        let second = event_log::batch(&mut queue);
        assert_eq!(times_in(&second), (fits as u64, 5));
        assert!(queue.log.is_empty());
    }

    #[test]
    fn nothing_is_taken_out_of_the_queue_that_is_not_in_the_batch() {
        // A batch that is exactly full leaves the next event queued, not lost.
        let fits = file::FILE_DATA_MAX_BYTES / RECORD_LEN;
        let mut queue = queue_of((fits + 1) as u64);
        event_log::batch(&mut queue);
        assert_eq!(queue.log.len(), 1);
        assert_eq!(queue.log.peek().unwrap().monotonic_us, fits as u64);
    }

    #[test]
    fn after_a_rotation_the_next_append_starts_a_new_log() {
        let files = files_with(2);
        event_log::start(&files, PATH, 2).unwrap();
        files.append(PATH, &log_types::encode(&event(50))).unwrap();
        assert_eq!(files.get(PATH).unwrap().len(), RECORD_LEN);
        assert_eq!(files.get("events.log.1").unwrap().len(), 2 * RECORD_LEN);
    }

    /// The first event `report` sends (as its code), and how many it sent.
    fn reported(started: &event_log::Started) -> (Option<core_event::CoreEvent>, usize) {
        let mut first = None;
        let mut count: usize = 0;
        event_log::report(started, |code, _text| {
            first.get_or_insert(code);
            count = count.saturating_add(1);
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
