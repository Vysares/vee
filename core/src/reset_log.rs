//! Reading the end of the reset log to work out how the previous run ended.
//!
//! `read_tail` has no I/O. It works on records that have already been read, so it can be tested
//! with plain byte arrays. `start` and `stop` do the file work, through the OSAL's `Files`.

use core::fmt;
use vee_core_api::config;
use vee_core_api::reset;
use vee_core_types::core_event;
use vee_core_types::reset_log as log_types;
use vee_osal::files;

/// How many records from the end of the log are read at startup. Unclean boots have no
/// `Shutdown` record, so this covers up to this many unclean boots in a row, or half as many
/// boots if every run was clean. It also caps `consecutive_unclean`.
pub const TAIL_RECORDS: usize = 8;

config::setting! {
    /// How many records the reset log may hold before `start` rotates it. The startup code passes
    /// it to `start` as `max_records`.
    pub RESET_LOG_MAX_RECORDS: u64 = 1024, 1..=1000000;
}

/// Something wrong with the log that the caller should report. Every problem except
/// `PartialRecord` makes `read_tail` fall back to `ResetInfo::first_boot()`, because the history
/// cannot be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// The file ends part way through a record (a crash while appending). Found by `start`, not
    /// by `read_tail`: the whole records before it are still trusted.
    PartialRecord,
    /// A record could not be decoded (bad CRC, unknown tag, and so on).
    CorruptRecord(log_types::DecodeError),
    /// The records decode, but are in an order a real log never has, such as two `Shutdown`
    /// records in a row.
    MisorderedRecords,
    /// The last boot count is the largest a `u32` can hold, so there is no next count.
    CountOverflow,
}

/// What reading the tail found: the reset info, and a problem if the log was not trustworthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TailReading {
    pub info: reset::ResetInfo,
    pub problem: Option<Problem>,
}

/// Works out the reset info for this run from the last records of the log.
///
/// `records` are whole records, oldest first, as many as were read (normally `TAIL_RECORDS`).
/// An empty slice means a missing or empty log, which is a first boot and not a problem.
pub fn read_tail(records: &[[u8; log_types::RECORD_LEN]]) -> TailReading {
    match scan(records) {
        Ok(info) => TailReading {
            info,
            problem: None,
        },
        Err(problem) => TailReading {
            info: reset::ResetInfo::first_boot(),
            problem: Some(problem),
        },
    }
}

fn scan(records: &[[u8; log_types::RECORD_LEN]]) -> Result<reset::ResetInfo, Problem> {
    if records.is_empty() {
        return Ok(reset::ResetInfo::first_boot());
    }

    let mut last_boot_count: Option<u32> = None;
    // True from a `Boot` until the `Shutdown` that closes its run. Every run seen here is an
    // earlier one: this run's `Boot` is only appended after the scan. So if it is still true
    // after the last record, the run just before this one ended uncleanly.
    let mut previous_run_open = false;
    // How the most recent run ended, as far as the records read so far say.
    let mut previous = reset::PreviousEnd::Unknown;
    // Runs before the most recent one that ended unclean, since the last clean shutdown.
    let mut unclean_runs: u32 = 0;

    for (index, record) in records.iter().enumerate() {
        match log_types::ResetLogEntry::decode(record).map_err(Problem::CorruptRecord)? {
            log_types::ResetLogEntry::Boot { count } => {
                // A new boot while a run is still open means that run never shut down.
                if previous_run_open {
                    unclean_runs += 1;
                }
                previous_run_open = true;
                last_boot_count = Some(count);
                previous = reset::PreviousEnd::Unknown;
            }
            log_types::ResetLogEntry::Shutdown { reason } => {
                // The first record read can be a `Shutdown` whose `Boot` is before the part of
                // the log that was read. Any other `Shutdown` must close a run.
                let is_first_record_read = index == 0;
                if !previous_run_open && !is_first_record_read {
                    return Err(Problem::MisorderedRecords);
                }
                previous_run_open = false;
                unclean_runs = 0;
                previous = reset::PreviousEnd::Clean(reason);
            }
        }
    }

    // A real log read with at least two records always has a `Boot` in it. Only a lone
    // `Shutdown` leaves no count to continue from.
    let last_boot_count = last_boot_count.ok_or(Problem::MisorderedRecords)?;
    let count = last_boot_count
        .checked_add(1)
        .ok_or(Problem::CountOverflow)?;

    // If the last run is still open, it is the most recent unclean run.
    let consecutive_unclean = unclean_runs + u32::from(previous_run_open);

    Ok(reset::ResetInfo {
        count,
        previous,
        consecutive_unclean,
    })
}

