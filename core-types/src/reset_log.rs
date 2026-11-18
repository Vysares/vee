//! The reset log's record format.
//!
//! The log is a file of fixed-size records. Each record is laid out as:
//!
//! ```text
//! | tag (1) | payload (4, zero padded) | crc (2, big-endian) |
//! ```
//!
//! The tag says which `ResetLogEntry` variant the record is. The payload is that variant's
//! fields, zero padded up to the size of the largest variant. The CRC covers the tag and the
//! payload. Multi-byte numbers are big-endian.

use vee_core_api::crc;
use vee_core_api::reset;

const TAG_OFFSET: usize = 0;
const PAYLOAD_OFFSET: usize = 1;
/// The size of the largest variant's fields: a `Boot`'s count.
const PAYLOAD_LEN: usize = size_of::<u32>();
const CRC_OFFSET: usize = PAYLOAD_OFFSET + PAYLOAD_LEN;
const CRC_LEN: usize = size_of::<u16>();

/// The size of every record in the log. The log reader uses this to find records from the end
/// of the file.
pub const RECORD_LEN: usize = CRC_OFFSET + CRC_LEN;

// Tags and reason codes start at 1, so a zero-filled record is never valid.
const TAG_BOOT: u8 = 1;
const TAG_SHUTDOWN: u8 = 2;

const REASON_COMMANDED: u8 = 1;
const REASON_SIGNAL: u8 = 2;

// `ShutdownReason` is defined in core-api, because modules need it too. Its codes in the log
// file are part of the log format, so they live here.
fn shutdown_reason_to_code(reason: reset::ShutdownReason) -> u8 {
    match reason {
        reset::ShutdownReason::Commanded => REASON_COMMANDED,
        reset::ShutdownReason::Signal => REASON_SIGNAL,
    }
}

fn shutdown_reason_from_code(code: u8) -> Result<reset::ShutdownReason, DecodeError> {
    match code {
        REASON_COMMANDED => Ok(reset::ShutdownReason::Commanded),
        REASON_SIGNAL => Ok(reset::ShutdownReason::Signal),
        unknown => Err(DecodeError::UnknownReason(unknown)),
    }
}

/// One record in the reset log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetLogEntry {
    /// Appended at startup. Only boots are counted.
    Boot { count: u32 },
    /// Appended on a clean shutdown. It belongs to the most recent `Boot` before it.
    Shutdown { reason: reset::ShutdownReason },
}

/// Why a record could not be decoded. Every variant means the record cannot be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// The input is not exactly `RECORD_LEN` bytes.
    BadLength,
    /// The CRC does not match the record's contents.
    BadCrc,
    UnknownTag(u8),
    UnknownReason(u8),
    /// The padding after a variant's fields is not all zero.
    NonZeroPadding,
}

