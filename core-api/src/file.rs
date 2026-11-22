//! What a request to the file service looks like, and what comes back. The file service is the
//! one place that does blocking file I/O during the run: a client (the core, or a module through
//! `ctx.file`) sends an `Op`, never waits, and gets a `Response` on a later tick. There is one
//! request in flight at a time, so a response is always the answer to the one request the client
//! is waiting for, and there is nothing to match them with.
//!
//! An `Op` and a `Response` are fixed size values that are moved, not borrowed: the data is
//! inside them (`Data`), up to `FILE_DATA_MAX_BYTES` bytes. So nothing is allocated, nothing is
//! lent to the file service's thread, and a request that times out leaves no buffer stuck on the
//! other side. A file bigger than that is sent in pieces.
//!
//! The errors are the file service's, not the OS's: besides the ways a file operation fails
//! (`NotFound` and the like) they include what only the service can say (`Busy`, `TimedOut`).
//! The OS's own errors belong to the OSAL, and the core maps them to these.
//!
//! Not here yet: `Delete` and the other operations (the OSAL's `Files` cannot do them yet), and a
//! read at an offset for a transfer in pieces.

use crate::config;
use crate::fixed_str;
use core::error;
use core::fmt;

config::setting! {
    /// The most bytes of file data in one request or response. Every request is the same size, so
    /// this sets how much memory the file service's queues take (their depth times a bit more than
    /// this). A table must fit in it, and an event log batch.
    pub FILE_DATA_MAX_BYTES: usize = 4096, 1..=65536;
}

config::setting! {
    /// The most bytes in a file path. A longer path is refused (`Error::TooLarge`), never cut.
    pub FILE_PATH_MAX_BYTES: usize = 128, 1..=1024;
}

/// A file path: ASCII, and never empty. Made with `path`, which checks both.
pub type Path = fixed_str::FixedStr<FILE_PATH_MAX_BYTES>;

/// The path for `text`. The path must be ASCII, not empty, and not longer than
/// `FILE_PATH_MAX_BYTES`. Unlike an event's text, a path is never cut off: a cut off path would
/// name a different file.
pub fn path(text: &str) -> Result<Path, Error> {
    if text.is_empty() {
        return Err(Error::InvalidPath);
    }
    if text.len() > FILE_PATH_MAX_BYTES {
        return Err(Error::TooLarge);
    }
    Path::truncated(text).map_err(|_| Error::InvalidPath)
}

/// Why a file request failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The file does not exist.
    NotFound,
    /// The process is not allowed to do this to the file.
    PermissionDenied,
    /// There is no room left on the storage device.
    StorageFull,
    /// The data, or the file that was read, does not fit in `FILE_DATA_MAX_BYTES` (or a path in
    /// `FILE_PATH_MAX_BYTES`).
    TooLarge,
    /// The path is empty, or has a byte that is not ASCII.
    InvalidPath,
    /// The file service's queue is full: nothing was sent, and the client may try again later.
    Busy,
    /// No response came within the time the client waits.
    TimedOut,
    /// Any other failure.
    Other,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Error::NotFound => "file not found",
            Error::PermissionDenied => "not allowed to use the file",
            Error::StorageFull => "no storage space",
            Error::TooLarge => "too large for a file request",
            Error::InvalidPath => "not a valid file path",
            Error::Busy => "file service busy",
            Error::TimedOut => "file service timed out",
            Error::Other => "file error",
        })
    }
}

impl error::Error for Error {}

/// The bytes of a file, or part of one: at most `FILE_DATA_MAX_BYTES`, held inside the value.
///
/// The fields are private so the bytes after `len` are always zero. That is what makes the
/// derived `==` right (two `Data` are equal when their bytes are) and keeps stale bytes from
/// being sent. It is not `Copy`: it is large, and a copy should be a choice (`clone`).
#[derive(Clone, PartialEq, Eq)]
pub struct Data {
    bytes: [u8; FILE_DATA_MAX_BYTES],
    len: usize,
}

impl Data {
    /// No bytes.
    pub const EMPTY: Data = Data {
        bytes: [0; FILE_DATA_MAX_BYTES],
        len: 0,
    };

    /// A copy of `bytes`, or `TooLarge` if there are more than `FILE_DATA_MAX_BYTES`.
    pub fn new(bytes: &[u8]) -> Result<Data, Error> {
        let mut data = Data::EMPTY;
        data.add_bytes(bytes)?;
        Ok(data)
    }

    /// Adds `bytes` to the end, or fails with `TooLarge` and adds nothing if they would not all
    /// fit. This is how a batch is built one record at a time.
    pub fn add_bytes(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let end = self.len.checked_add(bytes.len()).ok_or(Error::TooLarge)?;
        let room = self.bytes.get_mut(self.len..end).ok_or(Error::TooLarge)?;
        room.copy_from_slice(bytes);
        self.len = end;
        Ok(())
    }

    /// The bytes held.
    pub fn as_slice(&self) -> &[u8] {
        self.bytes.get(..self.len).unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Default for Data {
    fn default() -> Self {
        Data::EMPTY
    }
}

/// Shows the length, not 4 kB of bytes.
impl fmt::Debug for Data {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Data({} bytes)", self.len)
    }
}

/// What to do. Each variant has the path of the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// Adds `data` to the end of the file, creating it if it is missing. The bytes are on the
    /// disk when the response comes.
    Append { path: Path, data: Data },
    /// Replaces the whole file with `data`, creating it if it is missing. If the system dies part
    /// way through, the file holds the old contents or the new, never a mix.
    WriteFile { path: Path, data: Data },
    /// Reads the whole file. The response holds it, or `TooLarge` if it does not fit in `Data`.
    ReadFile { path: Path },
}

