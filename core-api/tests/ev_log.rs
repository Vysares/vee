//! Tests that `Ev::send` writes each event to the system log. This is its own test program because
//! it installs a logger for the whole program: it keeps every line in a list, and each test looks
//! for the lines of its own, found by a marker in the text, since tests run at the same time.

#![allow(
    clippy::unwrap_used,
    reason = "a test file: a failed unwrap is a failed test (`install` is not a `#[test]` fn, so \
              clippy.toml's exemption does not cover it)"
)]

use std::sync;
use vee_core_api::ev;
use vee_core_api::event;

#[derive(Debug, Clone, Copy, PartialEq, Eq, event::Code)]
#[repr(u16)]
enum SampleEv {
    #[severity(Info)]
    Started,
    #[severity(Warn)]
    Slow,
    #[severity(Err)]
    Failed,
}

/// A logger that keeps each line as `LEVEL target: message`.
struct Capture {
    lines: sync::Mutex<Vec<String>>,
}

impl log::Log for Capture {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        let line = format!("{} {}: {}", record.level(), record.target(), record.args());
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

/// Installs `CAPTURE` once for the whole program.
fn install() {
    static ONCE: sync::Once = sync::Once::new();
    ONCE.call_once(|| {
        log::set_logger(&CAPTURE).unwrap();
        log::set_max_level(log::LevelFilter::Trace);
    });
}

/// The lines that have `marker` in them.
fn lines_with(marker: &str) -> Vec<String> {
    CAPTURE
        .lines
        .lock()
        .unwrap_or_else(sync::PoisonError::into_inner)
        .iter()
        .filter(|line| line.contains(marker))
        .cloned()
        .collect()
}

#[test]
fn a_sent_event_is_written_with_its_source_code_time_and_text() {
    install();
    let mut queue = ev::Ev::new();
    queue.send(3, 10_200, SampleEv::Started, format_args!("marker-fields"));
    assert_eq!(
        lines_with("marker-fields"),
        ["INFO event: src=3 code=0 t=10200 us: marker-fields"]
    );
}

#[test]
fn the_log_level_follows_the_severity() {
    install();
    let mut queue = ev::Ev::new();
    queue.send(1, 0, SampleEv::Started, format_args!("marker-info"));
    queue.send(1, 0, SampleEv::Slow, format_args!("marker-warn"));
    queue.send(1, 0, SampleEv::Failed, format_args!("marker-err"));
    assert!(lines_with("marker-info")[0].starts_with("INFO "));
    assert!(lines_with("marker-warn")[0].starts_with("WARN "));
    assert!(lines_with("marker-err")[0].starts_with("ERROR "));
}

#[test]
fn text_that_was_cut_off_is_written_as_it_was_cut() {
    install();
    let mut queue = ev::Ev::new();
    let long = "y".repeat(event::EV_TEXT_MAX_BYTES + 10);
    queue.send(1, 0, SampleEv::Started, format_args!("marker-long {long}"));
    let line = &lines_with("marker-long")[0];
    // The event's text is the cut one, and it is what the line has.
    assert!(line.ends_with(&format!("{}~", "y".repeat(event::EV_TEXT_MAX_BYTES - 13))));
}

#[test]
fn an_event_the_full_queue_drops_is_still_written() {
    install();
    let mut queue = ev::Ev::new();
    for _ in 0..ev::EV_QUEUE_DEPTH {
        queue.send(1, 0, SampleEv::Started, format_args!("fill"));
    }
    assert_eq!(queue.take_dropped(), 0);

    queue.send(1, 0, SampleEv::Started, format_args!("marker-overflow"));

    assert_eq!(queue.take_dropped(), 1); // the queue did not take it
    assert_eq!(lines_with("marker-overflow").len(), 1); // but the log has it
}
