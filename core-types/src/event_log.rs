//! The event log's record format.
//!
//! The log is a file of fixed-size records, one per event. Each record is laid out as:
//!
//! ```text
//! | monotonic_us (8) | source (1) | code (2) | severity (1) | text (EV_TEXT_MAX_BYTES) | crc (2) |
//! ```
//!
//! `text` is a `FixedStr`: ASCII, zero padded. The CRC covers everything before it. Multi-byte
//! numbers are big-endian.
//!
//! `Event` is defined in core-api, so the codec is a pair of free functions here, not methods.

use crate::record;
use vee_core_api::crc;
use vee_core_api::event;
use vee_core_api::fixed_str;

const TIME_OFFSET: usize = 0;
const SOURCE_OFFSET: usize = TIME_OFFSET + size_of::<u64>();
const CODE_OFFSET: usize = SOURCE_OFFSET + size_of::<u8>();
const SEVERITY_OFFSET: usize = CODE_OFFSET + size_of::<u16>();
const TEXT_OFFSET: usize = SEVERITY_OFFSET + size_of::<u8>();
const CRC_OFFSET: usize = TEXT_OFFSET + event::EV_TEXT_MAX_BYTES;

/// The size of every record in the log.
pub const RECORD_LEN: usize = CRC_OFFSET + size_of::<u16>();

// Severity codes start at 1, so a zero-filled record is never valid.
const SEVERITY_INFO: u8 = 1;
const SEVERITY_WARN: u8 = 2;
const SEVERITY_ERR: u8 = 3;

// `Severity` is defined in core-api. Its codes in the log file are part of the log format, so
// they live here.
fn severity_to_code(severity: event::Severity) -> u8 {
    match severity {
        event::Severity::Info => SEVERITY_INFO,
        event::Severity::Warn => SEVERITY_WARN,
        event::Severity::Err => SEVERITY_ERR,
    }
}

fn severity_from_code(code: u8) -> Result<event::Severity, DecodeError> {
    match code {
        SEVERITY_INFO => Ok(event::Severity::Info),
        SEVERITY_WARN => Ok(event::Severity::Warn),
        SEVERITY_ERR => Ok(event::Severity::Err),
        unknown => Err(DecodeError::UnknownSeverity(unknown)),
    }
}

/// Why a record could not be decoded. Every variant means the record cannot be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// The input is not exactly `RECORD_LEN` bytes.
    BadLength,
    /// The CRC does not match the record's contents.
    BadCrc,
    UnknownSeverity(u8),
    /// The text is not valid: a non-ASCII byte, or non-zero padding.
    BadText(fixed_str::Error),
}

/// Writes an event as one record, with its CRC.
pub fn encode(event: &event::Event) -> [u8; RECORD_LEN] {
    let mut record = [0u8; RECORD_LEN];
    record[TIME_OFFSET..SOURCE_OFFSET].copy_from_slice(&event.monotonic_us.to_be_bytes());
    record[SOURCE_OFFSET] = event.source;
    record[CODE_OFFSET..SEVERITY_OFFSET].copy_from_slice(&event.code.to_be_bytes());
    record[SEVERITY_OFFSET] = severity_to_code(event.severity);

    record[TEXT_OFFSET..CRC_OFFSET].copy_from_slice(&event.text.to_bytes());

    let crc = crc::crc16_ccitt_false(&record[..CRC_OFFSET]);
    record[CRC_OFFSET..].copy_from_slice(&crc.to_be_bytes());
    record
}

/// Reads one record. A decoded event is as good as one built live: its text was checked by
/// `FixedStr::from_bytes`.
pub fn decode(bytes: &[u8]) -> Result<event::Event, DecodeError> {
    if bytes.len() != RECORD_LEN {
        return Err(DecodeError::BadLength);
    }
    // The CRC over a record that includes its own CRC is 0 when the record is good.
    if crc::crc16_ccitt_false(bytes) != 0 {
        return Err(DecodeError::BadCrc);
    }

    let [severity_code] = record::field(bytes, SEVERITY_OFFSET, DecodeError::BadLength)?;
    let severity = severity_from_code(severity_code)?;
    let text = fixed_str::FixedStr::from_bytes(record::field(
        bytes,
        TEXT_OFFSET,
        DecodeError::BadLength,
    )?)
    .map_err(DecodeError::BadText)?;
    let monotonic_us =
        u64::from_be_bytes(record::field(bytes, TIME_OFFSET, DecodeError::BadLength)?);
    let code = u16::from_be_bytes(record::field(bytes, CODE_OFFSET, DecodeError::BadLength)?);
    let [source] = record::field(bytes, SOURCE_OFFSET, DecodeError::BadLength)?;
    Ok(event::Event {
        source,
        code,
        severity,
        monotonic_us,
        text,
    })
}

#[cfg(test)]
mod tests {
    use super as event_log;
    use vee_core_api::crc;
    use vee_core_api::event;
    use vee_core_api::fixed_str;

    fn make(severity: event::Severity, text: &str) -> event::Event {
        event::Event {
            source: 2,
            code: 0x0102,
            severity,
            monotonic_us: 0x0102_0304_0506_0708,
            text: fixed_str::FixedStr::truncated(text).unwrap(),
        }
    }

    fn info(text: &str) -> event::Event {
        make(event::Severity::Info, text)
    }

    /// Recomputes the CRC of a record whose contents a test has changed, so the test can make a
    /// record that is well formed except for that one thing.
    fn resealed(mut record: [u8; event_log::RECORD_LEN]) -> [u8; event_log::RECORD_LEN] {
        let crc = crc::crc16_ccitt_false(&record[..event_log::CRC_OFFSET]);
        record[event_log::CRC_OFFSET..].copy_from_slice(&crc.to_be_bytes());
        record
    }

