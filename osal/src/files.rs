//! The OSAL's file API. Every file call in flight code goes through this trait, whether it is
//! made directly (the core's reset log at startup and shutdown) or from the file service's thread.
//!
//! It only has the calls needed so far: the reset log's. It will grow as the file service
//! (delete, table load and dump) and the event log need more.
//!
//! The trait moves plain bytes and knows nothing about record formats. Callers decide what a
//! record is.

use core::error;
use core::fmt;

/// Why a file call failed.
///
/// The variants are named after `std::io::ErrorKind`, and mean the same thing, so code written
/// against `std::io` carries over. This is an enum of its own (not `std::io::Error`) because the
/// flight crates are `no_std`, so they cannot name std types.
///
/// Only the kinds a flight system needs are listed. Any other std error is `Other`. `TooLarge`
/// is the one variant std has no equivalent for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileError {
    /// The file does not exist. (`Files::len` is the exception: it reports a missing file as `None`.)
    NotFound,
    /// The process is not allowed to do this to the file.
    PermissionDenied,
    /// There is no room left on the storage device.
    StorageFull,
    /// A read needed more bytes than the file has left. Same as std's `read_exact` failure.
    UnexpectedEof,
    /// A failure with no kind of its own above.
    Other,
    /// `read_file` found a file bigger than the buffer it was given. Not a std kind.
    TooLarge,
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            FileError::NotFound => "entity not found",
            FileError::PermissionDenied => "permission denied",
            FileError::StorageFull => "no storage space",
            FileError::UnexpectedEof => "unexpected end of file",
            FileError::Other => "other error",
            FileError::TooLarge => "file too large for the buffer",
        })
    }
}

impl error::Error for FileError {}

/// Every method takes `&self`: the real implementation has no state of its own (the file system
/// is the state), so it can be shared freely between the core and the file service's thread.
pub trait Files {
    /// The file's length in bytes, or `None` if the file does not exist (a first boot).
    fn len(&self, path: &str) -> Result<Option<u64>, FileError>;

    /// Fills all of `buf` from the file, starting `offset` bytes in.
    /// Fails with `NotFound` if the file is missing, or `UnexpectedEof` if it ends before `buf` is full.
    fn read_at(&self, path: &str, offset: u64, buf: &mut [u8]) -> Result<(), FileError>;

    /// Reads the whole file into the start of `buf` and returns how many bytes it held.
    /// Fails with `NotFound` if the file is missing, or `TooLarge` if it does not fit in `buf`.
    fn read_file(&self, path: &str, buf: &mut [u8]) -> Result<usize, FileError>;

    /// Replaces the whole contents of the file with `bytes`, creating it if it is missing. The
    /// bytes have been flushed to the disk when this returns. If the system dies part way
    /// through, the file holds the old contents or the new contents, never a mix.
    fn write_file(&self, path: &str, bytes: &[u8]) -> Result<(), FileError>;

    /// Adds `bytes` to the end of the file, creating it if it is missing. The bytes have been
    /// flushed to the disk when this returns.
    fn append(&self, path: &str, bytes: &[u8]) -> Result<(), FileError>;

    /// Renames `path` to `path.1`, replacing any old `path.1`. The original name is then free,
    /// so the next `append` starts a new file.
    fn rotate(&self, path: &str) -> Result<(), FileError>;
}

#[cfg(test)]
mod tests {
    use super as files;

    #[test]
    fn each_file_error_has_its_own_text() {
        let texts = [
            (files::FileError::NotFound, "entity not found"),
            (files::FileError::PermissionDenied, "permission denied"),
            (files::FileError::StorageFull, "no storage space"),
            (files::FileError::UnexpectedEof, "unexpected end of file"),
            (files::FileError::Other, "other error"),
            (files::FileError::TooLarge, "file too large for the buffer"),
        ];
        for (error, text) in texts {
            assert_eq!(error.to_string(), text);
        }
    }
}
