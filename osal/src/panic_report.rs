//! The panic report: the one system log line that says where and why the process panicked. This
//! file only builds the line, from plain parts, and uses only `core`. The panic hook that calls it
//! (`std_panic`) gets the parts out of std's panic information.
//!
//! It is the "last gasp": a panic aborts the process, and this line is what is left to read. It has
//! the same format and clock as every other system log line, so it sits in order among them.
//!
//! Who calls what: the hook calls `line` with what it found, then writes the result to stderr
//! in one write. Nothing here allocates, locks or can fail, because the hook runs in an unknown
//! state (see "Faults" in the design).

use crate::syslog;

/// The target of the report line, so it is easy to find: `grep ' panic: '`.
const TARGET: &str = "panic";

/// Shown where the panic did not tell us something. The angle brackets cannot be mistaken for a
/// real thread name, file or message.
const UNNAMED_THREAD: &str = "<unnamed>";
const UNKNOWN_FILE: &str = "<unknown location>";
const NO_TEXT_MESSAGE: &str = "<message is not text>";

/// Where in the source the panic happened. These are the values of `std::panic::Location`.
pub struct Location<'a> {
    pub file: &'a str,
    pub line: u32,
    pub column: u32,
}

/// The finished report line, at monotonic time `time_us`.
///
/// Each part may be missing, and the line says so with a placeholder rather than leaving it out,
/// so the line always has the same shape:
/// `<time> ERROR panic: thread '<thread>' panicked at <file>:<line>:<column>: <message>`.
///
/// A message that has several lines (an `assert_eq!` failure has) keeps them: the report is one
/// write, so its lines stay together. A line longer than the system log's limit is cut off and
/// marked, as any other (see `syslog::LineBuf`).
pub fn line(
    time_us: u64,
    thread: Option<&str>,
    location: Option<&Location<'_>>,
    message: Option<&str>,
) -> syslog::LineBuf {
    let thread = thread.unwrap_or(UNNAMED_THREAD);
    let message = message.unwrap_or(NO_TEXT_MESSAGE);
    let text = match location {
        Some(at) => format_args!(
            "thread '{thread}' panicked at {}:{}:{}: {message}",
            at.file, at.line, at.column
        ),
        None => format_args!("thread '{thread}' panicked at {UNKNOWN_FILE}: {message}"),
    };
    syslog::LineBuf::from_line(&syslog::Line {
        time_us,
        level: log::Level::Error,
        target: TARGET,
        message: text,
    })
}

#[cfg(test)]
mod tests {
    use super as panic_report;

    /// The report as text, without the newline at the end.
    fn report(
        thread: Option<&str>,
        location: Option<&panic_report::Location<'_>>,
        message: Option<&str>,
    ) -> String {
        let buf = panic_report::line(10_100, thread, location, message);
        let text = String::from_utf8(buf.as_bytes().to_vec()).unwrap();
        text.strip_suffix('\n').unwrap().to_string()
    }

    fn at() -> panic_report::Location<'static> {
        panic_report::Location {
            file: "core/src/lib.rs",
            line: 42,
            column: 9,
        }
    }

    #[test]
    fn the_line_has_the_thread_the_location_and_the_message() {
        let text = report(Some("main"), Some(&at()), Some("index out of bounds"));
        assert_eq!(
            text,
            "       10100 ERROR panic: thread 'main' panicked at core/src/lib.rs:42:9: index out of bounds"
        );
    }

    #[test]
    fn a_missing_thread_name_location_and_message_show_a_placeholder() {
        let text = report(None, None, None);
        assert_eq!(
            text,
            "       10100 ERROR panic: thread '<unnamed>' panicked at <unknown location>: <message is not text>"
        );
    }

    #[test]
    fn each_part_is_replaced_on_its_own() {
        let text = report(Some("vee-syslog"), None, Some("boom"));
        assert!(text.ends_with("thread 'vee-syslog' panicked at <unknown location>: boom"));
        let text = report(None, Some(&at()), Some("boom"));
        assert!(text.ends_with("thread '<unnamed>' panicked at core/src/lib.rs:42:9: boom"));
    }

    #[test]
    fn the_line_ends_with_one_newline() {
        let buf = panic_report::line(0, Some("main"), Some(&at()), Some("boom"));
        assert!(buf.as_bytes().ends_with(b"boom\n"));
        assert_eq!(buf.as_bytes().iter().filter(|&&b| b == b'\n').count(), 1);
    }

    #[test]
    fn a_message_with_several_lines_keeps_them() {
        let text = report(Some("main"), Some(&at()), Some("left: 1\n right: 2"));
        assert!(text.ends_with("42:9: left: 1\n right: 2"));
    }

    #[test]
    fn a_very_long_message_is_cut_off_and_marked_and_the_location_is_kept() {
        let long = "x".repeat(1000);
        let buf = panic_report::line(0, Some("main"), Some(&at()), Some(&long));
        let text = String::from_utf8(buf.as_bytes().to_vec()).unwrap();
        assert!(text.contains("panicked at core/src/lib.rs:42:9: xxx"));
        assert!(text.ends_with("x~\n"));
        assert_eq!(buf.as_bytes().len(), 256);
    }
}
