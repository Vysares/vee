//! `LogWriter`, with its real worker thread and a fake file system: how it batches the events,
//! that it has one write in flight, what `flush` does at shutdown (including giving up on a stuck
//! write), and that a streak of failed writes is reported ONCE. The reports go to the system log
//! (`log::error!`), which these tests capture, so this is its own test program (the logger is one
//! per program). The tests that read the captured lines take `SERIAL`, since tests run at the same
//! time and share the one list.

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
use vee_core::log_writer;
use vee_core_api::ev;
use vee_core_api::file;
use vee_core_types::core_event;
use vee_core_types::event_log as log_types;
use vee_osal::fake_files;
use vee_osal::files;

mod common;

use common::Gate;
use common::GatedFiles;

const PATH: &str = "events.log";
const RECORD_LEN: usize = log_types::RECORD_LEN;
/// How many records one write holds: as many as fit in a file request.
const FIT: usize = file::FILE_DATA_MAX_BYTES / RECORD_LEN;
const WAIT: time::Duration = time::Duration::from_secs(10);
const POLL: time::Duration = time::Duration::from_millis(1);

// ---- The captured system log ----

struct Capture {
    lines: sync::Mutex<Vec<String>>,
}

impl log::Log for Capture {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        let line = format!("{} {}", record.level(), record.args());
        self.lines
            .lock()
            .unwrap_or_else(sync::PoisonError::into_inner)
            .push(line);
    }

    fn flush(&self) {}
}

static CAPTURE: Capture = Capture {
    lines: sync::Mutex::new(Vec::new()),
};

/// Taken by every test that counts log lines, so two of them never share the list.
static SERIAL: sync::Mutex<()> = sync::Mutex::new(());

/// Installs the logger once, takes `SERIAL`, and empties the list.
fn capture() -> sync::MutexGuard<'static, ()> {
    static ONCE: sync::Once = sync::Once::new();
    ONCE.call_once(|| {
        log::set_logger(&CAPTURE).unwrap();
        log::set_max_level(log::LevelFilter::Trace);
    });
    let guard = SERIAL.lock().unwrap_or_else(sync::PoisonError::into_inner);
    CAPTURE.lines.lock().unwrap().clear();
    guard
}

/// How many captured lines contain `text`.
fn lines_with(text: &str) -> usize {
    CAPTURE
        .lines
        .lock()
        .unwrap()
        .iter()
        .filter(|line| line.contains(text))
        .count()
}

// ---- A file system whose appends can be made to fail ----

#[derive(Clone)]
struct Flaky {
    inner: fake_files::FakeFiles,
    failing: sync::Arc<atomic::AtomicBool>,
}

impl Flaky {
    fn new() -> Self {
        Flaky {
            inner: fake_files::FakeFiles::new(),
            failing: sync::Arc::new(atomic::AtomicBool::new(false)),
        }
    }

    fn fail(&self, failing: bool) {
        self.failing.store(failing, atomic::Ordering::SeqCst);
    }
}

impl files::Files for Flaky {
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
        if self.failing.load(atomic::Ordering::SeqCst) {
            return Err(files::FileError::StorageFull);
        }
        self.inner.append(path, bytes)
    }
    fn rotate(&self, path: &str) -> Result<(), files::FileError> {
        self.inner.rotate(path)
    }
}

// ---- Helpers ----

/// Queues `count` events whose text is `e<n>`, numbering from `first`.
fn queue(ev: &mut ev::Ev, first: usize, count: usize) {
    for n in first..first + count {
        ev.send(
            1,
            n as u64,
            core_event::CoreEvent::ResetReport,
            format_args!("e{n}"),
        );
    }
}

/// Steps the writer until nothing is in flight and nothing is queued, with a limit.
fn settle(writer: &mut log_writer::LogWriter, ev: &mut ev::Ev) {
    let started = time::Instant::now();
    loop {
        writer.step(ev, 0);
        if !writer.busy() && ev.log.is_empty() {
            return;
        }
        assert!(started.elapsed() < WAIT, "the writer never settled");
        thread::sleep(POLL);
    }
}

/// The texts of the records in the log, oldest first.
fn written(files: &fake_files::FakeFiles) -> Vec<String> {
    let bytes = files.get(PATH).unwrap_or_default();
    assert_eq!(bytes.len() % RECORD_LEN, 0, "a partial record");
    bytes
        .chunks(RECORD_LEN)
        .map(|record| {
            let event = log_types::decode(record).unwrap();
            String::from_utf8_lossy(event.text.as_bytes()).into_owned()
        })
        .collect()
}

fn writer_on<F: files::Files + Send + 'static>(files: F) -> log_writer::LogWriter {
    log_writer::LogWriter::start(files, PATH).unwrap()
}

/// Waits until the worker has taken `count` appends to its gate.
fn wait_for_the_gate(gate: &Gate, count: usize) {
    let started = time::Instant::now();
    while gate.reached.load(atomic::Ordering::SeqCst) < count {
        assert!(started.elapsed() < WAIT, "the worker never took a request");
        thread::sleep(POLL);
    }
}

