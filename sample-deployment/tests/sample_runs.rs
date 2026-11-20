//! The sample deployment, run as a program: the real binary, in a scratch directory, with the
//! real files and the real clock. What these tests check is what a person sees running it by
//! hand: the logs on stderr, the two log files it leaves, and what the next boot makes of how the
//! last run ended.
//!
//! `VEE_SAMPLE_TICKS=N` makes a run stop cleanly after N ticks (10 ms each). A run with no limit
//! is killed instead, which is how a crash looks to the next boot.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code: a failure should panic, and the helpers are not `#[test]` fns, so \
              clippy.toml's exemption does not cover them"
)]

use std::fs;
use std::path;
use std::process;
use std::thread;
use std::time;
use vee_core_api::event;
use vee_core_types::event_log;
use vee_core_types::reset_log;

/// The compiled sample deployment. Cargo builds it before these tests run and gives its path.
const SAMPLE: &str = env!("CARGO_BIN_EXE_vee-sample-deployment");

/// The file names the sample writes in its working directory.
const RESET_LOG: &str = "reset.log";
const EVENT_LOG: &str = "events.log";

/// The sample's module sources and the event codes used below (see `vee-example` and the core).
const CORE_SOURCE: u8 = 0;
const EXAMPLE_SOURCE: u8 = 1;
const RESET_REPORT: u16 = 0;
const EXAMPLE_STARTED: u16 = 0;
const EXAMPLE_STEPPED: u16 = 1;

/// How long to wait for a started program to show that it has started, and how often to look.
const START_TIMEOUT: time::Duration = time::Duration::from_secs(10);
const POLL: time::Duration = time::Duration::from_millis(10);

/// A new empty directory for one test, deleted when it goes out of scope. Each test passes its own
/// name, so tests running in parallel do not share a directory.
struct TempDir {
    dir: path::PathBuf,
}

#[allow(
    clippy::let_underscore_must_use,
    reason = "test cleanup: a failure to delete, or nothing to delete, does not matter"
)]
fn remove_dir(dir: &path::Path) {
    let _ = fs::remove_dir_all(dir);
}

impl TempDir {
    fn new(test_name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("vee-sample-{}-{test_name}", process::id()));
        remove_dir(&dir);
        fs::create_dir_all(&dir).unwrap();
        TempDir { dir }
    }

    fn file(&self, name: &str) -> path::PathBuf {
        self.dir.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        remove_dir(&self.dir);
    }
}

/// The sample, set up to run in `dir`, with the variables systemd would set removed, so a test run
/// from a service behaves the same as one from a shell.
fn sample(dir: &TempDir) -> process::Command {
    let mut command = process::Command::new(SAMPLE);
    command
        .current_dir(&dir.dir)
        .env_remove("NOTIFY_SOCKET")
        .env_remove("WATCHDOG_USEC")
        .env_remove("WATCHDOG_PID")
        .env_remove("VEE_SAMPLE_TICKS");
    command
}

/// Runs the sample for `ticks` ticks, and returns what it printed to stderr (the system log).
/// The run must end well.
fn run(dir: &TempDir, ticks: u32) -> String {
    let output = sample(dir)
        .env("VEE_SAMPLE_TICKS", ticks.to_string())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "the run failed: {stderr}");
    stderr
}

/// Every event in the event log, oldest first.
fn events(dir: &TempDir) -> Vec<event::Event> {
    let bytes = fs::read(dir.file(EVENT_LOG)).unwrap();
    assert_eq!(bytes.len() % event_log::RECORD_LEN, 0, "a partial record");
    bytes
        .chunks(event_log::RECORD_LEN)
        .map(|record| event_log::decode(record).unwrap())
        .collect()
}

/// How many bytes the reset log holds. A boot is one record, and a clean shutdown another.
fn reset_log_len(dir: &TempDir) -> usize {
    fs::read(dir.file(RESET_LOG)).unwrap().len()
}

/// The text of the event from `source` with `code`, if there is one.
fn text_of(events: &[event::Event], source: u8, code: u16) -> Option<String> {
    events
        .iter()
        .find(|event| event.source == source && event.code == code)
        .map(|event| String::from_utf8_lossy(event.text.as_bytes()).into_owned())
}

#[test]
fn a_run_that_ends_cleanly_is_seen_as_clean_by_the_next_boot() {
    let dir = TempDir::new("a_run_that_ends_cleanly");

    let first = run(&dir, 3);
    assert!(first.contains("reset 1: prev end unknown"), "{first}");
    // The boot and the clean shutdown: two records.
    assert_eq!(reset_log_len(&dir), 2 * reset_log::RECORD_LEN);

    let second = run(&dir, 3);
    assert!(second.contains("reset 2: prev end commanded"), "{second}");
    assert_eq!(reset_log_len(&dir), 4 * reset_log::RECORD_LEN);

    // Both runs' events are in the one event log, each starting with the core's reset report, and
    // the example module's `init` saw how the run before it ended.
    let events = events(&dir);
    let reports: Vec<_> = events
        .iter()
        .filter(|event| event.source == CORE_SOURCE && event.code == RESET_REPORT)
        .collect();
    assert_eq!(reports.len(), 2);
    let started: Vec<String> = events
        .iter()
        .filter(|event| event.source == EXAMPLE_SOURCE && event.code == EXAMPLE_STARTED)
        .map(|event| String::from_utf8_lossy(event.text.as_bytes()).into_owned())
        .collect();
    assert_eq!(
        started,
        [
            "started, previous end: unknown",
            "started, previous end: commanded"
        ]
    );
}

#[test]
fn the_example_module_steps_once_a_second() {
    let dir = TempDir::new("the_example_module_steps");
    // 120 ticks of 10 ms: the module is due on every 100th, so it steps once.
    let stderr = run(&dir, 120);
    assert!(stderr.contains("step 1 at"), "{stderr}");
    let events = events(&dir);
    let step = text_of(&events, EXAMPLE_SOURCE, EXAMPLE_STEPPED).unwrap();
    assert!(step.starts_with("step 1 at "), "{step}");
    assert_eq!(
        events
            .iter()
            .filter(|event| event.source == EXAMPLE_SOURCE && event.code == EXAMPLE_STEPPED)
            .count(),
        1
    );
}

#[test]
fn a_run_that_is_killed_is_seen_as_unclean_by_the_next_boot() {
    let dir = TempDir::new("a_run_that_is_killed");

    // No tick limit: it runs until it is killed. Wait until it has written its boot record, which
    // is the first thing the core does, and then kill it, as a crash or a power cut would.
    let mut child = sample(&dir).stderr(process::Stdio::null()).spawn().unwrap();
    let started = time::Instant::now();
    while fs::read(dir.file(RESET_LOG)).map_or(0, |bytes| bytes.len()) < reset_log::RECORD_LEN {
        assert!(started.elapsed() < START_TIMEOUT, "it never started");
        thread::sleep(POLL);
    }
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(reset_log_len(&dir), reset_log::RECORD_LEN); // the boot, and no shutdown

    let next = run(&dir, 3);
    assert!(
        next.contains("reset 2: prev end unknown, 1 unclean in a row"),
        "{next}"
    );
}

#[test]
fn a_tick_limit_that_is_not_a_number_fails_loudly() {
    let dir = TempDir::new("a_tick_limit_that_is_not_a_number");
    let output = sample(&dir)
        .env("VEE_SAMPLE_TICKS", "soon")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("InvalidTickLimit"), "{stderr}");
    // It stopped before it started anything: no log files.
    assert!(!dir.file(RESET_LOG).exists());
}
