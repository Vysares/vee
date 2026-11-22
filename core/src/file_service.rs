//! The file service: it does the blocking file I/O during the run, on a worker thread, so nothing
//! on the main thread ever waits on a disk. A client sends an `Op` (`submit`), carries on, and
//! collects the `Response` on a later tick (`poll`). The types of the exchange are in
//! `vee_core_api::file`; this is the part that runs them against the OSAL's `Files`.
//!
//! ONE request is in flight at a time, so each queue has one slot, and a response is always the
//! answer to the request the client is waiting for. There is one service, with its own worker
//! thread, for each client (the event log writer, tables, each module that uses files). More at
//! once comes from more services. A file should have one client that writes it, because between
//! clients there is no order.
//!
//! Who calls what:
//! - The executive starts one for each client, once, at startup, before the allocation guard is
//!   armed (`start` allocates: the queues, and the thread).
//! - The client calls `submit` and `poll` on the main thread, through a `file_device::FileDevice`,
//!   which keeps the rule of one at a time and the timeout. Neither waits. (`wait` is the one call
//!   that does, and only the shutdown uses it.)
//! - The worker thread (`work`) takes a request out of the queue, runs it (`handle`), and puts the
//!   response in the other queue. It waits only for the next request.
//!
//! Each slot takes about `FILE_DATA_MAX_BYTES` of memory, however small the data. A response that
//! cannot be queued is a bug (the client sent a second request before taking the first response,
//! which `FileDevice` does not allow): the worker drops it and says so in the system log.

use vee_core_api::file;
use vee_osal::channel;
use vee_osal::files;
use vee_osal::thread;

/// The main thread's side of the file service.
///
/// Dropping it closes the request queue, which makes the worker finish what is queued and then
/// end.
pub struct FileService {
    requests: channel::Sender<file::Op>,
    responses: channel::Receiver<file::Response>,
}

impl FileService {
    /// Starts the worker thread, which does its file operations on `files`. Give it its own
    /// handle to the files, not the one the core uses at startup and shutdown (two handles to the
    /// same disk are fine: `StdFiles` is the disk itself, and a fake's `clone` shares its files).
    ///
    /// Allocates (the queues and the thread), so it is called at startup. Fails, as a value, if
    /// the OS refuses a thread.
    pub fn start<F>(files: F) -> Result<Self, thread::SpawnError>
    where
        F: files::Files + Send + 'static,
    {
        let (request_sender, request_receiver) = channel::channel(1);
        let (response_sender, response_receiver) = channel::channel(1);
        thread::spawn("vee-file", move || {
            work(&files, &request_receiver, &response_sender);
        })?;
        Ok(FileService {
            requests: request_sender,
            responses: response_receiver,
        })
    }

    /// Queues `op` and returns at once. If the queue is full (or the worker is gone), the `op`
    /// comes back in the `Err`, and the client decides what to do (usually to tell its own caller
    /// `Busy`).
    #[allow(
        clippy::result_large_err,
        reason = "the op comes back by value on purpose, like `Queue::push`: the caller still \
                  owns it and may try again, and an op is meant to be moved, not boxed"
    )]
    pub fn submit(&self, op: file::Op) -> Result<(), file::Op> {
        self.requests.try_send(op)
    }

    /// The response that has come back, or `None` if there is none. Returns at once.
    pub fn poll(&self) -> Option<file::Response> {
        self.responses.try_recv()
    }

    /// The response, waiting for it for at most `timeout_us` microseconds, and then `None`. This
    /// is the one call here that waits, for the shutdown, which must not go on before the worker
    /// has finished what it was given, and must not wait for ever for a disk that has stopped.
    pub fn wait(&self, timeout_us: u64) -> Option<file::Response> {
        self.responses.recv_timeout(timeout_us)
    }
}

/// The worker thread's loop: waits for an op, runs it, sends the response, and ends when the
/// request queue is closed and empty.
///
/// A response that cannot be queued is dropped and reported to the system log, because the
/// worker has nowhere else to put it. It cannot happen if the client takes each response before
/// it sends the next request.
fn work<F: files::Files>(
    files: &F,
    requests: &channel::Receiver<file::Op>,
    responses: &channel::Sender<file::Response>,
) {
    while let Some(op) = requests.recv() {
        let response = handle(files, op);
        if responses.try_send(response).is_err() {
            log::error!(
                "file service: the response queue is full, dropped a response (a second request \
                 was sent before the first response was taken)"
            );
        }
    }
}

/// Runs one op against `files` and says how it went. This is all the worker does, without the
/// thread and the queues, so it can be tested on its own. A successful read returns the file's
/// bytes, and any other success returns no bytes.
pub fn handle<F: files::Files>(files: &F, op: file::Op) -> file::Response {
    match op {
        file::Op::Append { path, data } => files
            .append(path.as_str(), data.as_slice())
            .map(|()| file::Data::EMPTY)
            .map_err(error_from),
        file::Op::WriteFile { path, data } => files
            .write_file(path.as_str(), data.as_slice())
            .map(|()| file::Data::EMPTY)
            .map_err(error_from),
        file::Op::ReadFile { path } => {
            // The worker's own copy, on its stack: the file is read into it, and then copied into
            // the response, which is moved to the main thread.
            let mut buf = [0_u8; file::FILE_DATA_MAX_BYTES];
            let len = files
                .read_file(path.as_str(), &mut buf)
                .map_err(error_from)?;
            file::Data::new(buf.get(..len).ok_or(file::Error::Other)?)
        }
    }
}

