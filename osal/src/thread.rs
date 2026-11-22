//! Starting a thread. The one thing Vee needs from a thread is to run a piece of code for the
//! rest of the process, so that is the one call. It is not joined: a worker runs until its queue is
//! closed or the process ends, and a panic in it aborts the whole process in a flight build, so
//! there is nothing to collect.
//!
//! The call returns an error as a value if the OS refuses a thread (a limit on threads or on
//! memory), and never panics, because that is a failure Vee can report and carry on from.
//!
//! Starting a thread allocates, so it is done at startup, before the allocation guard is armed.

use std::fmt;
use std::io;
use std::thread;

/// Why a thread was not started.
#[derive(Debug)]
pub struct SpawnError(io::Error);

impl fmt::Display for SpawnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cannot start a thread: {}", self.0)
    }
}

impl std::error::Error for SpawnError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

/// Runs `work` on a new thread called `name` (it shows in debuggers and in the panic line).
/// Returns when the thread has been started, not when it has run.
pub fn spawn<F>(name: &str, work: F) -> Result<(), SpawnError>
where
    F: FnOnce() + Send + 'static,
{
    thread::Builder::new()
        .name(name.to_string())
        .spawn(work)
        .map(|_| ()) // the handle is dropped on purpose: the thread is not joined
        .map_err(SpawnError)
}

#[cfg(test)]
mod tests {
    use super as thread;
    use crate::channel;

    #[test]
    fn the_work_runs_on_another_thread_with_the_given_name() {
        let (sender, receiver) = channel::channel(1);
        thread::spawn("vee-test-worker", move || {
            let current = std::thread::current();
            let name = current.name().unwrap_or("").to_string();
            sender.try_send((name, current.id())).unwrap();
        })
        .unwrap();
        let (name, id) = receiver.recv().unwrap();
        assert_eq!(name, "vee-test-worker");
        assert_ne!(id, std::thread::current().id());
    }

    #[test]
    fn an_error_says_it_is_about_a_thread_and_has_a_source() {
        use std::error::Error as _;
        let error = thread::SpawnError(std::io::Error::from(std::io::ErrorKind::WouldBlock));
        assert!(error.to_string().starts_with("cannot start a thread: "));
        assert!(error.source().is_some());
    }
}
