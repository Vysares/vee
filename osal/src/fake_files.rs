//! An in-memory `Files`, so core logic can be tested without touching the disk.

use crate::files;
use std::collections;
use std::sync;

/// Files kept as byte vectors, keyed by path. Public (not test-only) because `vee-core`'s tests
/// use it too.
///
/// `Files` methods take `&self`, but this fake has state to change, so the map sits in a
/// `Mutex`. (The real implementation needs no lock: the file system is its state.)
#[derive(Debug, Default)]
pub struct FakeFiles {
    files: sync::Mutex<collections::HashMap<String, Vec<u8>>>,
}

impl FakeFiles {
    pub fn new() -> Self {
        Self::default()
    }

    /// Test setup: creates or replaces a file with exactly these bytes.
    pub fn put(&self, path: &str, bytes: &[u8]) {
        self.files
            .lock()
            .unwrap()
            .insert(path.to_string(), bytes.to_vec());
    }

    /// Test inspection: a copy of the file's bytes, or `None` if it does not exist.
    pub fn get(&self, path: &str) -> Option<Vec<u8>> {
        self.files.lock().unwrap().get(path).cloned()
    }
}

impl files::Files for FakeFiles {
    fn len(&self, path: &str) -> Result<Option<u64>, files::FileError> {
        let files = self.files.lock().unwrap();
        Ok(files.get(path).map(|bytes| bytes.len() as u64))
    }

    fn read_at(&self, path: &str, offset: u64, buf: &mut [u8]) -> Result<(), files::FileError> {
        let files = self.files.lock().unwrap();
        let bytes = files.get(path).ok_or(files::FileError::NotFound)?;
        let start = usize::try_from(offset).map_err(|_| files::FileError::UnexpectedEof)?;
        let end = start
            .checked_add(buf.len())
            .ok_or(files::FileError::UnexpectedEof)?;
        let source = bytes
            .get(start..end)
            .ok_or(files::FileError::UnexpectedEof)?;
        buf.copy_from_slice(source);
        Ok(())
    }

    fn read_file(&self, path: &str, buf: &mut [u8]) -> Result<usize, files::FileError> {
        let files = self.files.lock().unwrap();
        let bytes = files.get(path).ok_or(files::FileError::NotFound)?;
        let dest = buf
            .get_mut(..bytes.len())
            .ok_or(files::FileError::TooLarge)?;
        dest.copy_from_slice(bytes);
        Ok(bytes.len())
    }

    fn write_file(&self, path: &str, bytes: &[u8]) -> Result<(), files::FileError> {
        self.files
            .lock()
            .unwrap()
            .insert(path.to_string(), bytes.to_vec());
        Ok(())
    }

    fn append(&self, path: &str, bytes: &[u8]) -> Result<(), files::FileError> {
        self.files
            .lock()
            .unwrap()
            .entry(path.to_string())
            .or_default()
            .extend_from_slice(bytes);
        Ok(())
    }

