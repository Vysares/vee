//! Helper shared by the log record decoders.

/// Reads the `N` bytes that start at `offset`, as an array that `from_be_bytes` and
/// `FixedStr::from_bytes` take directly. If the record is too short, returns `too_short`.
///
/// `N` is not written at the call: it comes from what the array is used for. For example
/// `u64::from_be_bytes(record::field(bytes, offset, err)?)` reads 8 bytes. A decoder checks the
/// whole length first, so `too_short` should not happen, but the decoder still has an error for
/// it instead of a panic if its offsets are ever wrong.
pub fn field<const N: usize, E>(record: &[u8], offset: usize, too_short: E) -> Result<[u8; N], E> {
    record
        .get(offset..)
        .and_then(|rest| rest.first_chunk::<N>())
        .copied()
        .ok_or(too_short)
}

#[cfg(test)]
mod tests {
    use super as record;

    #[test]
    fn reads_the_bytes_at_the_offset() {
        let bytes = [10, 11, 12, 13, 14];
        assert_eq!(record::field(&bytes, 1, "short"), Ok([11, 12]));
        assert_eq!(record::field(&bytes, 0, "short"), Ok([10, 11, 12, 13, 14]));
        assert_eq!(record::field(&bytes, 4, "short"), Ok([14]));
    }

    #[test]
    fn too_short_gives_the_error() {
        let bytes = [10, 11, 12];
        assert_eq!(record::field::<2, _>(&bytes, 2, "short"), Err("short"));
        assert_eq!(record::field::<1, _>(&bytes, 3, "short"), Err("short"));
        assert_eq!(record::field::<1, _>(&bytes, 99, "short"), Err("short"));
    }
}