impl ResetLogEntry {
    pub fn encode(&self) -> [u8; RECORD_LEN] {
        let mut record = [0u8; RECORD_LEN];
        match self {
            ResetLogEntry::Boot { count } => {
                record[TAG_OFFSET] = TAG_BOOT;
                record[PAYLOAD_OFFSET..CRC_OFFSET].copy_from_slice(&count.to_be_bytes());
            }
            ResetLogEntry::Shutdown { reason } => {
                record[TAG_OFFSET] = TAG_SHUTDOWN;
                record[PAYLOAD_OFFSET] = shutdown_reason_to_code(*reason);
            }
        }
        let crc = crc::crc16_ccitt_false(&record[..CRC_OFFSET]);
        record[CRC_OFFSET..].copy_from_slice(&crc.to_be_bytes());
        record
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() != RECORD_LEN {
            return Err(DecodeError::BadLength);
        }
        // The CRC over a record that includes its own CRC is 0 when the record is good.
        if crc::crc16_ccitt_false(bytes) != 0 {
            return Err(DecodeError::BadCrc);
        }

        let payload = &bytes[PAYLOAD_OFFSET..CRC_OFFSET];
        match bytes[TAG_OFFSET] {
            TAG_BOOT => {
                let count = u32::from_be_bytes(core::array::from_fn(|i| payload[i]));
                Ok(ResetLogEntry::Boot { count })
            }
            TAG_SHUTDOWN => {
                let reason = shutdown_reason_from_code(payload[0])?;
                let padding = &payload[1..];
                if padding.iter().any(|&byte| byte != 0) {
                    return Err(DecodeError::NonZeroPadding);
                }
                Ok(ResetLogEntry::Shutdown { reason })
            }
            unknown => Err(DecodeError::UnknownTag(unknown)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super as reset_log;
    use vee_core_api::crc;
    use vee_core_api::reset;

    /// Builds a record with a correct CRC from a tag and payload, so tests can make records that
    /// are well formed except for one thing.
    fn sealed_record(
        tag: u8,
        payload: [u8; reset_log::PAYLOAD_LEN],
    ) -> [u8; reset_log::RECORD_LEN] {
        let mut record = [0u8; reset_log::RECORD_LEN];
        record[reset_log::TAG_OFFSET] = tag;
        record[reset_log::PAYLOAD_OFFSET..reset_log::CRC_OFFSET].copy_from_slice(&payload);
        let crc = crc::crc16_ccitt_false(&record[..reset_log::CRC_OFFSET]);
        record[reset_log::CRC_OFFSET..].copy_from_slice(&crc.to_be_bytes());
        record
    }

    #[test]
    fn record_len_is_seven() {
        assert_eq!(reset_log::RECORD_LEN, 7);
    }

    #[test]
    fn boot_round_trips() {
        for count in [0, 1, 41, u32::MAX] {
            let entry = reset_log::ResetLogEntry::Boot { count };
            assert_eq!(reset_log::ResetLogEntry::decode(&entry.encode()), Ok(entry));
        }
    }

    #[test]
    fn shutdown_round_trips_for_every_reason() {
        for reason in [
            reset::ShutdownReason::Commanded,
            reset::ShutdownReason::Signal,
        ] {
            let entry = reset_log::ResetLogEntry::Shutdown { reason };
            assert_eq!(reset_log::ResetLogEntry::decode(&entry.encode()), Ok(entry));
        }
    }

    #[test]
    fn boot_layout_is_tag_then_big_endian_count() {
        let record = reset_log::ResetLogEntry::Boot { count: 0x0102_0304 }.encode();
        assert_eq!(
            &record[..reset_log::CRC_OFFSET],
            &[reset_log::TAG_BOOT, 0x01, 0x02, 0x03, 0x04]
        );
    }

    #[test]
    fn shutdown_layout_is_tag_then_reason_then_zero_padding() {
        let record = reset_log::ResetLogEntry::Shutdown {
            reason: reset::ShutdownReason::Signal,
        }
        .encode();
        assert_eq!(
            &record[..reset_log::CRC_OFFSET],
            &[reset_log::TAG_SHUTDOWN, reset_log::REASON_SIGNAL, 0, 0, 0]
        );
    }

    #[test]
    fn short_input_is_rejected() {
        let record = reset_log::ResetLogEntry::Boot { count: 1 }.encode();
        assert_eq!(
            reset_log::ResetLogEntry::decode(&record[..reset_log::RECORD_LEN - 1]),
            Err(reset_log::DecodeError::BadLength)
        );
    }

    #[test]
    fn long_input_is_rejected() {
        let mut longer = [0u8; reset_log::RECORD_LEN + 1];
        longer[..reset_log::RECORD_LEN]
            .copy_from_slice(&reset_log::ResetLogEntry::Boot { count: 1 }.encode());
        assert_eq!(
            reset_log::ResetLogEntry::decode(&longer),
            Err(reset_log::DecodeError::BadLength)
        );
    }

    #[test]
    fn flipping_any_single_bit_is_rejected() {
        let record = reset_log::ResetLogEntry::Boot { count: 1 }.encode();
        for byte in 0..reset_log::RECORD_LEN {
            for bit in 0..u8::BITS {
                let mut damaged = record;
                damaged[byte] ^= 1 << bit;
                assert_eq!(
                    reset_log::ResetLogEntry::decode(&damaged),
                    Err(reset_log::DecodeError::BadCrc),
                    "byte {byte}, bit {bit}"
                );
            }
        }
    }

    #[test]
    fn zero_filled_record_is_rejected() {
        assert_eq!(
            reset_log::ResetLogEntry::decode(&[0u8; reset_log::RECORD_LEN]),
            Err(reset_log::DecodeError::BadCrc)
        );
    }

    #[test]
    fn unknown_tag_with_good_crc_is_rejected() {
        let record = sealed_record(99, [0; reset_log::PAYLOAD_LEN]);
        assert_eq!(
            reset_log::ResetLogEntry::decode(&record),
            Err(reset_log::DecodeError::UnknownTag(99))
        );
    }

    #[test]
    fn unknown_reason_with_good_crc_is_rejected() {
        let record = sealed_record(reset_log::TAG_SHUTDOWN, [99, 0, 0, 0]);
        assert_eq!(
            reset_log::ResetLogEntry::decode(&record),
            Err(reset_log::DecodeError::UnknownReason(99))
        );
    }

    #[test]
    fn nonzero_padding_with_good_crc_is_rejected() {
        let record = sealed_record(
            reset_log::TAG_SHUTDOWN,
            [reset_log::REASON_COMMANDED, 0, 7, 0],
        );
        assert_eq!(
            reset_log::ResetLogEntry::decode(&record),
            Err(reset_log::DecodeError::NonZeroPadding)
        );
    }
}
