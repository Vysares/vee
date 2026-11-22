//! The event log's writer: it takes the events waiting in the log queue and has them appended to
//! the event log by the file service, so that nothing on the main thread waits on a disk.
//!
//! It is a client of its own file service (`file_device::FileDevice`), so it has at most ONE write
//! in flight. Each tick (`step`):
//! 1. It collects what happened to the write in flight, if there is one.
//! 2. If the device is free and events are waiting, it builds ONE batch (`event_log::batch`: as
//!    many records as fit in a request, in order) and sends it.
//!
//! While a write is in flight, the events wait in the log queue, which is also what absorbs a
//! burst and rides out a stalled disk (a full queue refuses the newest and counts them, and the
//! core reports the count). One write is one sync, so at a low rate a batch is usually one event,
//! and at a high rate the batch grows by itself, so the sync is shared when the load is high.
//!
//! Failures are the writer's own to report: a batch that was not written (the disk is full, or the
//! write timed out) is lost, because there is nothing else to do with it, and it is said once, in
//! the system log, when a streak of failures starts. The event log cannot record its own
//! failure.
//!
//! At shutdown `flush` sends what is queued and waits for it, which is the one place that blocks.

use crate::event_log;
use crate::file_device;
use core::fmt;
use vee_core_api::config;
use vee_core_api::ev;
use vee_core_api::file;
use vee_core_types::event_log as log_types;
use vee_osal::files;
use vee_osal::thread;

config::setting! {
    /// How long a write to the event log may take, in microseconds, before it is called timed out
    /// (and the writer carries on, and the device stays busy until the worker finishes it). A write
    /// is about 2 ms on a disk that is well (more on flash). It is also the longest the shutdown
    /// waits for the last write. Default 5 seconds, 1..=3,600,000,000 (an hour).
    pub EVENT_LOG_TIMEOUT_US: usize = 5_000_000, 1..=3_600_000_000;
}

/// The timeout, as the `u64` that times are.
const TIMEOUT_US: u64 = EVENT_LOG_TIMEOUT_US as u64;

// A record must fit in an empty batch, or it could never be sent. A `FILE_DATA_MAX_BYTES` that is
// too small is a build error, not a log that silently stops.
const _: () = assert!(
    log_types::RECORD_LEN <= file::FILE_DATA_MAX_BYTES,
    "an event log record does not fit in a file request: raise VEE_FILE_DATA_MAX_BYTES"
);

/// Why the writer could not start.
#[derive(Debug)]
pub enum StartError {
    /// The path of the event log is not a valid path for the file service.
    Path(file::Error),
    /// The OS would not start the worker thread.
    Thread(thread::SpawnError),
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartError::Path(error) => write!(f, "log path: {error}"),
            StartError::Thread(error) => write!(f, "{error}"),
        }
    }
}

/// The event log's writer. There is one, owned by the core.
pub struct LogWriter {
    device: file_device::FileDevice,
    path: file::Path,
    /// Whether the last write failed. It makes a streak of failures one report: only the failure
    /// that starts a streak is reported.
    failing: bool,
}

impl LogWriter {
    /// Starts the worker thread, which appends to the log at `path` on `files` (its own handle:
    /// see `FileService::start`). Allocates, so it is called at startup.
    pub fn start<F>(files: F, path: &str) -> Result<Self, StartError>
    where
        F: files::Files + Send + 'static,
    {
        let path = file::path(path).map_err(StartError::Path)?;
        let device = file_device::FileDevice::start(files).map_err(StartError::Thread)?;
        Ok(LogWriter {
            device,
            path,
            failing: false,
        })
    }

    /// One tick of the writer, at time `now`: collects the result of the write in flight, and
    /// sends the next batch if the device is free. It never waits.
    pub fn step(&mut self, ev: &mut ev::Ev, now: u64) {
        if let Some(response) = self.device.poll(now) {
            self.note(&response);
        }
        self.send_next(ev, now);
    }

    /// Writes everything that is queued, and waits until it is on the disk, or gives up waiting
    /// for a write that takes longer than `EVENT_LOG_TIMEOUT_US`. This blocks: it is for the
    /// shutdown only.
    pub fn flush(&mut self, ev: &mut ev::Ev, now: u64) {
        loop {
            if self.device.busy() {
                match self.device.wait(TIMEOUT_US) {
                    Some(response) => self.note(&response),
                    None if self.device.busy() => {
                        log::error!("event log: gave up waiting for the last write");
                        return;
                    }
                    None => {} // the response of a write that had timed out: the device is free now
                }
            } else if ev.log.is_empty() {
                return;
            } else {
                self.send_next(ev, now);
            }
        }
    }

    /// Whether a write is in flight, or has timed out and is still with the worker.
    pub fn busy(&self) -> bool {
        self.device.busy()
    }

    /// Builds the next batch and sends it, if the device is free and there is something to send.
    fn send_next(&mut self, ev: &mut ev::Ev, now: u64) {
        if self.device.busy() || ev.log.is_empty() {
            return;
        }
        let data = event_log::batch(ev);
        if data.is_empty() {
            // A record always fits an empty batch (checked at build time), so this cannot happen.
            // If it did, the event would never go, so it is dropped to let the others through.
            log::error!("event log: an event does not fit in a batch, dropped");
            ev.log.pop();
            return;
        }
        let op = file::Op::Append {
            path: self.path,
            data,
        };
        if let Err(error) = self.device.request(now, TIMEOUT_US, op) {
            // The device was free, so this does not happen. The batch is lost if it does.
            log::error!("event log: a batch of events was lost: {error}");
        }
    }

    /// Takes note of how a write went. A failure is reported when it starts a streak, and a
    /// success ends the streak.
    fn note(&mut self, response: &file::Response) {
        match response {
            Ok(_) => self.failing = false,
            Err(error) if !self.failing => {
                self.failing = true;
                log::error!("event log: a batch of events was not written: {error}");
            }
            Err(_) => {}
        }
    }
}
