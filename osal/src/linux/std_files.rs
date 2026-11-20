//! The real `Files`, on top of `std::fs`.

use crate::files;
use std::fs;
use std::io;
use std::io::Read as _;
use std::io::Seek as _;
use std::io::Write as _;

/// Has no fields: the file system is the state. Create one with `StdFiles` and share it freely.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdFiles;

/// Lets `?` turn a std error into a `FileError`. The kinds that `FileError` names are kept, and
/// every other kind becomes `Other`.
impl From<io::Error> for files::FileError {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::NotFound => Self::NotFound,
            io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            io::ErrorKind::StorageFull => Self::StorageFull,
            io::ErrorKind::UnexpectedEof => Self::UnexpectedEof,
            _ => Self::Other,
        }
    }
}

impl files::Files for StdFiles {
    fn len(&self, path: &str) -> Result<Option<u64>, files::FileError> {
        match fs::metadata(path) {
            Ok(metadata) => Ok(Some(metadata.len())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn read_at(&self, path: &str, offset: u64, buf: &mut [u8]) -> Result<(), files::FileError> {
        let mut file = fs::File::open(path)?;
        file.seek(io::SeekFrom::Start(offset))?;
        file.read_exact(buf)?;
        Ok(())
    }

    fn read_file(&self, path: &str, buf: &mut [u8]) -> Result<usize, files::FileError> {
        let mut file = fs::File::open(path)?;
        let len =
            usize::try_from(file.metadata()?.len()).map_err(|_| files::FileError::TooLarge)?;
        let dest = buf.get_mut(..len).ok_or(files::FileError::TooLarge)?;
        file.read_exact(dest)?;
        Ok(len)
    }

    fn write_file(&self, path: &str, bytes: &[u8]) -> Result<(), files::FileError> {
        // Write the new contents to a side file and flush it, then rename it over the real one.
        // A rename replaces the old file in one step, so a crash leaves the old contents or the
        // new contents, never a mix.
        let side_path = format!("{path}.tmp");
        let mut side_file = fs::File::create(&side_path)?;
        side_file.write_all(bytes)?;
        side_file.sync_all()?;
        fs::rename(&side_path, path)?;
        Ok(())
    }

    fn append(&self, path: &str, bytes: &[u8]) -> Result<(), files::FileError> {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        file.write_all(bytes)?;
        file.sync_data()?;
        Ok(())
    }

    fn rotate(&self, path: &str) -> Result<(), files::FileError> {
        fs::rename(path, format!("{path}.1"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super as std_files;
    use crate::files;
    use crate::files::Files as _;
    use std::fs;
    use std::path;

    /// A new empty directory for one test, deleted when it goes out of scope. Each test passes
    /// its own name, so tests running in parallel do not share a directory.
    struct TempDir {
        dir: path::PathBuf,
    }

    /// Deletes a directory and everything in it. Cleanup failing, or the directory not being
    /// there, is not a test failure.
    #[allow(
        clippy::let_underscore_must_use,
        reason = "test cleanup: a failure to delete, or nothing to delete, does not matter"
    )]
    fn remove_dir(dir: &path::Path) {
        let _ = fs::remove_dir_all(dir);
    }

    impl TempDir {
        fn new(test_name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("vee-osal-{}-{test_name}", std::process::id()));
            remove_dir(&dir);
            fs::create_dir_all(&dir).unwrap();
            TempDir { dir }
        }

        /// The path of a file in the directory, as the `&str` that `Files` takes.
        fn path(&self, name: &str) -> String {
            self.dir.join(name).to_str().unwrap().to_string()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            remove_dir(&self.dir);
        }
    }

    #[test]
    fn missing_file_has_no_length() {
        let tmp = TempDir::new("missing_file_has_no_length");
        let real = std_files::StdFiles;
        assert_eq!(real.len(&tmp.path("log")), Ok(None));
    }

    #[test]
    fn append_creates_then_extends() {
        let tmp = TempDir::new("append_creates_then_extends");
        let real = std_files::StdFiles;
        let log = tmp.path("log");
        real.append(&log, &[1, 2]).unwrap();
        real.append(&log, &[3]).unwrap();
        assert_eq!(real.len(&log), Ok(Some(3)));
        assert_eq!(std::fs::read(&log).unwrap(), [1, 2, 3]);
    }

    #[test]
    fn read_at_reads_the_middle() {
        let tmp = TempDir::new("read_at_reads_the_middle");
        let log = tmp.path("log");
        std::fs::write(&log, [10, 11, 12, 13, 14]).unwrap();
        let mut buf = [0u8; 2];
        std_files::StdFiles.read_at(&log, 2, &mut buf).unwrap();
        assert_eq!(buf, [12, 13]);
    }

    #[test]
    fn read_at_reads_up_to_the_last_byte() {
        let tmp = TempDir::new("read_at_reads_up_to_the_last_byte");
        let log = tmp.path("log");
        std::fs::write(&log, [10, 11, 12]).unwrap();
        let mut buf = [0u8; 2];
        std_files::StdFiles.read_at(&log, 1, &mut buf).unwrap();
        assert_eq!(buf, [11, 12]);
    }

    #[test]
    fn read_past_the_end_is_unexpected_eof() {
        let tmp = TempDir::new("read_past_the_end_is_unexpected_eof");
        let log = tmp.path("log");
        std::fs::write(&log, [10, 11, 12]).unwrap();
        let mut buf = [0u8; 2];
        assert_eq!(
            std_files::StdFiles.read_at(&log, 2, &mut buf),
            Err(files::FileError::UnexpectedEof)
        );
    }

    #[test]
    fn read_at_on_a_missing_file_is_not_found() {
        let tmp = TempDir::new("read_at_on_a_missing_file_is_not_found");
        let mut buf = [0u8; 1];
        assert_eq!(
            std_files::StdFiles.read_at(&tmp.path("log"), 0, &mut buf),
            Err(files::FileError::NotFound)
        );
    }

    #[test]
    fn rotate_moves_the_file_and_frees_the_name() {
        let tmp = TempDir::new("rotate_moves_the_file_and_frees_the_name");
        let real = std_files::StdFiles;
        let log = tmp.path("log");
        std::fs::write(&log, [1, 2]).unwrap();
        real.rotate(&log).unwrap();
        assert_eq!(real.len(&log), Ok(None));
        assert_eq!(std::fs::read(tmp.path("log.1")).unwrap(), [1, 2]);
        real.append(&log, &[9]).unwrap();
        assert_eq!(std::fs::read(&log).unwrap(), [9]);
    }

    #[test]
    fn rotate_replaces_an_old_rotated_file() {
        let tmp = TempDir::new("rotate_replaces_an_old_rotated_file");
        let log = tmp.path("log");
        std::fs::write(tmp.path("log.1"), [0]).unwrap();
        std::fs::write(&log, [1, 2]).unwrap();
        std_files::StdFiles.rotate(&log).unwrap();
        assert_eq!(std::fs::read(tmp.path("log.1")).unwrap(), [1, 2]);
    }

    #[test]
    fn rotate_of_a_missing_file_is_not_found() {
        let tmp = TempDir::new("rotate_of_a_missing_file_is_not_found");
        assert_eq!(
            std_files::StdFiles.rotate(&tmp.path("log")),
            Err(files::FileError::NotFound)
        );
    }

    #[test]
    fn read_file_returns_the_length_and_fills_the_start_of_the_buffer() {
        let tmp = TempDir::new("read_file_returns_the_length");
        let tbl = tmp.path("tbl");
        std::fs::write(&tbl, [5, 6, 7]).unwrap();
        let mut buf = [0u8; 5];
        assert_eq!(std_files::StdFiles.read_file(&tbl, &mut buf), Ok(3));
        assert_eq!(buf, [5, 6, 7, 0, 0]);
    }

    #[test]
    fn read_file_that_exactly_fills_the_buffer_works() {
        let tmp = TempDir::new("read_file_that_exactly_fills_the_buffer_works");
        let tbl = tmp.path("tbl");
        std::fs::write(&tbl, [5, 6]).unwrap();
        let mut buf = [0u8; 2];
        assert_eq!(std_files::StdFiles.read_file(&tbl, &mut buf), Ok(2));
    }

    #[test]
    fn read_file_too_big_for_the_buffer_is_too_large() {
        let tmp = TempDir::new("read_file_too_big_for_the_buffer_is_too_large");
        let tbl = tmp.path("tbl");
        std::fs::write(&tbl, [5, 6, 7]).unwrap();
        let mut buf = [0u8; 2];
        assert_eq!(
            std_files::StdFiles.read_file(&tbl, &mut buf),
            Err(files::FileError::TooLarge)
        );
    }

    #[test]
    fn read_file_on_a_missing_file_is_not_found() {
        let tmp = TempDir::new("read_file_on_a_missing_file_is_not_found");
        let mut buf = [0u8; 2];
        assert_eq!(
            std_files::StdFiles.read_file(&tmp.path("tbl"), &mut buf),
            Err(files::FileError::NotFound)
        );
    }

    #[test]
    fn write_file_creates_then_replaces() {
        let tmp = TempDir::new("write_file_creates_then_replaces");
        let real = std_files::StdFiles;
        let tbl = tmp.path("tbl");
        real.write_file(&tbl, &[1, 2, 3]).unwrap();
        real.write_file(&tbl, &[9]).unwrap();
        assert_eq!(std::fs::read(&tbl).unwrap(), [9]);
    }

    #[test]
    fn write_file_leaves_no_side_file_behind() {
        let tmp = TempDir::new("write_file_leaves_no_side_file_behind");
        let tbl = tmp.path("tbl");
        std_files::StdFiles.write_file(&tbl, &[1]).unwrap();
        assert!(!std::path::Path::new(&tmp.path("tbl.tmp")).exists());
    }

    #[test]
    fn an_error_with_no_kind_of_its_own_is_other() {
        // A path "inside" a regular file fails, but not because anything is missing.
        let tmp = TempDir::new("an_error_with_no_kind_of_its_own_is_other");
        let plain_file = tmp.path("plain");
        std::fs::write(&plain_file, [0]).unwrap();
        let under_a_file = format!("{plain_file}/log");
        assert_eq!(
            std_files::StdFiles.append(&under_a_file, &[1]),
            Err(files::FileError::Other)
        );
    }

    #[test]
    fn std_error_kinds_map_to_the_matching_file_error() {
        let cases = [
            (std::io::ErrorKind::NotFound, files::FileError::NotFound),
            (
                std::io::ErrorKind::PermissionDenied,
                files::FileError::PermissionDenied,
            ),
            (
                std::io::ErrorKind::StorageFull,
                files::FileError::StorageFull,
            ),
            (
                std::io::ErrorKind::UnexpectedEof,
                files::FileError::UnexpectedEof,
            ),
            (std::io::ErrorKind::TimedOut, files::FileError::Other),
        ];
        for (kind, expected) in cases {
            assert_eq!(files::FileError::from(std::io::Error::from(kind)), expected);
        }
    }

    #[test]
    fn a_file_error_can_be_used_as_a_std_error() {
        let error: Box<dyn std::error::Error> = Box::new(files::FileError::NotFound);
        assert_eq!(error.to_string(), "entity not found");
    }
}