/// The answer to a request: `Ok` holds the file's bytes for a `ReadFile` and is empty for the
/// other operations.
pub type Response = Result<Data, Error>;

#[cfg(test)]
mod tests {
    use super as file;
    use crate::fixed_str;
    use core::fmt::Write as _;

    /// What `Display` or `Debug` writes, collected in a fixed string, since a `no_std` crate has no
    /// `String`. 64 bytes is more than any text here.
    fn shown(value: &dyn core::fmt::Display) -> fixed_str::FixedStr<64> {
        let mut text = fixed_str::FixedStr::<64>::EMPTY;
        write!(text, "{value}").unwrap();
        text
    }

    #[test]
    fn data_holds_the_bytes_it_was_given() {
        let data = file::Data::new(b"hello").unwrap();
        assert_eq!(data.as_slice(), b"hello");
        assert_eq!(data.len(), 5);
        assert!(!data.is_empty());
        assert!(file::Data::EMPTY.is_empty());
        assert_eq!(file::Data::default(), file::Data::EMPTY);
    }

    #[test]
    fn data_can_be_exactly_full_and_not_more() {
        let full = [7_u8; file::FILE_DATA_MAX_BYTES];
        assert_eq!(file::Data::new(&full).unwrap().len(), full.len());
        let over = [7_u8; file::FILE_DATA_MAX_BYTES + 1];
        assert_eq!(file::Data::new(&over), Err(file::Error::TooLarge));
    }

    #[test]
    fn add_bytes_adds_to_the_end_and_one_that_does_not_fit_adds_nothing() {
        let mut data = file::Data::new(b"ab").unwrap();
        data.add_bytes(b"cd").unwrap();
        assert_eq!(data.as_slice(), b"abcd");

        let fill = [1_u8; file::FILE_DATA_MAX_BYTES - 4];
        data.add_bytes(&fill).unwrap(); // now exactly full
        assert_eq!(data.add_bytes(b"x"), Err(file::Error::TooLarge));
        assert_eq!(data.len(), file::FILE_DATA_MAX_BYTES); // unchanged
        assert_eq!(data.add_bytes(b""), Ok(())); // nothing to add always fits
    }

    #[test]
    fn data_built_in_pieces_equals_data_built_at_once() {
        let mut pieces = file::Data::EMPTY;
        pieces.add_bytes(b"ab").unwrap();
        pieces.add_bytes(b"cde").unwrap();
        assert_eq!(pieces, file::Data::new(b"abcde").unwrap());
        assert_ne!(pieces, file::Data::new(b"abcd").unwrap());
    }

    #[test]
    fn debug_shows_the_length_and_not_the_bytes() {
        let data = file::Data::new(b"hello").unwrap();
        let mut text = fixed_str::FixedStr::<64>::EMPTY;
        write!(text, "{data:?}").unwrap();
        assert_eq!(text.as_str(), "Data(5 bytes)");
    }

    #[test]
    fn a_path_is_checked_and_never_cut() {
        let path = file::path("/var/lib/vee/events.log").unwrap();
        assert_eq!(path.as_str(), "/var/lib/vee/events.log");

        assert_eq!(file::path(""), Err(file::Error::InvalidPath));
        assert_eq!(file::path("caf\u{e9}"), Err(file::Error::InvalidPath));
        assert_eq!(file::path("a\0b"), Err(file::Error::InvalidPath));

        let longest = [b'a'; file::FILE_PATH_MAX_BYTES];
        let longest = core::str::from_utf8(&longest).unwrap();
        assert_eq!(file::path(longest).unwrap().as_str(), longest);
        let too_long = [b'a'; file::FILE_PATH_MAX_BYTES + 1];
        let too_long = core::str::from_utf8(&too_long).unwrap();
        assert_eq!(file::path(too_long), Err(file::Error::TooLarge));
    }

    #[test]
    fn each_error_has_its_own_text() {
        let errors = [
            file::Error::NotFound,
            file::Error::PermissionDenied,
            file::Error::StorageFull,
            file::Error::TooLarge,
            file::Error::InvalidPath,
            file::Error::Busy,
            file::Error::TimedOut,
            file::Error::Other,
        ];
        let texts = errors.map(|error| shown(&error));
        for (index, text) in texts.iter().enumerate() {
            assert!(!text.as_str().is_empty());
            assert!(
                !texts[..index].contains(text),
                "{} is repeated",
                text.as_str()
            );
        }
        let as_error: &dyn core::error::Error = &file::Error::Busy;
        let mut text = fixed_str::FixedStr::<64>::EMPTY;
        write!(text, "{as_error}").unwrap();
        assert_eq!(text.as_str(), "file service busy");
        assert!(as_error.source().is_none());
    }

    #[test]
    fn an_op_or_a_response_is_only_a_little_bigger_than_its_data() {
        // The queues hold these by value, so their memory is this each. The slack is the path, the
        // length and the enum's tag.
        let slack = file::FILE_PATH_MAX_BYTES + 64;
        assert!(core::mem::size_of::<file::Op>() <= file::FILE_DATA_MAX_BYTES + slack);
        assert!(core::mem::size_of::<file::Response>() <= file::FILE_DATA_MAX_BYTES + slack);
    }
}