/// What `start` found and did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Started {
    /// The reset info for this run. `Boot { count: info.count }` is already in the log.
    pub info: reset::ResetInfo,
    /// Problems found in the old log, for the caller to turn into events once the event service
    /// exists. A partial record comes first, then a problem from `read_tail`.
    pub problems: [Option<Problem>; MAX_START_PROBLEMS],
}

/// One slot for a partial record, one for a `read_tail` problem.
pub const MAX_START_PROBLEMS: usize = 2;

/// Runs once at startup, before any module's `init`: works out how the last run ended and
/// records this boot.
///
/// 1. Reads the last `TAIL_RECORDS` whole records and gives them to `read_tail`.
/// 2. If the old log cannot be appended to as it is, renames it to `<path>.1` (replacing any
///    older `.1`). That is the case when it is over `max_records`, when it ends in a partial
///    record (appending after one would shift every later record out of place), or when
///    `read_tail` found a problem (so a bad record does not keep failing every later boot).
/// 3. Appends `Boot { count }`, which creates the file if it is missing or was just rotated.
///
/// A missing file is a first boot, not an error. Any file failure is returned and the caller
/// must not carry on as if the log worked.
pub fn start<F: files::Files>(
    fs: &F,
    path: &str,
    max_records: u64,
) -> Result<Started, files::FileError> {
    // A missing file and an empty file mean the same thing here: there are no records.
    let len = fs.len(path)?.unwrap_or(0);
    let record_len = log_types::RECORD_LEN as u64;
    let whole_records = len / record_len;
    let has_partial_record = len % record_len != 0;

    // Read only the last `TAIL_RECORDS` whole records. The partial record, if any, is after
    // them and is not read.
    let read_count = whole_records.min(TAIL_RECORDS as u64) as usize; // at most TAIL_RECORDS
    let first_offset = (whole_records - read_count as u64) * record_len;
    let mut records = [[0u8; log_types::RECORD_LEN]; TAIL_RECORDS];
    // Nothing to read from a missing or empty file (and a missing file would be `NotFound`).
    if read_count > 0 {
        fs.read_at(
            path,
            first_offset,
            &mut records.as_flattened_mut()[..read_count * log_types::RECORD_LEN],
        )?;
    }
    let reading = read_tail(&records[..read_count]);

    let mut problems = [None; MAX_START_PROBLEMS];
    if has_partial_record {
        problems[0] = Some(Problem::PartialRecord);
    }
    problems[1] = reading.problem;

    let over_cap = whole_records >= max_records;
    if over_cap || has_partial_record || reading.problem.is_some() {
        fs.rotate(path)?;
    }
    fs.append(
        path,
        &log_types::ResetLogEntry::Boot {
            count: reading.info.count,
        }
        .encode(),
    )?;

    Ok(Started {
        info: reading.info,
        problems,
    })
}

/// Runs on a clean shutdown: records why this run ended. Without it, the next boot sees this
/// run as `Unknown`.
pub fn stop<F: files::Files>(
    fs: &F,
    path: &str,
    reason: reset::ShutdownReason,
) -> Result<(), files::FileError> {
    fs.append(
        path,
        &log_types::ResetLogEntry::Shutdown { reason }.encode(),
    )
}

