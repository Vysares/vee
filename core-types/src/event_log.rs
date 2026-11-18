//! The event log's record format.
//!
//! The log is a file of fixed-size records, one per event. Each record is laid out as:
//!
//! ```text
//! | monotonic_us (8) | source (1) | code (2) | severity (1) | args_len (1) | args (32) | crc (2) |
//! ```
//!
//! `args` is zero padded after its first `args_len` bytes. The CRC covers everything before it.
//! Multi-byte numbers are big-endian.
//!
//! `Event` is defined in core-api, so the codec is a pair of free functions here, not methods.

use vee_core_api::crc;
use vee_core_api::event;

const TIME_OFFSET: usize = 0;
const SOURCE_OFFSET: usize = TIME_OFFSET + size_of::<u64>();
const CODE_OFFSET: usize = SOURCE_OFFSET + size_of::<u8>();
const SEVERITY_OFFSET: usize = CODE_OFFSET + size_of::<u16>();
const ARGS_LEN_OFFSET: usize = SEVERITY_OFFSET + size_of::<u8>();
const ARGS_OFFSET: usize = ARGS_LEN_OFFSET + size_of::<u8>();
const CRC_OFFSET: usize = ARGS_OFFSET + event::EV_ARGS_MAX_BYTES;

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
    /// `args_len` is more than `EV_ARGS_MAX_BYTES`.
    ArgsLenTooLong(u8),
    /// The padding after the used args is not all zero.
    NonZeroPadding,
}

/// Writes an event as one record, with its CRC.
pub fn encode(event: &event::Event) -> [u8; RECORD_LEN] {
    let mut record = [0u8; RECORD_LEN];
    record[TIME_OFFSET..SOURCE_OFFSET].copy_from_slice(&event.monotonic_us.to_be_bytes());
    record[SOURCE_OFFSET] = event.source;
    record[CODE_OFFSET..SEVERITY_OFFSET].copy_from_slice(&event.code.to_be_bytes());
    record[SEVERITY_OFFSET] = severity_to_code(event.severity);

    let args = event.args();
    // Fits: an `Event` never has more than `EV_ARGS_MAX_BYTES` args, and that is at most 255.
    record[ARGS_LEN_OFFSET] = args.len() as u8;
    // The rest of the args area stays zero, which is the padding.
    record[ARGS_OFFSET..ARGS_OFFSET + args.len()].copy_from_slice(args);

    let crc = crc::crc16_ccitt_false(&record[..CRC_OFFSET]);
    record[CRC_OFFSET..].copy_from_slice(&crc.to_be_bytes());
    record
}

/// Reads one record. The checks are the ones `Event::new` relies on, so a decoded event is as
/// good as one built live.
pub fn decode(bytes: &[u8]) -> Result<event::Event, DecodeError> {
    if bytes.len() != RECORD_LEN {
        return Err(DecodeError::BadLength);
    }
    // The CRC over a record that includes its own CRC is 0 when the record is good.
    if crc::crc16_ccitt_false(bytes) != 0 {
        return Err(DecodeError::BadCrc);
    }

    let severity = severity_from_code(bytes[SEVERITY_OFFSET])?;

    let args_len = bytes[ARGS_LEN_OFFSET];
    let (args, padding) = bytes[ARGS_OFFSET..CRC_OFFSET]
        .split_at_checked(usize::from(args_len))
        .ok_or(DecodeError::ArgsLenTooLong(args_len))?;
    if padding.iter().any(|&byte| byte != 0) {
        return Err(DecodeError::NonZeroPadding);
    }

    let monotonic_us = u64::from_be_bytes(core::array::from_fn(|i| bytes[TIME_OFFSET + i]));
    let code = u16::from_be_bytes(core::array::from_fn(|i| bytes[CODE_OFFSET + i]));
    // The length was checked above, so this cannot fail. If it somehow did, the record is
    // reported as bad, not trusted.
    event::Event::new(bytes[SOURCE_OFFSET], code, severity, monotonic_us, args)
        .map_err(|event::Error::ArgsTooLong| DecodeError::ArgsLenTooLong(args_len))
}

#[cfg(test)]
mod tests {
    use super as event_log;
    use vee_core_api::crc;
    use vee_core_api::event;

    fn make(severity: event::Severity, args: &[u8]) -> event::Event {
        event::Event::new(2, 0x0102, severity, 0x0102_0304_0506_0708, args).unwrap()
    }

    fn info(args: &[u8]) -> event::Event {
        make(event::Severity::Info, args)
    }

