//! `FileDevice`, with its real worker thread and a fake file system. Time is given to it as a
//! number, so the timeouts are exact, and the worker is held at a gate where a test needs a request
//! to stay unanswered.

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
use vee_core::file_device;
use vee_core_api::file;
use vee_osal::fake_files;
use vee_osal::linux::std_alloc_guard;

mod common;

use common::Gate;
use common::GatedFiles;

// So a test can count the allocations of its own thread.
#[global_allocator]
static GUARD: std_alloc_guard::GuardAlloc = std_alloc_guard::GuardAlloc;

const TIMEOUT_US: u64 = 1_000;
const WAIT: time::Duration = time::Duration::from_secs(10);
const POLL: time::Duration = time::Duration::from_millis(1);

fn append(bytes: &[u8]) -> file::Op {
    file::Op::Append {
        path: file::path("log").unwrap(),
        data: file::Data::new(bytes).unwrap(),
    }
}

/// A device whose worker is held at a gate, and the gate.
fn gated_device() -> (file_device::FileDevice, sync::Arc<Gate>) {
    let gate = Gate::new();
    let gated = GatedFiles {
        inner: fake_files::FakeFiles::new(),
        gate: sync::Arc::clone(&gate),
    };
    (file_device::FileDevice::start(gated).unwrap(), gate)
}

/// Polls (at time `now_us`) until a response comes, with a limit.
fn next_response(device: &mut file_device::FileDevice, now_us: u64) -> file::Response {
    let started = time::Instant::now();
    loop {
        if let Some(response) = device.poll(now_us) {
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
        assert!(started.elapsed() < WAIT, "the worker never took a request");
        thread::sleep(POLL);
    }
}

#[test]
fn a_request_gets_its_response_and_the_device_is_free_again() {
    let files = fake_files::FakeFiles::new();
    let mut device = file_device::FileDevice::start(files.clone()).unwrap();
    assert!(!device.busy());

    device.request(0, TIMEOUT_US, append(b"hello")).unwrap();
    assert!(device.busy());
    assert_eq!(next_response(&mut device, 0), Ok(file::Data::EMPTY));
    assert!(!device.busy());
    assert_eq!(files.get("log"), Some(b"hello".to_vec()));
}

#[test]
fn the_device_is_busy_until_the_response_is_collected() {
    let mut device = file_device::FileDevice::start(fake_files::FakeFiles::new()).unwrap();
    device.request(0, TIMEOUT_US, append(b"a")).unwrap();
    // Busy at once, whether or not the worker has finished: the response is still to be collected.
    assert_eq!(
        device.request(0, TIMEOUT_US, append(b"b")),
        Err(file::Error::Busy)
    );
    assert!(next_response(&mut device, 0).is_ok());
    device.request(0, TIMEOUT_US, append(b"c")).unwrap(); // free again
    assert!(next_response(&mut device, 0).is_ok());
}

#[test]
fn a_request_that_gets_no_answer_times_out_at_its_deadline() {
    let (mut device, gate) = gated_device();
    device.request(100, TIMEOUT_US, append(b"x")).unwrap();
    wait_for_the_gate(&gate, 1); // the worker has it, and is stuck

    assert_eq!(device.poll(100 + TIMEOUT_US - 1), None); // not yet
    assert_eq!(
        device.poll(100 + TIMEOUT_US), // at the deadline
        Some(Err(file::Error::TimedOut))
    );
    assert_eq!(device.poll(100 + TIMEOUT_US), None); // told once
    gate.grant(1); // let the worker finish, so it can end
}

#[test]
fn after_a_timeout_the_device_stays_busy_until_the_worker_really_finishes() {
    let (mut device, gate) = gated_device();
    device.request(0, TIMEOUT_US, append(b"old")).unwrap();
    wait_for_the_gate(&gate, 1);
    assert_eq!(device.poll(TIMEOUT_US), Some(Err(file::Error::TimedOut)));

    // The client has been told, but the worker is still stuck on it, so a new request is
    // refused: it would only wait behind it, and could run late.
    assert!(device.busy());
    assert_eq!(
        device.request(TIMEOUT_US, TIMEOUT_US, append(b"new")),
        Err(file::Error::Busy)
    );

    // The worker finishes. Its response is the stale one: dropped, and the device is free.
    gate.grant(1);
    let started = time::Instant::now();
    while device.busy() {
        assert_eq!(device.poll(TIMEOUT_US), None);
        assert!(started.elapsed() < WAIT, "the worker never finished");
        thread::sleep(POLL);
    }
    device
        .request(TIMEOUT_US, TIMEOUT_US, append(b"new"))
        .unwrap();
    gate.grant(1);
    // The answer is the new request's, not the stale one's (a stale one would have come first).
    assert_eq!(
        next_response(&mut device, TIMEOUT_US),
        Ok(file::Data::EMPTY)
    );
}

#[test]
fn a_response_that_is_there_wins_over_a_deadline_that_has_passed() {
    let files = fake_files::FakeFiles::new();
    let mut device = file_device::FileDevice::start(files.clone()).unwrap();
    device.request(0, TIMEOUT_US, append(b"x")).unwrap();
    // Wait until the worker has done it and queued the response, without polling.
    let started = time::Instant::now();
    while files.get("log").is_none() {
        assert!(started.elapsed() < WAIT, "the worker never ran");
        thread::sleep(POLL);
    }
    thread::sleep(time::Duration::from_millis(100));
    // Polling long after the deadline gives the response, not a timeout.
    assert_eq!(device.poll(10 * TIMEOUT_US), Some(Ok(file::Data::EMPTY)));
    assert_eq!(device.poll(10 * TIMEOUT_US), None);
    assert!(!device.busy());
}

#[test]
fn wait_gives_the_response_of_the_request_in_flight() {
    let mut device = file_device::FileDevice::start(fake_files::FakeFiles::new()).unwrap();
    assert_eq!(device.wait(1_000), None); // nothing in flight: nothing to wait for
    device.request(0, TIMEOUT_US, append(b"x")).unwrap();
    assert_eq!(device.wait(10_000_000), Some(Ok(file::Data::EMPTY)));
    assert!(!device.busy());
}

#[test]
fn wait_that_runs_out_leaves_the_request_in_flight() {
    let (mut device, gate) = gated_device();
    device.request(0, TIMEOUT_US, append(b"x")).unwrap();
    wait_for_the_gate(&gate, 1);
    assert_eq!(device.wait(20_000), None);
    assert!(device.busy()); // the worker still has it
    gate.grant(1);
    assert_eq!(device.wait(10_000_000), Some(Ok(file::Data::EMPTY)));
}

#[test]
fn a_request_and_a_poll_do_not_allocate() {
    let mut device = file_device::FileDevice::start(fake_files::FakeFiles::new()).unwrap();
    device.request(0, TIMEOUT_US, append(b"warm up")).unwrap();
    assert!(next_response(&mut device, 0).is_ok());

    let op = append(b"y");
    let before = std_alloc_guard::allocations_on_this_thread();
    device.request(0, TIMEOUT_US, op).unwrap();
    let started = time::Instant::now();
    let response = loop {
        if let Some(response) = device.poll(0) {
            break response;
        }
        assert!(started.elapsed() < WAIT, "no response");
        thread::sleep(POLL);
    };
    let after = std_alloc_guard::allocations_on_this_thread();
    assert_eq!(after, before);
    assert!(response.is_ok());
}