// ---- Starting ----

#[test]
fn a_new_writer_is_idle_and_stepping_with_nothing_queued_writes_nothing() {
    let files = fake_files::FakeFiles::new();
    let mut writer = writer_on(files.clone());
    let mut ev = ev::Ev::new();
    assert!(!writer.busy());
    writer.step(&mut ev, 0);
    assert!(!writer.busy());
    assert_eq!(files.get(PATH), None);
}

#[test]
fn the_path_may_be_as_long_as_a_path_can_be_and_no_longer() {
    let longest = "a".repeat(file::FILE_PATH_MAX_BYTES);
    assert!(log_writer::LogWriter::start(fake_files::FakeFiles::new(), &longest).is_ok());

    let too_long = "a".repeat(file::FILE_PATH_MAX_BYTES + 1);
    let error = log_writer::LogWriter::start(fake_files::FakeFiles::new(), &too_long)
        .err()
        .unwrap();
    assert!(matches!(
        error,
        log_writer::StartError::Path(file::Error::TooLarge)
    ));
    assert_eq!(error.to_string(), "log path: too large for a file request");
}

#[test]
fn an_empty_path_and_a_path_that_is_not_ascii_are_refused() {
    for path in ["", "événements.log"] {
        let error = log_writer::LogWriter::start(fake_files::FakeFiles::new(), path)
            .err()
            .unwrap();
        assert!(matches!(
            error,
            log_writer::StartError::Path(file::Error::InvalidPath)
        ));
    }
}

// ---- Batching and the one write in flight ----

#[test]
fn a_step_sends_what_is_queued_as_one_write_in_order() {
    let files = fake_files::FakeFiles::new();
    let mut writer = writer_on(files.clone());
    let mut ev = ev::Ev::new();
    queue(&mut ev, 0, 3);
    writer.step(&mut ev, 0);
    assert!(ev.log.is_empty(), "the batch took all three");
    assert!(writer.busy());
    settle(&mut writer, &mut ev);
    assert_eq!(written(&files), ["e0", "e1", "e2"]);
}

#[test]
fn a_batch_holds_as_many_records_as_fit_and_the_rest_wait_for_the_next() {
    // Just under, exactly at and just over what one write holds.
    for (queued, left_after_the_first) in [(FIT - 1, 0), (FIT, 0), (FIT + 1, 1), (2 * FIT, FIT)] {
        let files = fake_files::FakeFiles::new();
        let mut writer = writer_on(files.clone());
        let mut ev = ev::Ev::new();
        queue(&mut ev, 0, queued);
        writer.step(&mut ev, 0);
        assert_eq!(ev.log.len(), left_after_the_first, "{queued} queued");
        settle(&mut writer, &mut ev);
        let texts = written(&files);
        assert_eq!(
            texts.len(),
            queued,
            "{queued} queued: all of them are written"
        );
        assert_eq!(texts.first().unwrap(), "e0");
        assert_eq!(texts.last().unwrap(), &format!("e{}", queued - 1));
    }
}

#[test]
fn events_queued_during_a_write_wait_and_only_one_write_is_ever_in_flight() {
    let gate = Gate::new();
    let files = fake_files::FakeFiles::new();
    let mut writer = writer_on(GatedFiles {
        inner: files.clone(),
        gate: sync::Arc::clone(&gate),
    });
    let mut ev = ev::Ev::new();

    queue(&mut ev, 0, 2);
    writer.step(&mut ev, 0);
    wait_for_the_gate(&gate, 1);

    // The first write is held at the gate. More events come, and steps do not start a second write.
    queue(&mut ev, 2, 2);
    writer.step(&mut ev, 0);
    writer.step(&mut ev, 0);
    assert_eq!(ev.log.len(), 2, "the new events wait in the queue");
    assert_eq!(gate.reached.load(atomic::Ordering::SeqCst), 1);

    // The first write finishes. One step collects its result and, in the same call, sends the
    // events that waited as the second write.
    gate.grant(1);
    let started = time::Instant::now();
    while !ev.log.is_empty() {
        writer.step(&mut ev, 0);
        assert!(
            started.elapsed() < WAIT,
            "the waiting events were never sent"
        );
        thread::sleep(POLL);
    }
    wait_for_the_gate(&gate, 2);
    assert_eq!(written(&files), ["e0", "e1"]);
    gate.grant(1);
    settle(&mut writer, &mut ev);
    assert_eq!(written(&files), ["e0", "e1", "e2", "e3"]);
}

// ---- Flush, at shutdown ----

#[test]
fn flush_writes_everything_and_waits_even_when_it_takes_several_writes() {
    let files = fake_files::FakeFiles::new();
    let mut writer = writer_on(files.clone());
    let mut ev = ev::Ev::new();
    let count = 2 * FIT + 3;
    queue(&mut ev, 0, count);
    writer.flush(&mut ev, 0);
    assert!(ev.log.is_empty());
    assert!(!writer.busy());
    assert_eq!(written(&files).len(), count);
}