    /// Recomputes the CRC of a record whose contents a test has changed, so the test can make a
    /// record that is well formed except for that one thing.
    fn resealed(mut record: [u8; event_log::RECORD_LEN]) -> [u8; event_log::RECORD_LEN] {
        let crc = crc::crc16_ccitt_false(&record[..event_log::CRC_OFFSET]);
        record[event_log::CRC_OFFSET..].copy_from_slice(&crc.to_be_bytes());
        record
    }

    #[test]
    fn record_len_is_forty_seven() {
        assert_eq!(event_log::RECORD_LEN, 47);
    }

    #[test]
    fn events_round_trip() {
        let full = [9u8; event::EV_ARGS_MAX_BYTES];
        for ev in [info(&[]), info(&[1, 2, 3]), info(&full)] {
            assert_eq!(event_log::decode(&event_log::encode(&ev)), Ok(ev));
        }
    }

    #[test]
    fn every_severity_round_trips() {
        for severity in [
            event::Severity::Info,
            event::Severity::Warn,
            event::Severity::Err,
        ] {
            let ev = make(severity, &[1]);
            assert_eq!(event_log::decode(&event_log::encode(&ev)), Ok(ev));
        }
    }

    #[test]
    fn the_largest_values_round_trip() {
        let ev = event::Event::new(u8::MAX, u16::MAX, event::Severity::Err, u64::MAX, &[]).unwrap();
        assert_eq!(event_log::decode(&event_log::encode(&ev)), Ok(ev));
    }

    #[test]
    fn layout_is_time_source_code_severity_len_args_padding_then_big_endian_crc() {
        let ev = make(event::Severity::Warn, &[0xAA, 0xBB]);
        let record = event_log::encode(&ev);

        let mut expected = [0u8; event_log::CRC_OFFSET];
        expected[..8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]); // monotonic_us
        expected[8] = 2; // source
        expected[9..11].copy_from_slice(&[0x01, 0x02]); // code
        expected[11] = 2; // severity: Warn
        expected[12] = 2; // args_len
        expected[13..15].copy_from_slice(&[0xAA, 0xBB]); // args, then zero padding
        assert_eq!(record[..event_log::CRC_OFFSET], expected);

        let crc = crc::crc16_ccitt_false(&expected);
        assert_eq!(record[event_log::CRC_OFFSET..], crc.to_be_bytes());
    }

    #[test]
    fn short_input_is_rejected() {
        let record = event_log::encode(&info(&[]));
        assert_eq!(
            event_log::decode(&record[..event_log::RECORD_LEN - 1]),
            Err(event_log::DecodeError::BadLength)
        );
    }

    #[test]
    fn long_input_is_rejected() {
        let mut longer = [0u8; event_log::RECORD_LEN + 1];
        longer[..event_log::RECORD_LEN].copy_from_slice(&event_log::encode(&info(&[])));
        assert_eq!(
            event_log::decode(&longer),
            Err(event_log::DecodeError::BadLength)
        );
    }

    #[test]
    fn flipping_any_single_bit_is_rejected() {
        let record = event_log::encode(&info(&[1, 2, 3]));
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
            let mut record = event_log::encode(&info(&[]));
            record[event_log::SEVERITY_OFFSET] = code;
            assert_eq!(
                event_log::decode(&resealed(record)),
                Err(event_log::DecodeError::UnknownSeverity(code))
            );
        }
    }

    #[test]
    fn args_len_over_the_maximum_with_good_crc_is_rejected() {
        for len in [event::EV_ARGS_MAX_BYTES as u8 + 1, u8::MAX] {
            let mut record = event_log::encode(&info(&[]));
            record[event_log::ARGS_LEN_OFFSET] = len;
            assert_eq!(
                event_log::decode(&resealed(record)),
                Err(event_log::DecodeError::ArgsLenTooLong(len))
            );
        }
    }

    #[test]
    fn nonzero_padding_with_good_crc_is_rejected() {
        let mut record = event_log::encode(&info(&[1, 2]));
        record[event_log::ARGS_OFFSET + 2] = 1; // the first padding byte
        assert_eq!(
            event_log::decode(&resealed(record)),
            Err(event_log::DecodeError::NonZeroPadding)
        );
    }

    #[test]
    fn nonzero_padding_in_the_last_args_byte_is_rejected() {
        let mut record = event_log::encode(&info(&[1, 2]));
        record[event_log::CRC_OFFSET - 1] = 1;
        assert_eq!(
            event_log::decode(&resealed(record)),
            Err(event_log::DecodeError::NonZeroPadding)
        );
    }
}
