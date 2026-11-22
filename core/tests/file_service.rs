//! The file service with its real worker thread and queues, against a fake file system. What these
//! tests check is what a client sees: ops and responses through `submit`, `poll` and `wait`, in
//! order, with a full queue handing the op back, and no allocation per request. The thread is
//! real, so a test waits for responses by polling, with a limit.
//!
//! The service itself does not stop a client from sending a second request before it has taken
//! the first response: that is `FileDevice`'s job, and it is tested in `file_device.rs`. A test
//! here that sends more than one at a time says so.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failure should panic, and the helpers are not `#[test]` fns, so \
              clippy.toml's exemption does not cover them"
)]

use std::sync;
use std::sync::atomic;
use std::thread;
use std::time;
use vee_core::file_service;
use vee_core_api::file;
use vee_osal::fake_files;
use vee_osal::linux::std_alloc_guard;

mod common;

use common::Gate;
use common::GatedFiles;

// So a test can count the allocations of its own thread.
#[global_allocator]
static GUARD: std_alloc_guard::GuardAlloc = std_alloc_guard::GuardAlloc;

/// How long a test waits for the worker before it calls it a failure, and how often it looks.
const WAIT: time::Duration = time::Duration::from_secs(10);
const POLL: time::Duration = time::Duration::from_millis(1);

fn append(path: &str, bytes: &[u8]) -> file::Op {
    file::Op::Append {
        path: file::path(path).unwrap(),
        data: file::Data::new(bytes).unwrap(),
    }
}

fn read(path: &str) -> file::Op {
    file::Op::ReadFile {
        path: file::path(path).unwrap(),
    }
}

/// Polls until a response has come back.
fn next_response(service: &file_service::FileService) -> file::Response {
    let started = time::Instant::now();
    loop {
        if let Some(response) = service.poll() {
            return response;
        }
        assert!(started.elapsed() < WAIT, "no response");
        thread::sleep(POLL);
    }
}

/// Waits until the worker has taken `count` ops to its gate.
fn wait_for_the_gate(gate: &Gate, count: usize) {
    let started = time::Instant::now();
    while gate.reached.load(atomic::Ordering::SeqCst) < count {
        assert!(started.elapsed() < WAIT, "the worker never took an op");
        thread::sleep(POLL);
    }
}

#[test]
fn an_op_is_done_by_the_worker_and_its_response_comes_back() {
    let files = fake_files::FakeFiles::new();
    let service = file_service::FileService::start(files.clone()).unwrap();

    service.submit(append("log", b"hello")).unwrap();
    assert_eq!(next_response(&service), Ok(file::Data::EMPTY));
    assert_eq!(files.get("log"), Some(b"hello".to_vec()));

    service.submit(read("log")).unwrap();
    assert_eq!(
        next_response(&service),
        Ok(file::Data::new(b"hello").unwrap())
    );
}

#[test]
fn an_error_comes_back_as_the_services_error_and_the_service_carries_on() {
    let service = file_service::FileService::start(fake_files::FakeFiles::new()).unwrap();
    service.submit(read("missing")).unwrap();
    assert_eq!(next_response(&service), Err(file::Error::NotFound));
    service.submit(append("log", b"x")).unwrap();
    assert_eq!(next_response(&service), Ok(file::Data::EMPTY));
}

#[test]
fn ops_one_after_another_are_done_in_order() {
    let files = fake_files::FakeFiles::new();
    let service = file_service::FileService::start(files.clone()).unwrap();
    for byte in b"abcdef" {
        service.submit(append("log", &[*byte])).unwrap();
        assert!(next_response(&service).is_ok());
    }
    // A read after the appends sees all of them.
    service.submit(read("log")).unwrap();
    assert_eq!(
        next_response(&service),
        Ok(file::Data::new(b"abcdef").unwrap())
    );
}

#[test]
fn a_full_queue_gives_the_op_back_whole() {
    let gate = Gate::new();
    let gated = GatedFiles {
        inner: fake_files::FakeFiles::new(),
        gate: sync::Arc::clone(&gate),
    };
    let service = file_service::FileService::start(gated).unwrap();

    // The worker takes the first op and stops at the gate, so the queue (one slot) is empty again.
    service.submit(append("log", b"0")).unwrap();
    wait_for_the_gate(&gate, 1);
    // The next fills the slot, and the one after that is handed back.
    service.submit(append("log", b"1")).unwrap();
    let over = append("log", b"over");
    assert_eq!(service.submit(over.clone()).unwrap_err(), over);

    // Let the worker go one op at a time, taking each response before the next (it holds one).
    gate.grant(1);
    assert!(next_response(&service).is_ok());
    gate.grant(1);
    assert!(next_response(&service).is_ok());

    // With room again, the op that was handed back is accepted.
    service.submit(over).unwrap();
    gate.grant(1);
    assert!(next_response(&service).is_ok());
}

