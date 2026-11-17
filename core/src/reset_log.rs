//! Reading the end of the reset log to work out how the previous run ended.
//!
//! This file has no I/O. It works on records that have already been read, so it can be tested
//! with plain byte arrays.

use vee_core_api::reset::{PreviousEnd, ResetInfo};
use vee_core_types::reset_log::{DecodeError, RECORD_LEN, ResetLogEntry};

/// How many records from the end of the log are read at startup. Unclean boots have no
/// `Shutdown` record, so this covers up to this many unclean boots in a row, or half as many
/// boots if every run was clean. It also caps `consecutive_unclean`.
pub const TAIL_RECORDS: usize = 8;

/// Something wrong with the log that the caller should report. Whenever one is found, the
/// reading falls back to `ResetInfo::first_boot()`, because the history cannot be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// A record could not be decoded (bad CRC, unknown tag, and so on).
    CorruptRecord(DecodeError),
    /// The records decode, but are in an order a real log never has, such as two `Shutdown`
    /// records in a row.
    MisorderedRecords,
    /// The last boot count is the largest a `u32` can hold, so there is no next count.
    CountOverflow,
}

/// What reading the tail found: the reset info, and a problem if the log was not trustworthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TailReading {
    pub info: ResetInfo,
    pub problem: Option<Problem>,
}

/// Works out the reset info for this run from the last records of the log.
///
/// `records` are whole records, oldest first, as many as were read (normally `TAIL_RECORDS`).
/// An empty slice means a missing or empty log, which is a first boot and not a problem.
pub fn read_tail(records: &[[u8; RECORD_LEN]]) -> TailReading {
    match scan(records) {
        Ok(info) => TailReading {
            info,
            problem: None,
        },
        Err(problem) => TailReading {
            info: ResetInfo::first_boot(),
            problem: Some(problem),
        },
    }
}

