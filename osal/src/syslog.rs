//! The system log, the platform neutral part: what a line is, and how one is formatted. It uses
//! only `core`. A platform's logger lives in its own file (`std_syslog`) and uses these types.
//!
//! The system log is free-form text for developers, and for failures that the event system cannot
//! report (an event log that will not open, say). It is the lowest level of output Vee makes.
//! Real events come first, and the system log goes to the ground only when it is needed.
//!
//! The interface is the `log` crate's: any crate writes `log::info!("...")`, and `log` hands the
//! message to the one logger that is installed. Code does not need a `Ctx` for it: a module's data
//! goes through `ctx` because that data has types, and a log line is only text.
//!
//! What a platform provides is a `log::Log` that turns each record into a `LineBuf` with
//! `LineBuf::from_line`, and then delivers the bytes where that platform wants them. A new
//! platform needs only that, and a way to install it.

use core::fmt;
use core::fmt::Write as _;

const TIME_WIDTH: usize = 12;
const LEVEL_WIDTH: usize = 5;

/// The most bytes in one finished line, including the ending (see `LineBuf`).
const LINE_MAX: usize = 256;
/// The most bytes of text in a line. The last two bytes of `LINE_MAX` are kept for the ending.
const TEXT_MAX: usize = LINE_MAX.saturating_sub(2);

/// One line of the log. `Display` writes it, so the same line can be printed to stderr, written
/// into a fixed size buffer, or compared as text in a test, with no allocation of its own.
///
/// The text is `<time> <level> <target>: <message>`, for example
/// `       10100 INFO  vee_core::reset_log: reset 1: prev end unknown, 0 unclean in a row`.
/// - `time_us` is the monotonic time in microseconds, the same unit as an event's `monotonic_us`,
///   right aligned so the columns line up. A larger number is printed in full.
/// - `target` is where the message came from: by default the module path of the `log::` call.
pub struct Line<'a> {
    pub time_us: u64,
    pub level: log::Level,
    pub target: &'a str,
    pub message: fmt::Arguments<'a>,
}

impl fmt::Display for Line<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:>TIME_WIDTH$} {:<LEVEL_WIDTH$} {}: {}",
            self.time_us, self.level, self.target, self.message
        )
    }
}

/// One finished line, in a fixed size buffer, so queuing it needs no allocation. The bytes are the
/// line's text, then `~` if the text was too long and was cut off, then a newline. So the
/// buffer can be written to stderr or to a file as it is.
///
/// `from_line` makes one. It writes the text through `fmt::Write` (`write_str`, once for each piece
/// the formatter produces) and then `finish` adds the ending.
pub struct LineBuf {
    bytes: [u8; LINE_MAX],
    /// How many bytes of `bytes` are used.
    len: usize,
    /// Set when some text did not fit. After that, nothing more is added to the text.
    cut: bool,
}

impl LineBuf {
    fn new() -> Self {
        LineBuf {
            bytes: [0; LINE_MAX],
            len: 0,
            cut: false,
        }
    }

    /// The finished line, formatted from `line`.
    #[allow(
        clippy::let_underscore_must_use,
        reason = "writing into a `LineBuf` never fails (`write_str` always returns `Ok`)"
    )]
    pub fn from_line(line: &Line<'_>) -> Self {
        let mut buf = LineBuf::new();
        let _ = write!(buf, "{line}");
        buf.finish();
        buf
    }

    /// The bytes of the finished line, ending with a newline.
    pub fn as_bytes(&self) -> &[u8] {
        self.bytes.get(..self.len).unwrap_or_default()
    }

    /// Adds the ending: `~` if the text was cut, then a newline. `write_str` kept two bytes free
    /// for them, so they always fit.
    fn finish(&mut self) {
        let ending: &[u8] = if self.cut { b"~\n" } else { b"\n" };
        let end = self.len.saturating_add(ending.len());
        if let Some(to) = self.bytes.get_mut(self.len..end) {
            to.copy_from_slice(ending);
            self.len = end;
        }
    }
}

