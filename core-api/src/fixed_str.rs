//! A fixed-size ASCII text field, for event args.
//!
//! It is exactly `N` bytes, on the wire and in memory. The text is at the start and the rest is
//! zero padding, so the text ends at the first zero byte (as in cFS). That means it can never
//! hold a zero byte, and it needs no length field.

use core::fmt;
use core::fmt::Write as _;

/// Why a `FixedStr` could not be made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// A byte of the text is not ASCII, or is zero. Zero ends the text, so it cannot be in it.
    InvalidByte,
    /// `from_bytes` found a non-zero byte after the end of the text.
    NonZeroPadding,
}

/// Text bytes are ASCII 1 to 127.
fn is_text_byte(byte: u8) -> bool {
    byte != 0 && byte.is_ascii()
}

/// Always holds valid text followed by zero padding. The field is private, so the only ways to
/// make one are `EMPTY`, `truncated` and `from_bytes`, which all keep that true.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedStr<const N: usize>([u8; N]);

impl<const N: usize> FixedStr<N> {
    /// The empty string: all padding.
    pub const EMPTY: Self = FixedStr([0; N]);

    /// Copies `text`, cutting it off after `N` bytes if it is longer. The name is a reminder
    /// that text can be lost. Fails if any of `text` is invalid, including a part that would be
    /// cut off, so the result does not depend on `N`.
    pub fn truncated(text: &str) -> Result<Self, Error> {
        if !text.bytes().all(is_text_byte) {
            return Err(Error::InvalidByte);
        }
        let used = text.len().min(N);
        let mut bytes = [0u8; N];
        bytes[..used].copy_from_slice(&text.as_bytes()[..used]);
        Ok(FixedStr(bytes))
    }

    /// Rebuilds a string from its wire bytes. The text is everything before the first zero, and
    /// everything after that zero must be zero too.
    pub fn from_bytes(bytes: [u8; N]) -> Result<Self, Error> {
        let (text, padding) = bytes.split_at(text_len(&bytes));
        if !text.iter().copied().all(is_text_byte) {
            return Err(Error::InvalidByte);
        }
        if padding.iter().any(|&byte| byte != 0) {
            return Err(Error::NonZeroPadding);
        }
        Ok(FixedStr(bytes))
    }

    /// The wire bytes: the text, then zero padding, `N` bytes in all.
    pub fn to_bytes(self) -> [u8; N] {
        self.0
    }

    /// Just the text, without the padding.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0[..text_len(&self.0)]
    }
}

/// The index of the first zero byte, or the whole length if there is none.
fn text_len(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(bytes.len())
}

/// Prints the text, so it can be used in `{}` when the ground renders an event template.
impl<const N: usize> fmt::Display for FixedStr<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for &byte in self.as_bytes() {
            f.write_char(char::from(byte))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super as fixed_str;
    use core::fmt;
    use core::fmt::Write as _;

    /// Collects what `Display` writes, since a `no_std` crate has no `String`.
    struct Collector {
        bytes: [u8; 16],
        len: usize,
    }

    impl fmt::Write for Collector {
        fn write_str(&mut self, text: &str) -> fmt::Result {
            let end = self.len + text.len();
            self.bytes
                .get_mut(self.len..end)
                .ok_or(fmt::Error)?
                .copy_from_slice(text.as_bytes());
            self.len = end;
            Ok(())
        }
    }

    fn displayed<const N: usize>(text: &fixed_str::FixedStr<N>) -> ([u8; 16], usize) {
        let mut out = Collector {
            bytes: [0; 16],
            len: 0,
        };
        write!(out, "{text}").unwrap();
        (out.bytes, out.len)
    }

    #[test]
    fn empty_is_all_zero_with_no_text() {
        let text = fixed_str::FixedStr::<4>::EMPTY;
        assert_eq!(text.to_bytes(), [0; 4]);
        assert_eq!(text.as_bytes(), b"");
    }

    #[test]
    fn short_text_is_zero_padded() {
        let text = fixed_str::FixedStr::<6>::truncated("abc").unwrap();
        assert_eq!(text.to_bytes(), *b"abc\0\0\0");
        assert_eq!(text.as_bytes(), b"abc");
    }

    #[test]
    fn text_exactly_the_size_fits_with_no_padding() {
        let text = fixed_str::FixedStr::<3>::truncated("abc").unwrap();
        assert_eq!(text.to_bytes(), *b"abc");
        assert_eq!(text.as_bytes(), b"abc");
    }

    #[test]
    fn longer_text_is_cut_off() {
        let text = fixed_str::FixedStr::<3>::truncated("abcdef").unwrap();
        assert_eq!(text.as_bytes(), b"abc");
    }

    #[test]
    fn a_size_of_zero_holds_nothing() {
        let text = fixed_str::FixedStr::<0>::truncated("abc").unwrap();
        assert_eq!(text.as_bytes(), b"");
    }

    #[test]
    fn non_ascii_text_is_rejected() {
        assert_eq!(
            fixed_str::FixedStr::<8>::truncated("caf\u{e9}"),
            Err(fixed_str::Error::InvalidByte)
        );
    }

    #[test]
    fn a_bad_byte_in_the_part_that_would_be_cut_off_is_still_rejected() {
        assert_eq!(
            fixed_str::FixedStr::<2>::truncated("ab\u{e9}"),
            Err(fixed_str::Error::InvalidByte)
        );
    }

    #[test]
    fn a_zero_byte_in_the_text_is_rejected() {
        assert_eq!(
            fixed_str::FixedStr::<8>::truncated("a\0b"),
            Err(fixed_str::Error::InvalidByte)
        );
    }

    #[test]
    fn bytes_round_trip() {
        let text = fixed_str::FixedStr::<6>::truncated("abc").unwrap();
        assert_eq!(fixed_str::FixedStr::from_bytes(text.to_bytes()), Ok(text));
    }

    #[test]
    fn full_length_bytes_round_trip() {
        let bytes = *b"abc";
        let text = fixed_str::FixedStr::from_bytes(bytes).unwrap();
        assert_eq!(text.to_bytes(), bytes);
    }

    #[test]
    fn all_zero_bytes_are_the_empty_string() {
        assert_eq!(
            fixed_str::FixedStr::from_bytes([0u8; 4]),
            Ok(fixed_str::FixedStr::EMPTY)
        );
    }

    #[test]
    fn from_bytes_rejects_a_non_ascii_byte() {
        assert_eq!(
            fixed_str::FixedStr::from_bytes([b'a', 0xC3, 0, 0]),
            Err(fixed_str::Error::InvalidByte)
        );
    }

    #[test]
    fn from_bytes_rejects_text_after_the_first_zero() {
        assert_eq!(
            fixed_str::FixedStr::from_bytes([b'a', 0, b'b', 0]),
            Err(fixed_str::Error::NonZeroPadding)
        );
    }

    #[test]
    fn display_prints_only_the_text() {
        let text = fixed_str::FixedStr::<6>::truncated("abc").unwrap();
        let (bytes, len) = displayed(&text);
        assert_eq!(&bytes[..len], b"abc");
    }
}