#[test]
fn flush_with_nothing_to_write_returns_at_once() {
    let mut writer = writer_on(fake_files::FakeFiles::new());
    let mut ev = ev::Ev::new();
    let started = time::Instant::now();
    writer.flush(&mut ev, 0);
    assert!(started.elapsed() < time::Duration::from_secs(1));
    assert!(!writer.busy());
}

#[test]
fn flush_waits_for_a_write_that_is_in_flight_and_then_sends_what_waited() {
    let gate = Gate::new();
    let files = fake_files::FakeFiles::new();
    let mut writer = writer_on(GatedFiles {
        inner: files.clone(),
        gate: sync::Arc::clone(&gate),
    });
    let mut ev = ev::Ev::new();
    queue(&mut ev, 0, 1);
    writer.step(&mut ev, 0);
    wait_for_the_gate(&gate, 1);
    queue(&mut ev, 1, 1);

    // Let both writes through from another thread, a little later.
    let opener = {
        let gate = sync::Arc::clone(&gate);
        thread::spawn(move || {
            thread::sleep(time::Duration::from_millis(50));
            gate.grant(2);
        })
    };
    writer.flush(&mut ev, 0);
    opener.join().unwrap();
    assert_eq!(written(&files), ["e0", "e1"]);
    assert!(!writer.busy());
}

#[test]
fn flush_gives_up_on_a_write_that_takes_longer_than_the_timeout() {
    // This one takes the whole timeout (`EVENT_LOG_TIMEOUT_US`, 5 seconds by default) of real time.
    let _guard = capture();
    let gate = Gate::new();
    let files = fake_files::FakeFiles::new();
    let mut writer = writer_on(GatedFiles {
        inner: files.clone(),
        gate: sync::Arc::clone(&gate),
    });
    let mut ev = ev::Ev::new();
    queue(&mut ev, 0, 1);
    writer.step(&mut ev, 0);
    wait_for_the_gate(&gate, 1);
    queue(&mut ev, 1, 1);

    let started = time::Instant::now();
    writer.flush(&mut ev, 0);
    let waited = started.elapsed();

    assert!(
        waited >= time::Duration::from_micros(log_writer::EVENT_LOG_TIMEOUT_US as u64),
        "it gave up early, after {waited:?}"
    );
    assert!(writer.busy(), "the stuck write is still with the worker");
    assert_eq!(ev.log.len(), 1, "the event that waited was not sent");
    assert_eq!(lines_with("gave up waiting for the last write"), 1);

    // Let the worker finish, so it does not outlive the test.
    gate.grant(1);
}

// ---- Failures ----

#[test]
fn a_streak_of_failed_writes_is_reported_once_and_a_success_ends_the_streak() {
    let _guard = capture();
    let flaky = Flaky::new();
    let files = flaky.inner.clone();
    let mut writer = writer_on(flaky.clone());
    let mut ev = ev::Ev::new();

    // Three writes fail in a row: one report.
    flaky.fail(true);
    for n in 0..3 {
        queue(&mut ev, n, 1);
        settle(&mut writer, &mut ev);
    }
    assert_eq!(lines_with("a batch of events was not written"), 1);
    assert_eq!(
        written(&files),
        Vec::<String>::new(),
        "a failed batch is lost"
    );

    // A write works: nothing to report, and the streak is over.
    flaky.fail(false);
    queue(&mut ev, 3, 1);
    settle(&mut writer, &mut ev);
    assert_eq!(written(&files), ["e3"]);
    assert_eq!(lines_with("a batch of events was not written"), 1);

    // The next failure starts a new streak: a second report.
    flaky.fail(true);
    queue(&mut ev, 4, 1);
    settle(&mut writer, &mut ev);
    assert_eq!(lines_with("a batch of events was not written"), 2);
}

#[test]
fn after_a_failure_the_writer_carries_on_with_the_events_that_come_next() {
    let _guard = capture();
    let flaky = Flaky::new();
    let files = flaky.inner.clone();
    let mut writer = writer_on(flaky.clone());
    let mut ev = ev::Ev::new();

    flaky.fail(true);
    queue(&mut ev, 0, 2);
    settle(&mut writer, &mut ev);
    flaky.fail(false);
    queue(&mut ev, 2, 2);
    settle(&mut writer, &mut ev);
    // The first batch (e0, e1) was lost, as designed; the writer did not stop.
    assert_eq!(written(&files), ["e2", "e3"]);
}

#[test]
fn a_failed_write_still_empties_the_queue() {
    let _guard = capture();
    let flaky = Flaky::new();
    let mut writer = writer_on(flaky.clone());
    let mut ev = ev::Ev::new();
    flaky.fail(true);
    queue(&mut ev, 0, FIT + 5);
    settle(&mut writer, &mut ev);
    assert!(ev.log.is_empty());
}