impl fmt::Write for LineBuf {
    /// Appends as much of `s` as fits in `TEXT_MAX`, counting whole characters only, so the text
    /// stays valid. If something does not fit, the line is marked as cut and later pieces are
    /// ignored. This still returns `Ok`: an `Err` would stop the formatting, and a cut off line is
    /// better than none.
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if self.cut {
            return Ok(());
        }
        let room = TEXT_MAX.saturating_sub(self.len);
        let mut take: usize = 0;
        for c in s.chars() {
            let next = take.saturating_add(c.len_utf8());
            if next > room {
                break;
            }
            take = next;
        }
        let end = self.len.saturating_add(take);
        if let (Some(to), Some(from)) =
            (self.bytes.get_mut(self.len..end), s.as_bytes().get(..take))
        {
            to.copy_from_slice(from);
            self.len = end;
        }
        if take < s.len() {
            self.cut = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super as syslog;
    use core::fmt::Write as _;

    /// A line made from these parts, as text.
    fn text(
        time_us: u64,
        level: log::Level,
        target: &str,
        message: core::fmt::Arguments<'_>,
    ) -> String {
        syslog::Line {
            time_us,
            level,
            target,
            message,
        }
        .to_string()
    }

    #[test]
    fn a_line_has_the_time_the_level_the_target_and_the_message() {
        let line = text(
            10_100,
            log::Level::Info,
            "vee_core::suppressed",
            format_args!("hello {}", 7),
        );
        assert_eq!(line, "       10100 INFO  vee_core::suppressed: hello 7");
    }

    #[test]
    fn every_level_puts_the_target_in_the_same_column() {
        let levels = [
            log::Level::Error,
            log::Level::Warn,
            log::Level::Info,
            log::Level::Debug,
            log::Level::Trace,
        ];
        for level in levels {
            let line = text(1, level, "target", format_args!("m"));
            assert_eq!(line.find("target"), Some(19), "level {level}: {line:?}");
        }
    }

    #[test]
    fn a_time_wider_than_the_column_is_printed_in_full() {
        let line = text(u64::MAX, log::Level::Warn, "t", format_args!("m"));
        assert_eq!(line, "18446744073709551615 WARN  t: m");
    }

    #[test]
    fn the_message_is_written_as_given() {
        let line = text(0, log::Level::Error, "t", format_args!("a: b {{c}}"));
        assert!(line.ends_with("t: a: b {c}"), "{line:?}");
    }

    #[test]
    fn a_finished_line_is_the_text_pieces_in_order_then_a_newline() {
        let mut buf = syslog::LineBuf::new();
        write!(buf, "ab{}", 12).unwrap();
        write!(buf, "!").unwrap();
        buf.finish();
        assert_eq!(buf.as_bytes(), b"ab12!\n");
    }

    #[test]
    fn from_line_makes_the_finished_line() {
        let buf = syslog::LineBuf::from_line(&syslog::Line {
            time_us: 5,
            level: log::Level::Warn,
            target: "t",
            message: format_args!("m"),
        });
        assert_eq!(buf.as_bytes(), b"           5 WARN  t: m\n");
    }

    #[test]
    fn text_that_just_fits_is_not_marked_as_cut() {
        let mut buf = syslog::LineBuf::new();
        buf.write_str(&"x".repeat(syslog::TEXT_MAX)).unwrap();
        buf.finish();
        assert_eq!(buf.len, syslog::LINE_MAX - 1); // the text and the newline, no `~`
        assert!(buf.as_bytes().ends_with(b"x\n"));
    }

    #[test]
    fn a_long_line_is_cut_at_a_whole_character_and_ends_with_a_tilde() {
        let mut buf = syslog::LineBuf::new();
        buf.write_str("a").unwrap();
        // Each "é" is 2 bytes, and TEXT_MAX is 254. 126 of them make 253 bytes with the "a", and
        // the 127th would need byte 255, so it is left out whole.
        for _ in 0..200 {
            buf.write_str("é").unwrap();
        }
        assert_eq!(buf.len, 1 + 2 * 126);
        // Nothing is added after a cut, even a piece that would fit in the byte that is left.
        buf.write_str("m").unwrap();
        assert_eq!(buf.len, 1 + 2 * 126);
        buf.finish();
        assert_eq!(buf.len, 1 + 2 * 126 + 2);
        assert!(buf.as_bytes().ends_with("é~\n".as_bytes()));
        assert!(buf.len <= syslog::LINE_MAX);
    }
}