/// The file service's error for one of the OSAL's. A kind the service's clients cannot do
/// anything different about (a read that ended early, an unknown failure) is `Other`.
///
/// This is a function, not a `From` impl, because neither type is this crate's, and Rust does not
/// allow an impl of a foreign trait between two foreign types. It is here because the core is
/// the one crate that sees both.
fn error_from(error: files::FileError) -> file::Error {
    match error {
        files::FileError::NotFound => file::Error::NotFound,
        files::FileError::PermissionDenied => file::Error::PermissionDenied,
        files::FileError::StorageFull => file::Error::StorageFull,
        files::FileError::TooLarge => file::Error::TooLarge,
        files::FileError::UnexpectedEof | files::FileError::Other => file::Error::Other,
    }
}

#[cfg(test)]
mod tests {
    use super as file_service;
    use vee_core_api::file;
    use vee_osal::fake_files;
    use vee_osal::files;

    fn path(text: &str) -> file::Path {
        file::path(text).unwrap()
    }

    fn data(bytes: &[u8]) -> file::Data {
        file::Data::new(bytes).unwrap()
    }

    fn handled(files: &fake_files::FakeFiles, op: file::Op) -> file::Response {
        file_service::handle(files, op)
    }

    #[test]
    fn an_append_creates_the_file_then_adds_to_it_and_answers_with_no_data() {
        let files = fake_files::FakeFiles::new();
        let first = handled(
            &files,
            file::Op::Append {
                path: path("log"),
                data: data(b"ab"),
            },
        );
        assert_eq!(first, Ok(file::Data::EMPTY));
        let second = handled(
            &files,
            file::Op::Append {
                path: path("log"),
                data: data(b"cd"),
            },
        );
        assert_eq!(second, Ok(file::Data::EMPTY));
        assert_eq!(files.get("log"), Some(b"abcd".to_vec()));
    }

    #[test]
    fn a_write_replaces_the_whole_file() {
        let files = fake_files::FakeFiles::new();
        files.put("table", b"old contents");
        let response = handled(
            &files,
            file::Op::WriteFile {
                path: path("table"),
                data: data(b"new"),
            },
        );
        assert_eq!(response, Ok(file::Data::EMPTY));
        assert_eq!(files.get("table"), Some(b"new".to_vec()));
    }

    #[test]
    fn a_read_answers_with_the_files_bytes() {
        let files = fake_files::FakeFiles::new();
        files.put("table", b"contents");
        let response = handled(
            &files,
            file::Op::ReadFile {
                path: path("table"),
            },
        );
        assert_eq!(response, Ok(data(b"contents")));
    }

    #[test]
    fn a_read_of_an_empty_file_answers_with_no_bytes() {
        let files = fake_files::FakeFiles::new();
        files.put("empty", b"");
        let response = handled(
            &files,
            file::Op::ReadFile {
                path: path("empty"),
            },
        );
        assert_eq!(response, Ok(file::Data::EMPTY));
    }

    #[test]
    fn a_read_of_a_missing_file_is_not_found() {
        let files = fake_files::FakeFiles::new();
        let response = handled(&files, file::Op::ReadFile { path: path("nope") });
        assert_eq!(response, Err(file::Error::NotFound));
    }

    #[test]
    fn a_read_of_a_file_that_does_not_fit_is_too_large() {
        let files = fake_files::FakeFiles::new();
        files.put("big", &[1_u8; file::FILE_DATA_MAX_BYTES + 1]);
        let response = handled(&files, file::Op::ReadFile { path: path("big") });
        assert_eq!(response, Err(file::Error::TooLarge));
        // A file of exactly the largest size does fit.
        files.put("exact", &[1_u8; file::FILE_DATA_MAX_BYTES]);
        let response = handled(
            &files,
            file::Op::ReadFile {
                path: path("exact"),
            },
        );
        assert_eq!(response.unwrap().len(), file::FILE_DATA_MAX_BYTES);
    }

    #[test]
    fn each_os_error_maps_to_the_service_error_a_client_can_act_on() {
        let table = [
            (files::FileError::NotFound, file::Error::NotFound),
            (
                files::FileError::PermissionDenied,
                file::Error::PermissionDenied,
            ),
            (files::FileError::StorageFull, file::Error::StorageFull),
            (files::FileError::TooLarge, file::Error::TooLarge),
            (files::FileError::UnexpectedEof, file::Error::Other),
            (files::FileError::Other, file::Error::Other),
        ];
        for (os_error, expected) in table {
            assert_eq!(file_service::error_from(os_error), expected, "{os_error:?}");
        }
    }
}
