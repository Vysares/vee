//! Helpers shared by the file service tests.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    reason = "test code: each test program uses some of these, and a failure should panic"
)]

use std::sync;
use std::sync::atomic;
use vee_osal::fake_files;
use vee_osal::files;

/// A file system whose appends wait at a gate until the test grants a permit for each one, so a
/// test can hold the worker in the middle of a request while it fills the queue, and then let it
/// finish one request at a time.
#[derive(Clone)]
pub struct GatedFiles {
    pub inner: fake_files::FakeFiles,
    pub gate: sync::Arc<Gate>,
}

pub struct Gate {
    /// Appends that may go on now. Each one that does uses up a permit.
    permits: sync::Mutex<usize>,
    granted: sync::Condvar,
    /// How many appends have reached the gate.
    pub reached: atomic::AtomicUsize,
}

impl Gate {
    pub fn new() -> sync::Arc<Gate> {
        sync::Arc::new(Gate {
            permits: sync::Mutex::new(0),
            granted: sync::Condvar::new(),
            reached: atomic::AtomicUsize::new(0),
        })
    }

    fn wait_here(&self) {
        self.reached.fetch_add(1, atomic::Ordering::SeqCst);
        let mut permits = self.permits.lock().unwrap();
        while *permits == 0 {
            permits = self.granted.wait(permits).unwrap();
        }
        *permits -= 1;
    }

    pub fn grant(&self, count: usize) {
        *self.permits.lock().unwrap() += count;
        self.granted.notify_all();
    }
}

impl files::Files for GatedFiles {
    fn len(&self, path: &str) -> Result<Option<u64>, files::FileError> {
        self.inner.len(path)
    }
    fn read_at(&self, path: &str, offset: u64, buf: &mut [u8]) -> Result<(), files::FileError> {
        self.inner.read_at(path, offset, buf)
    }
    fn read_file(&self, path: &str, buf: &mut [u8]) -> Result<usize, files::FileError> {
        self.inner.read_file(path, buf)
    }
    fn write_file(&self, path: &str, bytes: &[u8]) -> Result<(), files::FileError> {
        self.inner.write_file(path, bytes)
    }
    fn append(&self, path: &str, bytes: &[u8]) -> Result<(), files::FileError> {
        self.gate.wait_here();
        self.inner.append(path, bytes)
    }
    fn rotate(&self, path: &str) -> Result<(), files::FileError> {
        self.inner.rotate(path)
    }
}
