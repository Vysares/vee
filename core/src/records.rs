//! What a log file's length says about its records. Both logs use it.

/// A log file is a run of whole fixed-size records, then maybe the start of one more that a crash
/// cut short.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    /// How many whole records the file holds.
    pub whole: u64,
    /// The file ends part way through a record.
    pub has_partial: bool,
}

/// Works out the `Shape` of a file of `len` bytes made of records of `record_len` bytes.
///
/// A `record_len` of 0 is not a real record size. It gives an empty `Shape`, since there is no
/// answer to give, and this function must not panic on it.
pub fn shape(len: u64, record_len: usize) -> Shape {
    let record_len = record_len as u64;
    Shape {
        whole: len.checked_div(record_len).unwrap_or(0),
        has_partial: len.checked_rem(record_len).is_some_and(|rest| rest != 0),
    }
}

#[cfg(test)]
mod tests {
    use super as records;

    #[test]
    fn an_empty_file_has_no_records() {
        let shape = records::shape(0, 7);
        assert_eq!(shape.whole, 0);
        assert!(!shape.has_partial);
    }

    #[test]
    fn whole_records_have_no_partial() {
        let shape = records::shape(21, 7);
        assert_eq!(shape.whole, 3);
        assert!(!shape.has_partial);
    }

    #[test]
    fn leftover_bytes_are_a_partial_record() {
        let shape = records::shape(24, 7);
        assert_eq!(shape.whole, 3);
        assert!(shape.has_partial);
    }

    #[test]
    fn less_than_one_record_is_only_a_partial() {
        let shape = records::shape(3, 7);
        assert_eq!(shape.whole, 0);
        assert!(shape.has_partial);
    }

    #[test]
    fn a_record_size_of_zero_does_not_panic() {
        let shape = records::shape(100, 0);
        assert_eq!(shape.whole, 0);
        assert!(!shape.has_partial);
    }
}