fn scan(records: &[[u8; RECORD_LEN]]) -> Result<ResetInfo, Problem> {
    if records.is_empty() {
        return Ok(ResetInfo::first_boot());
    }

    let mut last_boot_count: Option<u32> = None;
    // True from a `Boot` until the `Shutdown` that closes its run.
    let mut run_open = false;
    // How the most recent run ended, as far as the records read so far say.
    let mut previous = PreviousEnd::Unknown;
    // Runs before the most recent one that ended unclean, since the last clean shutdown.
    let mut unclean_runs: u32 = 0;

    for (index, record) in records.iter().enumerate() {
        match ResetLogEntry::decode(record).map_err(Problem::CorruptRecord)? {
            ResetLogEntry::Boot { count } => {
                // A new boot while a run is still open means that run never shut down.
                if run_open {
                    unclean_runs += 1;
                }
                run_open = true;
                last_boot_count = Some(count);
                previous = PreviousEnd::Unknown;
            }
            ResetLogEntry::Shutdown { reason } => {
                // The first record read can be a `Shutdown` whose `Boot` is before the part of
                // the log that was read. Any other `Shutdown` must close a run.
                let is_first_record_read = index == 0;
                if !run_open && !is_first_record_read {
                    return Err(Problem::MisorderedRecords);
                }
                run_open = false;
                unclean_runs = 0;
                previous = PreviousEnd::Clean(reason);
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
    let consecutive_unclean = unclean_runs + u32::from(run_open);

    Ok(ResetInfo {
        count,
        previous,
        consecutive_unclean,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vee_core_api::reset::ShutdownReason;

    fn boot(count: u32) -> ResetLogEntry {
        ResetLogEntry::Boot { count }
    }

    fn shutdown(reason: ShutdownReason) -> ResetLogEntry {
        ResetLogEntry::Shutdown { reason }
    }

    const COMMANDED: ShutdownReason = ShutdownReason::Commanded;
    const SIGNAL: ShutdownReason = ShutdownReason::Signal;

    #[test]
    fn empty_log_is_a_first_boot_with_no_problem() {
        let reading = read_tail(&[]);
        assert_eq!(reading.info, ResetInfo::first_boot());
        assert_eq!(reading.problem, None);
    }

    #[test]
    fn one_boot_with_no_shutdown_is_one_unclean_run() {
        let records = [boot(1)].map(|e| e.encode());
        let reading = read_tail(&records);
        assert_eq!(
            reading.info,
            ResetInfo {
                count: 2,
                previous: PreviousEnd::Unknown,
                consecutive_unclean: 1,
            }
        );
        assert_eq!(reading.problem, None);
    }

    #[test]
    fn boot_then_shutdown_is_a_clean_end_with_its_reason() {
        let records = [boot(1), shutdown(COMMANDED)].map(|e| e.encode());
        let reading = read_tail(&records);
        assert_eq!(
            reading.info,
            ResetInfo {
                count: 2,
                previous: PreviousEnd::Clean(COMMANDED),
                consecutive_unclean: 0,
            }
        );
        assert_eq!(reading.problem, None);
    }

    #[test]
    fn a_clean_run_then_an_unclean_one_counts_one_unclean() {
        let records = [boot(1), shutdown(SIGNAL), boot(2)].map(|e| e.encode());
        let reading = read_tail(&records);
        assert_eq!(
            reading.info,
            ResetInfo {
                count: 3,
                previous: PreviousEnd::Unknown,
                consecutive_unclean: 1,
            }
        );
    }

    #[test]
    fn three_boots_in_a_row_with_no_shutdown_are_three_unclean_runs() {
        let records = [boot(1), boot(2), boot(3)].map(|e| e.encode());
        let reading = read_tail(&records);
        assert_eq!(
            reading.info,
            ResetInfo {
                count: 4,
                previous: PreviousEnd::Unknown,
                consecutive_unclean: 3,
            }
        );
    }

    #[test]
    fn a_clean_shutdown_ends_the_unclean_streak() {
        // Run 1 unclean, run 2 clean, runs 3 and 4 unclean: the streak is 2.
        let records = [boot(1), boot(2), shutdown(COMMANDED), boot(3), boot(4)].map(|e| e.encode());
        let reading = read_tail(&records);
        assert_eq!(
            reading.info,
            ResetInfo {
                count: 5,
                previous: PreviousEnd::Unknown,
                consecutive_unclean: 2,
            }
        );
    }

    #[test]
    fn a_full_window_of_unclean_boots_hits_the_cap() {
        let records: [[u8; RECORD_LEN]; TAIL_RECORDS] =
            core::array::from_fn(|i| boot(i as u32 + 10).encode());
        let reading = read_tail(&records);
        assert_eq!(reading.info.count, 10 + TAIL_RECORDS as u32);
        assert_eq!(reading.info.consecutive_unclean, TAIL_RECORDS as u32);
    }

    #[test]
    fn a_window_can_start_with_a_shutdown_whose_boot_was_not_read() {
        let records = [shutdown(SIGNAL), boot(5), shutdown(COMMANDED)].map(|e| e.encode());
        let reading = read_tail(&records);
        assert_eq!(
            reading.info,
            ResetInfo {
                count: 6,
                previous: PreviousEnd::Clean(COMMANDED),
                consecutive_unclean: 0,
            }
        );
        assert_eq!(reading.problem, None);
    }

    #[test]
    fn a_corrupt_record_falls_back_to_first_boot_and_reports_it() {
        let mut records = [boot(1), shutdown(COMMANDED), boot(2)].map(|e| e.encode());
        records[1][1] ^= 0x01;
        let reading = read_tail(&records);
        assert_eq!(reading.info, ResetInfo::first_boot());
        assert_eq!(
            reading.problem,
            Some(Problem::CorruptRecord(DecodeError::BadCrc))
        );
    }

    #[test]
    fn two_shutdowns_in_a_row_are_misordered() {
        let records = [boot(1), shutdown(COMMANDED), shutdown(COMMANDED)].map(|e| e.encode());
        let reading = read_tail(&records);
        assert_eq!(reading.info, ResetInfo::first_boot());
        assert_eq!(reading.problem, Some(Problem::MisorderedRecords));
    }

    #[test]
    fn a_lone_shutdown_has_no_boot_to_count_from() {
        let records = [shutdown(COMMANDED)].map(|e| e.encode());
        let reading = read_tail(&records);
        assert_eq!(reading.info, ResetInfo::first_boot());
        assert_eq!(reading.problem, Some(Problem::MisorderedRecords));
    }

    #[test]
    fn a_maximum_boot_count_cannot_be_incremented() {
        let records = [boot(u32::MAX)].map(|e| e.encode());
        let reading = read_tail(&records);
        assert_eq!(reading.info, ResetInfo::first_boot());
        assert_eq!(reading.problem, Some(Problem::CountOverflow));
    }
}
