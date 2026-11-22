//! `FileDevice`: a client's whole side of the file service in one value. It is a
//! `file_service::FileService` (the worker thread and its two one-slot queues) and a small state:
//! is a request in flight, and when is it due.
//!
//! A client has one, and calls, on the main thread:
//!
//! - `request(now_us, timeout_us, op)` sends an operation. It is `Busy` if a request is in
//!   flight already: ONE request at a time. `Busy` is an ordinary answer, and each client deals with
//!   it in its own way: the event log writer leaves its events in the queue until the device is
//!   free, and a command module NAKs the command.
//! - `poll(now_us)` returns what has happened to the request in flight: its response, or
//!   `TimedOut` when it has gone overdue. It returns at once. Call it once a step, before
//!   `request`.
//!
//! There is only one request in flight, so a response is always the answer to the request the
//! client is waiting for, and there is nothing to match it with. A request that times out is
//! reported to the client as `TimedOut`, but the device stays BUSY until the worker has really
//! finished it, because the worker may be stuck, and anything sent now would only wait behind it
//! (and could run late, after the operator was told it had failed). A response that comes after
//! the timeout is the stale one, and is dropped.
//!
//! More than one thing at a time comes from more devices, not a deeper one.

use crate::file_service;
use vee_core_api::file;
use vee_osal::files;
use vee_osal::thread;

/// Where a request is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Nothing in flight: `request` is accepted.
    Idle,
    /// A request is with the worker, due at `deadline_us`.
    Waiting { deadline_us: u64 },
    /// The request timed out and the client was told, but the worker has not finished it yet.
    TimedOut,
}

/// A client's file service and the state of its one request.
pub struct FileDevice {
    service: file_service::FileService,
    state: State,
}

impl FileDevice {
    /// Starts the worker thread, which does its file operations on `files` (see
    /// `FileService::start`: give it its own handle). Allocates, so it is called at startup.
    pub fn start<F>(files: F) -> Result<Self, thread::SpawnError>
    where
        F: files::Files + Send + 'static,
    {
        Ok(FileDevice {
            service: file_service::FileService::start(files)?,
            state: State::Idle,
        })
    }

    /// Sends `op`, due `timeout_us` after `now_us`. `Busy` if a request is in flight already (or
    /// the one that timed out is still with the worker), and then nothing was sent.
    pub fn request(
        &mut self,
        now_us: u64,
        timeout_us: u64,
        op: file::Op,
    ) -> Result<(), file::Error> {
        if self.state != State::Idle {
            return Err(file::Error::Busy);
        }
        // The worker is idle, so its queue is empty and the op is accepted. If it were not (a
        // bug), the op is given back and dropped here, and the device says `Busy`.
        if self.service.submit(op).is_err() {
            return Err(file::Error::Busy);
        }
        self.state = State::Waiting {
            deadline_us: now_us.saturating_add(timeout_us),
        };
        Ok(())
    }

    /// What has happened to the request in flight: its response, or `TimedOut` once, at its
    /// deadline (not after it), or `None`. A real response comes before a timeout, so an answer
    /// that arrives in the same step as the deadline is the answer. It also returns `None` when
    /// it dropped the stale response of a request that had timed out.
    pub fn poll(&mut self, now_us: u64) -> Option<file::Response> {
        if let Some(response) = self.service.poll() {
            return self.arrived(response);
        }
        if let State::Waiting { deadline_us } = self.state
            && deadline_us <= now_us
        {
            self.state = State::TimedOut;
            return Some(Err(file::Error::TimedOut));
        }
        None
    }

    /// Waits for the request in flight, for at most `timeout_us` microseconds, and returns its
    /// response, or `None` if it did not come. This is the one call here that waits, for the
    /// shutdown. After a `None`, `busy` says whether the request is still with the worker (the wait
    /// ran out) or not (a stale response was dropped).
    pub fn wait(&mut self, timeout_us: u64) -> Option<file::Response> {
        if self.state == State::Idle {
            return None;
        }
        let response = self.service.wait(timeout_us)?;
        self.arrived(response)
    }

    /// Whether a request is in flight, or has timed out and is still with the worker. A new
    /// request is accepted only when this is false.
    pub fn busy(&self) -> bool {
        self.state != State::Idle
    }

    /// A response came from the worker. The request is finished, so the device is free again.
    /// The response is the client's if the client was still waiting for it.
    fn arrived(&mut self, response: file::Response) -> Option<file::Response> {
        let was = self.state;
        self.state = State::Idle;
        match was {
            State::Waiting { .. } => Some(response),
            State::TimedOut => {
                log::debug!("file device: dropped the response to a request that timed out");
                None
            }
            State::Idle => {
                log::error!("file device: a response came with no request in flight, dropped");
                None
            }
        }
    }
}