    #[test]
    fn record_len_is_the_fixed_fields_plus_the_text() {
        // time 8, source 1, code 2, severity 1, text, crc 2
        assert_eq!(
            event_log::RECORD_LEN,
            8 + 1 + 2 + 1 + event::EV_TEXT_MAX_BYTES + 2
        );
    }

    #[test]
    fn events_round_trip() {
        let full = [b'x'; event::EV_TEXT_MAX_BYTES];
        let full = core::str::from_utf8(&full).unwrap();
        for ev in [info(""), info("reset 3"), info(full)] {
            assert_eq!(event_log::decode(&event_log::encode(&ev)), Ok(ev));
        }
    }

    #[test]
    fn text_that_was_cut_off_round_trips_with_its_mark() {
        let too_long = [b'x'; event::EV_TEXT_MAX_BYTES + 5];
        let ev = info(core::str::from_utf8(&too_long).unwrap());
        let read = event_log::decode(&event_log::encode(&ev)).unwrap();
        assert_eq!(read, ev);
        assert_eq!(
            read.text.as_bytes().last(),
            Some(&fixed_str::TRUNCATED_MARK)
        );
    }

    #[test]
    fn every_severity_round_trips() {
        for severity in [
            event::Severity::Info,
            event::Severity::Warn,
            event::Severity::Err,
        ] {
            let ev = make(severity, "a");
            assert_eq!(event_log::decode(&event_log::encode(&ev)), Ok(ev));
        }
    }

    #[test]
    fn the_largest_values_round_trip() {
        let ev = event::Event {
            source: u8::MAX,
            code: u16::MAX,
            severity: event::Severity::Err,
            monotonic_us: u64::MAX,
            text: fixed_str::FixedStr::EMPTY,
        };
        assert_eq!(event_log::decode(&event_log::encode(&ev)), Ok(ev));
    }

    #[test]
    fn layout_is_time_source_code_severity_text_padding_then_big_endian_crc() {
        let ev = make(event::Severity::Warn, "AB");
        let record = event_log::encode(&ev);

        let mut expected = [0u8; event_log::CRC_OFFSET];
        expected[..8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]); // monotonic_us
        expected[8] = 2; // source
        expected[9..11].copy_from_slice(&[0x01, 0x02]); // code
        expected[11] = 2; // severity: Warn
        expected[12..14].copy_from_slice(b"AB"); // text, then zero padding
        assert_eq!(record[..event_log::CRC_OFFSET], expected);

        let crc = crc::crc16_ccitt_false(&expected);
        assert_eq!(record[event_log::CRC_OFFSET..], crc.to_be_bytes());
    }

    #[test]
    fn short_input_is_rejected() {
        let record = event_log::encode(&info(""));
        assert_eq!(
            event_log::decode(&record[..event_log::RECORD_LEN - 1]),
            Err(event_log::DecodeError::BadLength)
        );
    }

    #[test]
    fn long_input_is_rejected() {
        let mut longer = [0u8; event_log::RECORD_LEN + 1];
        longer[..event_log::RECORD_LEN].copy_from_slice(&event_log::encode(&info("")));
        assert_eq!(
            event_log::decode(&longer),
            Err(event_log::DecodeError::BadLength)
        );
    }

    #[test]
    fn flipping_any_single_bit_is_rejected() {
        let record = event_log::encode(&info("abc"));
        for byte in 0..event_log::RECORD_LEN {
            for bit in 0..u8::BITS {
                let mut damaged = record;
                damaged[byte] ^= 1 << bit;
                assert_eq!(
                    event_log::decode(&damaged),
                    Err(event_log::DecodeError::BadCrc),
                    "byte {byte} bit {bit}"
                );
            }
        }
    }

    #[test]
    fn zero_filled_record_is_rejected() {
        assert_eq!(
            event_log::decode(&[0u8; event_log::RECORD_LEN]),
            Err(event_log::DecodeError::BadCrc)
        );
    }

    #[test]
    fn unknown_severity_with_good_crc_is_rejected() {
        for code in [0, 4, 99] {
            let mut record = event_log::encode(&info(""));
            record[event_log::SEVERITY_OFFSET] = code;
            assert_eq!(
                event_log::decode(&resealed(record)),
                Err(event_log::DecodeError::UnknownSeverity(code))
            );
        }
    }

    #[test]
    fn non_ascii_text_with_good_crc_is_rejected() {
        let mut record = event_log::encode(&info("ab"));
        record[event_log::TEXT_OFFSET] = 0xC3;
        assert_eq!(
            event_log::decode(&resealed(record)),
            Err(event_log::DecodeError::BadText(
                fixed_str::Error::InvalidByte
            ))
        );
    }

    #[test]
    fn nonzero_padding_with_good_crc_is_rejected() {
        let mut record = event_log::encode(&info("ab"));
        record[event_log::TEXT_OFFSET + 3] = b'x'; // after the first padding byte
        assert_eq!(
            event_log::decode(&resealed(record)),
            Err(event_log::DecodeError::BadText(
                fixed_str::Error::NonZeroPadding
            ))
        );
    }

    #[test]
    fn nonzero_padding_in_the_last_text_byte_is_rejected() {
        let mut record = event_log::encode(&info("ab"));
        record[event_log::CRC_OFFSET - 1] = b'x';
        assert_eq!(
            event_log::decode(&resealed(record)),
            Err(event_log::DecodeError::BadText(
                fixed_str::Error::NonZeroPadding
            ))
        );
    }
}