#[test]
fn wait_gives_the_response_when_it_comes_and_none_when_it_does_not() {
    let gate = Gate::new();
    let gated = GatedFiles {
        inner: fake_files::FakeFiles::new(),
        gate: sync::Arc::clone(&gate),
    };
    let service = file_service::FileService::start(gated).unwrap();
    service.submit(append("log", b"x")).unwrap();
    wait_for_the_gate(&gate, 1);

    // The worker is held at the gate, so a short wait gives nothing, after about that long.
    let started = time::Instant::now();
    assert_eq!(service.wait(20_000), None);
    assert!(started.elapsed() >= time::Duration::from_millis(20));

    // Let it go: a long wait returns as soon as the response is there.
    gate.grant(1);
    assert_eq!(service.wait(10_000_000), Some(Ok(file::Data::EMPTY)));
}

#[test]
fn the_worker_ends_when_the_service_is_dropped() {
    let files = fake_files::FakeFiles::new();
    let service = file_service::FileService::start(files.clone()).unwrap();
    service.submit(append("log", b"x")).unwrap();
    assert!(next_response(&service).is_ok());

    // The worker holds a handle to the files, so there are two (the worker's and ours) while its
    // thread runs, and one once it has ended.
    assert_eq!(files.handles(), 2);
    drop(service);
    let started = time::Instant::now();
    while files.handles() > 1 {
        assert!(started.elapsed() < WAIT, "the worker did not end");
        thread::sleep(POLL);
    }
}

#[test]
fn submit_and_poll_do_not_allocate() {
    let service = file_service::FileService::start(fake_files::FakeFiles::new()).unwrap();
    // Warm up: the first op through any new queue may set things up.
    service.submit(append("log", b"x")).unwrap();
    assert!(next_response(&service).is_ok());

    let op = append("log", b"y");
    let before = std_alloc_guard::allocations_on_this_thread();
    service.submit(op).unwrap();
    let started = time::Instant::now();
    let response = loop {
        if let Some(response) = service.poll() {
            break response;
        }
        assert!(started.elapsed() < WAIT, "no response");
        thread::sleep(POLL);
    };
    let after = std_alloc_guard::allocations_on_this_thread();
    assert_eq!(after, before);
    assert!(response.is_ok());
}

/// Collects what the program logs, so a test can see a line that the code under test writes to the
/// system log. The logger is global (one for the whole test program, which runs its tests on
/// several threads), so a test looks only for a line of its own.
struct Capture {
    lines: sync::Mutex<Vec<String>>,
}

static CAPTURE: Capture = Capture {
    lines: sync::Mutex::new(Vec::new()),
};

impl log::Log for Capture {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }
    fn log(&self, record: &log::Record<'_>) {
        self.lines.lock().unwrap().push(record.args().to_string());
    }
    fn flush(&self) {}
}

/// Installs `CAPTURE` once. Only the first call does anything.
fn capture_logs() {
    static ONCE: sync::Once = sync::Once::new();
    ONCE.call_once(|| {
        log::set_logger(&CAPTURE).unwrap();
        log::set_max_level(log::LevelFilter::Error);
    });
}

fn logged(text: &str) -> bool {
    CAPTURE
        .lines
        .lock()
        .unwrap()
        .iter()
        .any(|line| line.contains(text))
}

#[test]
fn a_second_request_before_the_first_response_is_taken_loses_the_second_response_and_says_so() {
    // The service itself allows it, so this is the misuse that `FileDevice` prevents: two ops in
    // flight, and one response slot.
    capture_logs();
    let files = fake_files::FakeFiles::new();
    let service = file_service::FileService::start(files.clone()).unwrap();
    service.submit(append("log", b"1")).unwrap();
    // The first response fills the slot (once the worker has done it), and only then is the
    // second op sent. The worker does it, and cannot queue its response.
    let started = time::Instant::now();
    while files.get("log").is_none() {
        assert!(started.elapsed() < WAIT, "the first op never ran");
        thread::sleep(POLL);
    }
    thread::sleep(time::Duration::from_millis(50)); // the response is queued
    service.submit(append("log", b"2")).unwrap();
    let started = time::Instant::now();
    while !logged("the response queue is full") {
        assert!(started.elapsed() < WAIT, "the drop was not logged");
        thread::sleep(POLL);
    }
    assert_eq!(files.get("log"), Some(b"12".to_vec())); // both ops were done
    assert!(service.poll().is_some()); // the first response is the one that is there
    assert!(service.poll().is_none());
}
