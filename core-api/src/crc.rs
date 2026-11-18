//! CRC-16/CCITT-FALSE: polynomial 0x1021, initial value 0xFFFF, no input or output reflection,
//! no final XOR. This is the CRC CCSDS uses for packet error control.

const POLYNOMIAL: u16 = 0x1021;
const INITIAL_VALUE: u16 = 0xFFFF;
const TOP_BIT: u16 = 0x8000;

const TABLE_LEN: usize = u8::MAX as usize + 1;

/// What the register becomes after shifting `byte` (placed in its top 8 bits) out one bit at a
/// time. Each time a 1 falls off the top, the polynomial is folded in.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "runs at compile time, where an overflow would be a build error"
)]
const fn crc_of_one_byte(byte: u8) -> u16 {
    let mut crc = (byte as u16) << 8;
    let mut bit = 0;
    while bit < u8::BITS {
        if crc & TOP_BIT != 0 {
            crc = (crc << 1) ^ POLYNOMIAL;
        } else {
            crc <<= 1;
        }
        bit += 1;
    }
    crc
}

/// One entry for each possible byte value. Built by the compiler, so it is read-only data in
/// the binary: 512 bytes, with no code running at startup.
#[allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    reason = "runs at compile time (an overflow would be a build error), and `value < TABLE_LEN` is the loop condition, so `value` fits a u8"
)]
const fn build_table() -> [u16; TABLE_LEN] {
    let mut table = [0u16; TABLE_LEN];
    let mut value = 0;
    while value < TABLE_LEN {
        table[value] = crc_of_one_byte(value as u8);
        value += 1;
    }
    table
}

static TABLE: [u16; TABLE_LEN] = build_table();

/// The one place the table is indexed. `TABLE_LEN` is `u8::MAX + 1`, so every `u8` is in range
/// and the index cannot fail. Taking a `u8` (not a `usize`) is what makes that true.
#[allow(
    clippy::indexing_slicing,
    reason = "a u8 index is always below TABLE_LEN, which is u8::MAX + 1"
)]
fn table_entry(index: u8) -> u16 {
    TABLE[usize::from(index)]
}

/// Computes the CRC over `data`.
///
/// To check a record that ends with its own CRC (stored big-endian), compute the CRC over the
/// whole record, CRC bytes included. A good record gives 0.
pub fn crc16_ccitt_false(data: &[u8]) -> u16 {
    let mut crc = INITIAL_VALUE;
    for &byte in data {
        // The register's top byte mixed with the input byte decides which polynomial
        // multiples get folded in. The table has that result for all 256 values.
        // The register's low byte shifts up to become the new top byte.
        let index = (crc >> 8) as u8 ^ byte;
        crc = (crc << 8) ^ table_entry(index);
    }
    crc
}

#[cfg(test)]
mod tests {
    use super as crc;

    /// The standard check value for CRC-16/CCITT-FALSE, from the CRC catalogue.
    #[test]
    fn check_value() {
        assert_eq!(crc::crc16_ccitt_false(b"123456789"), 0x29B1);
    }

    #[test]
    fn empty_input_is_the_initial_value() {
        assert_eq!(crc::crc16_ccitt_false(&[]), crc::INITIAL_VALUE);
    }

    #[test]
    fn single_byte() {
        assert_eq!(crc::crc16_ccitt_false(b"A"), 0xB915);
    }

    #[test]
    fn data_followed_by_its_own_crc_gives_zero() {
        let data = b"123456789";
        let crc = crc::crc16_ccitt_false(data);
        let mut record = [0u8; 11];
        record[..9].copy_from_slice(data);
        record[9..].copy_from_slice(&crc.to_be_bytes());
        assert_eq!(crc::crc16_ccitt_false(&record), 0);
    }

    /// The plain bit-by-bit algorithm, with no table. Slow, but easy to check against the
    /// definition of the CRC, so the table version is tested against it.
    fn bitwise_reference(data: &[u8]) -> u16 {
        let mut crc = crc::INITIAL_VALUE;
        for &byte in data {
            crc ^= u16::from(byte) << 8;
            for _ in 0..u8::BITS {
                if crc & crc::TOP_BIT != 0 {
                    crc = (crc << 1) ^ crc::POLYNOMIAL;
                } else {
                    crc <<= 1;
                }
            }
        }
        crc
    }

    #[test]
    fn table_version_matches_bitwise_reference_for_every_single_byte() {
        for value in 0..=u8::MAX {
            assert_eq!(
                crc::crc16_ccitt_false(&[value]),
                bitwise_reference(&[value])
            );
        }
    }

    #[test]
    fn table_version_matches_bitwise_reference_for_a_long_input() {
        let mut data = [0u8; 1024];
        // A repeating pattern of bytes that wraps round at 256.
        let mut next: u8 = 7;
        for byte in &mut data {
            *byte = next;
            next = next.wrapping_add(31);
        }
        assert_eq!(crc::crc16_ccitt_false(&data), bitwise_reference(&data));
    }

    #[test]
    fn a_flipped_bit_is_detected() {
        let mut data = *b"123456789";
        let good = crc::crc16_ccitt_false(&data);
        data[4] ^= 0x01;
        assert_ne!(crc::crc16_ccitt_false(&data), good);
    }
}