/// Turns what `start` found into the events to send: first the reset report, then one event for
/// each problem, in the order `start` found them. It calls `send` once per event, with the event's
/// code and its text.
///
/// It does no I/O and sends nothing itself, so it can be tested on its own. The startup code
/// calls it as `report(&started, |code, text| ctx.ev.send(code, text))`. The text is given as
/// `fmt::Arguments` (the result of `format_args!`), so the event service is the one that writes it
/// into the event's fixed size text, and cuts it off if it does not fit.
pub fn report<F>(started: &Started, mut send: F)
where
    F: FnMut(core_event::CoreEvent, fmt::Arguments<'_>),
{
    let reset::ResetInfo {
        count,
        previous,
        consecutive_unclean,
    } = started.info;
    send(
        core_event::CoreEvent::ResetReport,
        format_args!("reset {count}: prev end {previous}, {consecutive_unclean} unclean in a row"),
    );

    for problem in started.problems.iter().flatten() {
        match problem {
            Problem::PartialRecord => send(
                core_event::CoreEvent::ResetLogPartialRecord,
                format_args!("reset log ends in a partial record"),
            ),
            Problem::CorruptRecord(error) => send(
                core_event::CoreEvent::ResetLogCorruptRecord,
                format_args!("reset log corrupt record: {error}"),
            ),
            Problem::MisorderedRecords => send(
                core_event::CoreEvent::ResetLogMisorderedRecords,
                format_args!("reset log records are out of order"),
            ),
            Problem::CountOverflow => send(
                core_event::CoreEvent::ResetLogCountOverflow,
                format_args!("reset log boot count is at its maximum"),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super as reset_log;
    use core::fmt::Write as _;
    use vee_core_api::ev;
    use vee_core_api::event;
    use vee_core_api::fixed_str;
    use vee_core_api::reset;
    use vee_core_types::core_event;
    use vee_core_types::reset_log as log_types;
    use vee_osal::fake_files;
    use vee_osal::files::Files as _;

    fn boot(count: u32) -> log_types::ResetLogEntry {
        log_types::ResetLogEntry::Boot { count }
    }

    fn shutdown(reason: reset::ShutdownReason) -> log_types::ResetLogEntry {
        log_types::ResetLogEntry::Shutdown { reason }
    }

    const COMMANDED: reset::ShutdownReason = reset::ShutdownReason::Commanded;
    const SIGNAL: reset::ShutdownReason = reset::ShutdownReason::Signal;

    #[test]
    fn empty_log_is_a_first_boot_with_no_problem() {
        let reading = reset_log::read_tail(&[]);
        assert_eq!(reading.info, reset::ResetInfo::first_boot());
        assert_eq!(reading.problem, None);
    }

    #[test]
    fn one_boot_with_no_shutdown_is_one_unclean_run() {
        let records = [boot(1)].map(|e| e.encode());
        let reading = reset_log::read_tail(&records);
        assert_eq!(
            reading.info,
            reset::ResetInfo {
                count: 2,
                previous: reset::PreviousEnd::Unknown,
                consecutive_unclean: 1,
            }
        );
        assert_eq!(reading.problem, None);
    }

    #[test]
    fn boot_then_shutdown_is_a_clean_end_with_its_reason() {
        let records = [boot(1), shutdown(COMMANDED)].map(|e| e.encode());
        let reading = reset_log::read_tail(&records);
        assert_eq!(
            reading.info,
            reset::ResetInfo {
                count: 2,
                previous: reset::PreviousEnd::Clean(COMMANDED),
                consecutive_unclean: 0,
            }
        );
        assert_eq!(reading.problem, None);
    }

    #[test]
    fn a_clean_run_then_an_unclean_one_counts_one_unclean() {
        let records = [boot(1), shutdown(SIGNAL), boot(2)].map(|e| e.encode());
        let reading = reset_log::read_tail(&records);
        assert_eq!(
            reading.info,
            reset::ResetInfo {
                count: 3,
                previous: reset::PreviousEnd::Unknown,
                consecutive_unclean: 1,
            }
        );
    }

    #[test]
    fn three_boots_in_a_row_with_no_shutdown_are_three_unclean_runs() {
        let records = [boot(1), boot(2), boot(3)].map(|e| e.encode());
        let reading = reset_log::read_tail(&records);
        assert_eq!(
            reading.info,
            reset::ResetInfo {
                count: 4,
                previous: reset::PreviousEnd::Unknown,
                consecutive_unclean: 3,
            }
        );
    }

    #[test]
    fn a_clean_shutdown_ends_the_unclean_streak() {
        // Run 1 unclean, run 2 clean, runs 3 and 4 unclean: the streak is 2.
        let records = [boot(1), boot(2), shutdown(COMMANDED), boot(3), boot(4)].map(|e| e.encode());
        let reading = reset_log::read_tail(&records);
        assert_eq!(
            reading.info,
            reset::ResetInfo {
                count: 5,
                previous: reset::PreviousEnd::Unknown,
                consecutive_unclean: 2,
            }
        );
    }

    #[test]
    fn a_full_window_of_unclean_boots_hits_the_cap() {
        let records: [[u8; log_types::RECORD_LEN]; reset_log::TAIL_RECORDS] =
            core::array::from_fn(|i| boot(i as u32 + 10).encode());
        let reading = reset_log::read_tail(&records);
        assert_eq!(reading.info.count, 10 + reset_log::TAIL_RECORDS as u32);
        assert_eq!(
            reading.info.consecutive_unclean,
            reset_log::TAIL_RECORDS as u32
        );
    }

    #[test]
    fn a_window_can_start_with_a_shutdown_whose_boot_was_not_read() {
        let records = [shutdown(SIGNAL), boot(5), shutdown(COMMANDED)].map(|e| e.encode());
        let reading = reset_log::read_tail(&records);
        assert_eq!(
            reading.info,
            reset::ResetInfo {
                count: 6,
                previous: reset::PreviousEnd::Clean(COMMANDED),
                consecutive_unclean: 0,
            }
        );
        assert_eq!(reading.problem, None);
    }

    #[test]
    fn a_corrupt_record_falls_back_to_first_boot_and_reports_it() {
        let mut records = [boot(1), shutdown(COMMANDED), boot(2)].map(|e| e.encode());
        records[1][1] ^= 0x01;
        let reading = reset_log::read_tail(&records);
        assert_eq!(reading.info, reset::ResetInfo::first_boot());
        assert_eq!(
            reading.problem,
            Some(reset_log::Problem::CorruptRecord(
                log_types::DecodeError::BadCrc
            ))
        );
    }

    #[test]
    fn two_shutdowns_in_a_row_are_misordered() {
        let records = [boot(1), shutdown(COMMANDED), shutdown(COMMANDED)].map(|e| e.encode());
        let reading = reset_log::read_tail(&records);
        assert_eq!(reading.info, reset::ResetInfo::first_boot());
        assert_eq!(reading.problem, Some(reset_log::Problem::MisorderedRecords));
    }

    #[test]
    fn a_lone_shutdown_has_no_boot_to_count_from() {
        let records = [shutdown(COMMANDED)].map(|e| e.encode());
        let reading = reset_log::read_tail(&records);
        assert_eq!(reading.info, reset::ResetInfo::first_boot());
        assert_eq!(reading.problem, Some(reset_log::Problem::MisorderedRecords));
    }

    #[test]
    fn a_maximum_boot_count_cannot_be_incremented() {
        let records = [boot(u32::MAX)].map(|e| e.encode());
        let reading = reset_log::read_tail(&records);
        assert_eq!(reading.info, reset::ResetInfo::first_boot());
        assert_eq!(reading.problem, Some(reset_log::Problem::CountOverflow));
    }

    // ---- start and stop ----

    const PATH: &str = "reset.log";
    const CAP: u64 = 1024;

    /// A fake file system holding a reset log made of these entries.
    fn files_with(entries: &[log_types::ResetLogEntry]) -> fake_files::FakeFiles {
        let files = fake_files::FakeFiles::new();
        for entry in entries {
            files.append(PATH, &entry.encode()).unwrap();
        }
        files
    }

    /// The bytes of a log made of these entries.
    fn log_bytes<const N: usize>(
        entries: [log_types::ResetLogEntry; N],
    ) -> [[u8; log_types::RECORD_LEN]; N] {
        entries.map(|e| e.encode())
    }

    fn no_problems() -> [Option<reset_log::Problem>; reset_log::MAX_START_PROBLEMS] {
        [None; reset_log::MAX_START_PROBLEMS]
    }

    #[test]
    fn first_boot_creates_the_log_with_boot_1() {
        let files = fake_files::FakeFiles::new();
        let started = reset_log::start(&files, PATH, CAP).unwrap();
        assert_eq!(started.info, reset::ResetInfo::first_boot());
        assert_eq!(started.problems, no_problems());
        assert_eq!(files.get(PATH).as_deref(), Some(&boot(1).encode()[..]));
    }

    #[test]
    fn boot_stop_boot_reports_the_commanded_shutdown() {
        let files = fake_files::FakeFiles::new();
        reset_log::start(&files, PATH, CAP).unwrap();
        reset_log::stop(&files, PATH, COMMANDED).unwrap();
        let started = reset_log::start(&files, PATH, CAP).unwrap();
        assert_eq!(
            started.info,
            reset::ResetInfo {
                count: 2,
                previous: reset::PreviousEnd::Clean(COMMANDED),
                consecutive_unclean: 0,
            }
        );
        assert_eq!(started.problems, no_problems());
    }

    #[test]
    fn boot_with_no_stop_is_unclean_next_time() {
        let files = fake_files::FakeFiles::new();
        reset_log::start(&files, PATH, CAP).unwrap();
        let started = reset_log::start(&files, PATH, CAP).unwrap();
        assert_eq!(started.info.count, 2);
        assert_eq!(started.info.previous, reset::PreviousEnd::Unknown);
        assert_eq!(started.info.consecutive_unclean, 1);
    }

    #[test]
    fn only_the_last_tail_records_are_read_from_a_long_log() {
        // 20 runs, each clean, then one unclean: 41 records. Count must come from the end.
        let files = fake_files::FakeFiles::new();
        for count in 1..=20 {
            files.append(PATH, &boot(count).encode()).unwrap();
            files.append(PATH, &shutdown(COMMANDED).encode()).unwrap();
        }
        files.append(PATH, &boot(21).encode()).unwrap();
        let started = reset_log::start(&files, PATH, CAP).unwrap();
        assert_eq!(started.info.count, 22);
        assert_eq!(started.info.consecutive_unclean, 1);
        assert_eq!(started.problems, no_problems());
    }

    #[test]
    fn a_partial_record_is_reported_and_the_log_is_rotated_with_the_count_kept() {
        let files = files_with(&[boot(1), shutdown(COMMANDED), boot(2)]);
        let old = files.get(PATH).unwrap();
        // A crash part way through appending the next record.
        files.append(PATH, &[0x01, 0x02, 0x03]).unwrap();
        let old_with_partial = files.get(PATH).unwrap();
        assert_eq!(old_with_partial.len(), old.len() + 3);

        let started = reset_log::start(&files, PATH, CAP).unwrap();

        assert_eq!(started.info.count, 3);
        assert_eq!(started.info.previous, reset::PreviousEnd::Unknown);
        assert_eq!(
            started.problems,
            [Some(reset_log::Problem::PartialRecord), None]
        );
        // The evidence is kept untouched, and the new log is whole records only.
        assert_eq!(files.get("reset.log.1"), Some(old_with_partial));
        assert_eq!(files.get(PATH).as_deref(), Some(&boot(3).encode()[..]));
    }

    #[test]
    fn a_corrupt_record_restarts_the_count_and_rotates_the_log() {
        let mut records = log_bytes([boot(1), shutdown(COMMANDED), boot(2)]);
        records[1][1] ^= 0x01;
        let files = fake_files::FakeFiles::new();
        for record in &records {
            files.append(PATH, record).unwrap();
        }
        let started = reset_log::start(&files, PATH, CAP).unwrap();
        assert_eq!(started.info, reset::ResetInfo::first_boot());
        assert_eq!(
            started.problems,
            [
                None,
                Some(reset_log::Problem::CorruptRecord(
                    log_types::DecodeError::BadCrc
                ))
            ]
        );
        assert_eq!(files.get(PATH).as_deref(), Some(&boot(1).encode()[..]));
        assert_eq!(
            files.len("reset.log.1"),
            Ok(Some(3 * log_types::RECORD_LEN as u64))
        );

        // The next boot is not stuck on the old bad record.
        let next = reset_log::start(&files, PATH, CAP).unwrap();
        assert_eq!(next.info.count, 2);
        assert_eq!(next.problems, no_problems());
    }

    #[test]
    fn a_partial_record_and_a_corrupt_record_are_both_reported() {
        let mut records = log_bytes([boot(1), boot(2)]);
        records[0][1] ^= 0x01;
        let files = fake_files::FakeFiles::new();
        for record in &records {
            files.append(PATH, record).unwrap();
        }
        files.append(PATH, &[0xFF]).unwrap();
        let started = reset_log::start(&files, PATH, CAP).unwrap();
        assert_eq!(
            started.problems,
            [
                Some(reset_log::Problem::PartialRecord),
                Some(reset_log::Problem::CorruptRecord(
                    log_types::DecodeError::BadCrc
                ))
            ]
        );
    }

    #[test]
    fn a_log_at_the_cap_is_rotated_and_the_count_carries_forward() {
        let files = files_with(&[boot(1), shutdown(COMMANDED), boot(2), shutdown(SIGNAL)]);
        let old = files.get(PATH).unwrap();
        let started = reset_log::start(&files, PATH, 4).unwrap();
        assert_eq!(started.info.count, 3);
        assert_eq!(started.info.previous, reset::PreviousEnd::Clean(SIGNAL));
        assert_eq!(started.problems, no_problems());
        assert_eq!(files.get("reset.log.1"), Some(old));
        assert_eq!(files.get(PATH).as_deref(), Some(&boot(3).encode()[..]));
    }

    #[test]
    fn a_log_under_the_cap_is_not_rotated() {
        let files = files_with(&[boot(1), shutdown(COMMANDED), boot(2)]);
        reset_log::start(&files, PATH, 4).unwrap();
        assert_eq!(files.get("reset.log.1"), None);
        assert_eq!(files.len(PATH), Ok(Some(4 * log_types::RECORD_LEN as u64)));
    }

    #[test]
    fn stop_appends_a_shutdown_record() {
        let files = files_with(&[boot(1)]);
        reset_log::stop(&files, PATH, SIGNAL).unwrap();
        let mut expected = [0u8; 2 * log_types::RECORD_LEN];
        expected[..log_types::RECORD_LEN].copy_from_slice(&boot(1).encode());
        expected[log_types::RECORD_LEN..].copy_from_slice(&shutdown(SIGNAL).encode());
        assert_eq!(files.get(PATH).as_deref(), Some(&expected[..]));
    }

    // ---- report ----

    /// The text of one event, written the way the event service will write it. Longer than any
    /// event's text, so these tests do not depend on `EV_TEXT_MAX_BYTES`.
    type Sent = (core_event::CoreEvent, fixed_str::FixedStr<80>);

    /// Everything `report` sends, in order, and how many events that was.
    fn reported(started: &reset_log::Started) -> ([Option<Sent>; 3], usize) {
        let mut sent = [None; 3];
        let mut count = 0;
        reset_log::report(started, |code, text| {
            let mut written = fixed_str::FixedStr::EMPTY;
            write!(written, "{text}").unwrap();
            sent[count] = Some((code, written));
            count += 1;
        });
        (sent, count)
    }

    fn started(
        info: reset::ResetInfo,
        problems: [Option<reset_log::Problem>; reset_log::MAX_START_PROBLEMS],
    ) -> reset_log::Started {
        reset_log::Started { info, problems }
    }

    fn info(count: u32, previous: reset::PreviousEnd, unclean: u32) -> reset::ResetInfo {
        reset::ResetInfo {
            count,
            previous,
            consecutive_unclean: unclean,
        }
    }

    fn text_of(sent: &Option<Sent>) -> &[u8] {
        sent.as_ref().unwrap().1.as_bytes()
    }

    #[test]
    fn with_no_problems_only_the_reset_report_is_sent() {
        let started = started(info(7, reset::PreviousEnd::Clean(SIGNAL), 0), no_problems());
        let (sent, count) = reported(&started);
        assert_eq!(count, 1);
        assert_eq!(sent[0].unwrap().0, core_event::CoreEvent::ResetReport);
        assert_eq!(
            text_of(&sent[0]),
            b"reset 7: prev end signal, 0 unclean in a row"
        );
    }

    #[test]
    fn the_report_text_has_the_count_how_the_last_run_ended_and_the_unclean_count() {
        let first = started(reset::ResetInfo::first_boot(), no_problems());
        assert_eq!(
            text_of(&reported(&first).0[0]),
            b"reset 1: prev end unknown, 0 unclean in a row"
        );
        let commanded = started(
            info(12, reset::PreviousEnd::Clean(COMMANDED), 0),
            no_problems(),
        );
        assert_eq!(
            text_of(&reported(&commanded).0[0]),
            b"reset 12: prev end commanded, 0 unclean in a row"
        );
        let crashed = started(info(5, reset::PreviousEnd::Unknown, 3), no_problems());
        assert_eq!(
            text_of(&reported(&crashed).0[0]),
            b"reset 5: prev end unknown, 3 unclean in a row"
        );
    }

    #[test]
    fn each_problem_is_sent_as_its_own_event_with_its_own_text() {
        for (problem, code, text) in [
            (
                reset_log::Problem::PartialRecord,
                core_event::CoreEvent::ResetLogPartialRecord,
                &b"reset log ends in a partial record"[..],
            ),
            (
                reset_log::Problem::CorruptRecord(log_types::DecodeError::BadCrc),
                core_event::CoreEvent::ResetLogCorruptRecord,
                &b"reset log corrupt record: bad crc"[..],
            ),
            (
                reset_log::Problem::CorruptRecord(log_types::DecodeError::UnknownTag(99)),
                core_event::CoreEvent::ResetLogCorruptRecord,
                &b"reset log corrupt record: unknown tag 99"[..],
            ),
            (
                reset_log::Problem::MisorderedRecords,
                core_event::CoreEvent::ResetLogMisorderedRecords,
                &b"reset log records are out of order"[..],
            ),
            (
                reset_log::Problem::CountOverflow,
                core_event::CoreEvent::ResetLogCountOverflow,
                &b"reset log boot count is at its maximum"[..],
            ),
        ] {
            let started = started(reset::ResetInfo::first_boot(), [Some(problem), None]);
            let (sent, count) = reported(&started);
            assert_eq!(count, 2, "{problem:?}");
            assert_eq!(sent[1].unwrap().0, code, "{problem:?}");
            assert_eq!(text_of(&sent[1]), text, "{problem:?}");
        }
    }

    #[test]
    fn the_reset_report_is_first_and_problems_follow_in_the_order_start_found_them() {
        let started = started(
            reset::ResetInfo::first_boot(),
            [
                Some(reset_log::Problem::PartialRecord),
                Some(reset_log::Problem::MisorderedRecords),
            ],
        );
        let (sent, count) = reported(&started);
        assert_eq!(count, 3);
        assert_eq!(sent[0].unwrap().0, core_event::CoreEvent::ResetReport);
        assert_eq!(
            sent[1].unwrap().0,
            core_event::CoreEvent::ResetLogPartialRecord
        );
        assert_eq!(
            sent[2].unwrap().0,
            core_event::CoreEvent::ResetLogMisorderedRecords
        );
    }

    #[test]
    fn a_problem_in_the_second_slot_alone_is_still_sent() {
        let started = started(
            reset::ResetInfo::first_boot(),
            [None, Some(reset_log::Problem::CountOverflow)],
        );
        let (sent, count) = reported(&started);
        assert_eq!(count, 2);
        assert_eq!(
            sent[1].unwrap().0,
            core_event::CoreEvent::ResetLogCountOverflow
        );
    }

    #[test]
    fn report_works_on_what_start_returns() {
        // A log whose last record is half written: `start` finds the problem, `report` sends it.
        let files = files_with(&[boot(3), shutdown(COMMANDED)]);
        let mut bytes = files.get(PATH).unwrap();
        bytes.extend_from_slice(&[1, 2, 3]);
        files.put(PATH, &bytes);
        let started = reset_log::start(&files, PATH, CAP).unwrap();
        let (sent, count) = reported(&started);
        assert_eq!(count, 2);
        assert_eq!(
            text_of(&sent[0]),
            b"reset 4: prev end commanded, 0 unclean in a row"
        );
        assert_eq!(
            sent[1].unwrap().0,
            core_event::CoreEvent::ResetLogPartialRecord
        );
    }

    #[test]
    fn the_reset_report_goes_through_the_event_service_in_order() {
        let started = started(
            info(4, reset::PreviousEnd::Unknown, 1),
            [Some(reset_log::Problem::PartialRecord), None],
        );
        let mut queue = ev::Ev::new();
        reset_log::report(&started, |code, text| {
            queue.send(core_event::SOURCE, 5, code, text);
        });

        let report = queue.pop().unwrap();
        assert_eq!(report.source, core_event::SOURCE);
        assert_eq!(report.code, 0);
        assert_eq!(report.severity, event::Severity::Info);
        assert_eq!(
            report.text.as_bytes(),
            b"reset 4: prev end unknown, 1 unclean in a row"
        );
        let partial = queue.pop().unwrap();
        assert_eq!(partial.code, 1);
        assert_eq!(partial.severity, event::Severity::Warn);
        assert_eq!(queue.pop(), None);
    }
}