    fn rotate(&self, path: &str) -> Result<(), files::FileError> {
        let mut files = self.files.lock().unwrap();
        // Rotating a file that does not exist is a caller bug, so it is an error, not a no-op.
        let bytes = files.remove(path).ok_or(files::FileError::NotFound)?;
        files.insert(format!("{path}.1"), bytes);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super as fake_files;
    use crate::files;
    use crate::files::Files as _;

    #[test]
    fn missing_file_has_no_length() {
        let fake = fake_files::FakeFiles::new();
        assert_eq!(fake.len("log"), Ok(None));
    }

    #[test]
    fn append_creates_then_extends() {
        let fake = fake_files::FakeFiles::new();
        fake.append("log", &[1, 2]).unwrap();
        fake.append("log", &[3]).unwrap();
        assert_eq!(fake.len("log"), Ok(Some(3)));
        assert_eq!(fake.get("log"), Some(vec![1, 2, 3]));
    }

    #[test]
    fn read_at_reads_the_middle() {
        let fake = fake_files::FakeFiles::new();
        fake.put("log", &[10, 11, 12, 13, 14]);
        let mut buf = [0u8; 2];
        fake.read_at("log", 2, &mut buf).unwrap();
        assert_eq!(buf, [12, 13]);
    }

    #[test]
    fn read_at_reads_up_to_the_last_byte() {
        let fake = fake_files::FakeFiles::new();
        fake.put("log", &[10, 11, 12]);
        let mut buf = [0u8; 2];
        fake.read_at("log", 1, &mut buf).unwrap();
        assert_eq!(buf, [11, 12]);
    }

    #[test]
    fn read_past_the_end_is_unexpected_eof() {
        let fake = fake_files::FakeFiles::new();
        fake.put("log", &[10, 11, 12]);
        let mut buf = [0u8; 2];
        assert_eq!(
            fake.read_at("log", 2, &mut buf),
            Err(files::FileError::UnexpectedEof)
        );
    }

    #[test]
    fn read_at_on_a_missing_file_is_not_found() {
        let fake = fake_files::FakeFiles::new();
        let mut buf = [0u8; 1];
        assert_eq!(
            fake.read_at("log", 0, &mut buf),
            Err(files::FileError::NotFound)
        );
    }

    #[test]
    fn rotate_moves_the_file_and_frees_the_name() {
        let fake = fake_files::FakeFiles::new();
        fake.put("log", &[1, 2]);
        fake.rotate("log").unwrap();
        assert_eq!(fake.len("log"), Ok(None));
        assert_eq!(fake.get("log.1"), Some(vec![1, 2]));
        fake.append("log", &[9]).unwrap();
        assert_eq!(fake.get("log"), Some(vec![9]));
    }

    #[test]
    fn rotate_replaces_an_old_rotated_file() {
        let fake = fake_files::FakeFiles::new();
        fake.put("log.1", &[0]);
        fake.put("log", &[1, 2]);
        fake.rotate("log").unwrap();
        assert_eq!(fake.get("log.1"), Some(vec![1, 2]));
    }

    #[test]
    fn rotate_of_a_missing_file_is_an_error() {
        let fake = fake_files::FakeFiles::new();
        assert_eq!(fake.rotate("log"), Err(files::FileError::NotFound));
    }

    #[test]
    fn read_file_returns_the_length_and_fills_the_start_of_the_buffer() {
        let fake = fake_files::FakeFiles::new();
        fake.put("tbl", &[5, 6, 7]);
        let mut buf = [0u8; 5];
        assert_eq!(fake.read_file("tbl", &mut buf), Ok(3));
        assert_eq!(buf, [5, 6, 7, 0, 0]);
    }

    #[test]
    fn read_file_that_exactly_fills_the_buffer_works() {
        let fake = fake_files::FakeFiles::new();
        fake.put("tbl", &[5, 6]);
        let mut buf = [0u8; 2];
        assert_eq!(fake.read_file("tbl", &mut buf), Ok(2));
    }

    #[test]
    fn read_file_too_big_for_the_buffer_is_too_large() {
        let fake = fake_files::FakeFiles::new();
        fake.put("tbl", &[5, 6, 7]);
        let mut buf = [0u8; 2];
        assert_eq!(
            fake.read_file("tbl", &mut buf),
            Err(files::FileError::TooLarge)
        );
    }

    #[test]
    fn read_file_on_a_missing_file_is_not_found() {
        let fake = fake_files::FakeFiles::new();
        let mut buf = [0u8; 2];
        assert_eq!(
            fake.read_file("tbl", &mut buf),
            Err(files::FileError::NotFound)
        );
    }

    #[test]
    fn write_file_creates_then_replaces() {
        let fake = fake_files::FakeFiles::new();
        fake.write_file("tbl", &[1, 2, 3]).unwrap();
        fake.write_file("tbl", &[9]).unwrap();
        assert_eq!(fake.get("tbl"), Some(vec![9]));
    }
}
