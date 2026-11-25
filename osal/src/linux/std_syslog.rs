//! The std system log: the logger behind the `log` crate, writing to stderr. The platform neutral
//! part (the line and its format) is in `syslog`.
//!
//! Who calls what:
//! - The executive's `main` calls `init` first, before anything can fail, because a `log::` call
//!   made before `init` is thrown away without any sign of it.
//! - Any code calls the `log::` macros. They check the level set by `init`, so a message below
//!   that level costs one atomic load and a branch, and then they call `Logger::log`.
//! - `Logger::log` checks the level and calls `Logger::emit`, which formats a `syslog::LineBuf`
//!   on the calling thread and then writes it to stderr (direct mode) or pushes it into the
//!   queue (queued mode).
//!
//! The two modes:
//! - Direct, from `init`: `emit` writes each line to stderr itself. That can block, which is
//!   acceptable at startup and in development, and it is the only way to see a very early failure.
//! - Queued, after `start_queue`: `emit` only pushes the line into a bounded queue, which never
//!   waits, dropping and counting the line when the queue is full. A writer thread takes lines
//!   out and writes them to stderr. It polls with `try_recv` and a sleep, and never blocks in
//!   `recv`: a blocked consumer makes every producer's push cost much more (it has to wake it).
//!
//! The executive's start sequence calls `start_queue` just before it sets `started`, as part of
//! the same function, so it cannot be forgotten. Starting the thread allocates, so it must come
//! before `started`. If it fails, the logger stays in direct mode and the executive reports it.

use crate::channel;
use crate::clock::Clock as _;
use crate::linux::std_clock;
use crate::syslog;
use crate::thread;
use std::fmt;
use std::io;
use std::io::Write as _;
use std::sync;
use std::sync::atomic;
use std::time;

/// How many lines the queue holds before `emit` starts dropping new ones. It is also the most
/// lines the writer takes in one pass, so it gets to report the dropped lines even when the
/// producers keep the queue full.
const QUEUE_DEPTH: usize = 1024;
/// How long the writer sleeps between passes.
const POLL: time::Duration = time::Duration::from_millis(10);

/// Why `start_queue` did not start the queued mode.
#[derive(Debug)]
pub enum StartError {
    /// `init` has not run, so there is no logger to switch.
    NotInstalled,
    /// The queued mode was started already.
    AlreadyStarted,
    /// The OS would not start the writer thread (a limit on threads or memory, say). The logger
    /// is still in direct mode.
    Thread(thread::SpawnError),
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartError::NotInstalled => f.write_str("the system log is not installed"),
            StartError::AlreadyStarted => f.write_str("the log queue was started already"),
            StartError::Thread(error) => write!(f, "log writer: {error}"),
        }
    }
}

/// The logger that `init` installs. `log` needs it to be `Sync`, because any thread may log.
struct Logger {
    clock: std_clock::StdClock,
    /// The queue's sending end. Empty means direct mode: `emit` writes to stderr itself. Once
    /// `start_queue` has filled it, `emit` pushes into it instead.
    queue: sync::OnceLock<channel::Sender<syslog::LineBuf>>,
    /// How many lines `emit` has dropped because the queue was full, not yet reported. It wraps
    /// after 4 billion, which is not a concern.
    dropped: atomic::AtomicU32,
}

impl Logger {
    fn new(clock: std_clock::StdClock) -> Self {
        Logger {
            clock,
            queue: sync::OnceLock::new(),
            dropped: atomic::AtomicU32::new(0),
        }
    }

    /// Switches to queued mode: makes the queue, starts the writer thread, and only then lets
    /// `emit` use the queue, so a line is never queued without a writer. On an error the logger
    /// stays as it was.
    ///
    /// The writer thread is not joined and never ends: it runs for the rest of the process,
    /// because the logger, which holds the queue's sending end, lives that long too. So the queue
    /// is never closed, and the writer does not look for it. It is called once, at startup, from
    /// one thread: a second call is refused before anything is made.
    fn start_queue(&'static self) -> Result<(), StartError> {
        if self.queue.get().is_some() {
            return Err(StartError::AlreadyStarted);
        }
        let (sender, receiver) = channel::channel(QUEUE_DEPTH);
        thread::spawn("vee-syslog", move || {
            let mut out = io::stderr();
            loop {
                self.drain(&receiver, &mut out);
                std::thread::sleep(POLL);
            }
        })
        .map_err(StartError::Thread)?;
        self.queue
            .set(sender)
            .map_err(|_| StartError::AlreadyStarted)
    }

    /// One pass of the writer: writes the lines that are queued (at most `QUEUE_DEPTH`) to
    /// `out`, then reports how many lines `emit` dropped since the last pass.
    ///
    /// The report is written straight to `out`, not through the queue, which may be full.
    #[allow(
        clippy::let_underscore_must_use,
        reason = "if the output cannot be written to there is nowhere else to report it"
    )]
    fn drain(&self, receiver: &channel::Receiver<syslog::LineBuf>, out: &mut impl io::Write) {
        for _ in 0..QUEUE_DEPTH {
            let Some(line) = receiver.try_recv() else {
                break;
            };
            let _ = out.write_all(line.as_bytes());
        }
        let dropped = self.dropped.swap(0, atomic::Ordering::Relaxed);
        if dropped > 0 {
            let report = syslog::LineBuf::from_line(&syslog::Line {
                time_us: self.clock.monotonic_us(),
                level: log::Level::Warn,
                target: module_path!(),
                message: format_args!("{dropped} lines dropped: the queue was full"),
            });
            let _ = out.write_all(report.as_bytes());
        }
    }

    /// Writes one record out, in the current mode. This is `log` without the level check.
    ///
    /// Both modes format the line the same way, into a `LineBuf`, on the calling thread. They
    /// differ only in what happens to the finished line.
    #[allow(
        clippy::let_underscore_must_use,
        reason = "if stderr cannot be written to there is nowhere else to report it, and \
                  `eprintln!` would panic and abort the process instead"
    )]
    fn emit(&self, record: &log::Record<'_>) {
        let buf = syslog::LineBuf::from_line(&syslog::Line {
            time_us: self.clock.monotonic_us(),
            level: record.level(),
            target: record.target(),
            message: *record.args(),
        });
        match self.queue.get() {
            // Direct mode: one write of the whole line. `stderr` is not buffered, and the lock
            // keeps the line from being mixed with one from another thread. The write can block,
            // which is acceptable before startup.
            None => {
                let _ = io::stderr().lock().write_all(buf.as_bytes());
            }
            // Queued mode: `try_send` never waits. If the queue is full it hands the line back, and
            // the line is dropped and counted. The oldest lines
            // stay, as with events: the first lines of a problem are the most useful.
            Some(queue) => {
                if queue.try_send(buf).is_err() {
                    self.dropped.fetch_add(1, atomic::Ordering::Relaxed);
                }
            }
        }
    }
}

impl log::Log for Logger {
    /// Whether a message at this level is kept. The `log::` macros already check this against the
    /// level set by `init`, but the `Log` trait asks each logger to check as well.
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            self.emit(record);
        }
    }

    /// Nothing to flush: stderr is not buffered.
    fn flush(&self) {}
}

/// There is one logger for the whole process. `log::set_logger` needs a `&'static` one, so it
/// lives here, and `OnceLock` fills it in the first time `init` runs.
static LOGGER: sync::OnceLock<Logger> = sync::OnceLock::new();

/// Installs the system log, and keeps messages at `level` and less verbose (`Info` keeps `Error`,
/// `Warn` and `Info`). Call it first thing in `main`.
///
/// `clock` times the lines. Give it a copy of the same `StdClock` the executive uses, so a line's
/// time and an event's `monotonic_us` count from the same start.
///
/// Fails if a logger is already installed, which leaves that logger and its level alone. The
/// caller decides what that means: for the executive it is a bug, and for a test program that runs
/// many tests it is expected.
pub fn init(
    clock: std_clock::StdClock,
    level: log::LevelFilter,
) -> Result<(), log::SetLoggerError> {
    let logger = LOGGER.get_or_init(|| Logger::new(clock));
    log::set_logger(logger)?;
    log::set_max_level(level);
    Ok(())
}

/// Switches the installed logger to queued mode (see the module doc). Call it from the
/// executive's start sequence, after `init` and before `started` is set, and report an error:
/// the logger is still usable, in direct mode.
pub fn start_queue() -> Result<(), StartError> {
    LOGGER.get().ok_or(StartError::NotInstalled)?.start_queue()
}

#[cfg(test)]
mod tests {
    use super as std_syslog;
    use crate::channel;
    use crate::linux::std_alloc_guard;
    use crate::linux::std_clock;
    use crate::syslog;
    use log::Log as _;
    use std::fmt;
    use std::io;
    use std::sync::atomic;

    /// A logger in queued mode with a queue of `depth` lines, and the receiving end. It is not the
    /// global logger, and tests call `emit` on it, which skips the global level check.
    fn queued_logger(depth: usize) -> (std_syslog::Logger, channel::Receiver<syslog::LineBuf>) {
        let (sender, receiver) = channel::channel(depth);
        let logger = std_syslog::Logger::new(std_clock::StdClock::new());
        assert!(logger.queue.set(sender).is_ok());
        (logger, receiver)
    }

    fn emit(logger: &std_syslog::Logger, message: fmt::Arguments<'_>) {
        let record = log::Record::builder()
            .level(log::Level::Info)
            .target("t")
            .args(message)
            .build();
        logger.emit(&record);
    }

    #[test]
    fn in_queued_mode_a_line_is_pushed_and_not_counted_as_dropped() {
        let (logger, receiver) = queued_logger(4);
        emit(&logger, format_args!("hello {}", 7));
        let buf = receiver.try_recv().unwrap();
        assert!(buf.as_bytes().ends_with(b" INFO  t: hello 7\n"));
        assert_eq!(logger.dropped.load(atomic::Ordering::Relaxed), 0);
    }

    #[test]
    fn a_full_queue_drops_the_new_lines_counts_them_and_does_not_wait() {
        let (logger, receiver) = queued_logger(1);
        emit(&logger, format_args!("one"));
        emit(&logger, format_args!("two")); // the queue is full: this returns at once
        emit(&logger, format_args!("three"));
        assert_eq!(logger.dropped.load(atomic::Ordering::Relaxed), 2);
        // The oldest line is the one kept.
        let buf = receiver.try_recv().unwrap();
        assert!(buf.as_bytes().ends_with(b"t: one\n"));
        assert!(receiver.try_recv().is_none());
    }

    /// One writer pass into a buffer, and what was written.
    fn drained(
        logger: &std_syslog::Logger,
        receiver: &channel::Receiver<syslog::LineBuf>,
    ) -> String {
        let mut out = Vec::new();
        logger.drain(receiver, &mut out);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn a_pass_writes_the_queued_lines_in_order() {
        let (logger, receiver) = queued_logger(4);
        emit(&logger, format_args!("a"));
        emit(&logger, format_args!("b"));
        let text = drained(&logger, &receiver);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].ends_with("t: a"));
        assert!(lines[1].ends_with("t: b"));
    }

    #[test]
    fn a_pass_reports_the_dropped_lines_once() {
        let (logger, receiver) = queued_logger(1);
        for message in ["one", "two", "three"] {
            emit(&logger, format_args!("{message}"));
        }
        let text = drained(&logger, &receiver);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].ends_with("t: one"));
        assert!(lines[1].ends_with("2 lines dropped: the queue was full"));
        assert!(lines[1].contains(" WARN "));
        // The count was taken, so the next pass has nothing to report.
        assert_eq!(logger.dropped.load(atomic::Ordering::Relaxed), 0);
        let text = drained(&logger, &receiver);
        assert_eq!(text, "");
    }

    #[test]
    fn a_pass_writes_at_most_a_queue_full_of_lines() {
        let extra = 10;
        let (logger, receiver) = queued_logger(std_syslog::QUEUE_DEPTH + extra);
        for _ in 0..std_syslog::QUEUE_DEPTH + extra {
            emit(&logger, format_args!("x"));
        }
        let text = drained(&logger, &receiver);
        assert_eq!(text.lines().count(), std_syslog::QUEUE_DEPTH);
        let mut left = 0;
        while receiver.try_recv().is_some() {
            left += 1;
        }
        assert_eq!(left, extra); // left for the next pass
    }

    #[test]
    fn emit_in_queued_mode_does_not_allocate() {
        let (logger, _receiver) = queued_logger(4);
        let before = std_alloc_guard::allocations_on_this_thread();
        emit(&logger, format_args!("hello {}", 7));
        let after = std_alloc_guard::allocations_on_this_thread();
        assert_eq!(after, before);
    }

    #[test]
    fn emit_in_direct_mode_does_not_allocate() {
        let logger = std_syslog::Logger::new(std_clock::StdClock::new());
        // The first write to stderr may set up the standard library's own state, so write once
        // first and count the second.
        emit(&logger, format_args!("warm up"));
        let before = std_alloc_guard::allocations_on_this_thread();
        emit(&logger, format_args!("hello {}", 7));
        let after = std_alloc_guard::allocations_on_this_thread();
        assert_eq!(after, before);
    }

    #[test]
    fn a_pass_does_not_allocate() {
        let (logger, receiver) = queued_logger(1);
        emit(&logger, format_args!("one"));
        emit(&logger, format_args!("two")); // dropped, so the pass also writes the report
        let mut out = io::sink(); // a `Vec` would allocate as it grows
        let before = std_alloc_guard::allocations_on_this_thread();
        logger.drain(&receiver, &mut out);
        let after = std_alloc_guard::allocations_on_this_thread();
        assert_eq!(after, before);
    }

    #[test]
    fn start_queue_switches_to_queued_mode_once() {
        // `start_queue` needs a `&'static` logger, as the global one is. Leaking one in a test is
        // fine. This does not touch the global logger.
        let logger: &'static std_syslog::Logger = Box::leak(Box::new(std_syslog::Logger::new(
            std_clock::StdClock::new(),
        )));
        assert!(logger.queue.get().is_none());
        assert!(logger.start_queue().is_ok());
        assert!(logger.queue.get().is_some());
        assert!(matches!(
            logger.start_queue(),
            Err(std_syslog::StartError::AlreadyStarted)
        ));
    }

    #[test]
    fn init_installs_the_logger_once_and_sets_the_level() {
        // This is the only test in this program that installs the global logger.
        let first = std_syslog::init(std_clock::StdClock::new(), log::LevelFilter::Info);
        assert!(first.is_ok());
        assert_eq!(log::max_level(), log::LevelFilter::Info);

        // A second `init` is refused, and the first logger and level stay.
        let second = std_syslog::init(std_clock::StdClock::new(), log::LevelFilter::Trace);
        assert!(second.is_err());
        assert_eq!(log::max_level(), log::LevelFilter::Info);

        // `enabled` follows the level: Info and more severe are kept, Debug is not.
        let logger = std_syslog::LOGGER.get().unwrap();
        let at = |level| logger.enabled(&log::Metadata::builder().level(level).build());
        assert!(at(log::Level::Error));
        assert!(at(log::Level::Info));
        assert!(!at(log::Level::Debug));
    }

    #[test]
    fn each_start_error_says_what_went_wrong() {
        assert_eq!(
            std_syslog::StartError::NotInstalled.to_string(),
            "the system log is not installed"
        );
        assert_eq!(
            std_syslog::StartError::AlreadyStarted.to_string(),
            "the log queue was started already"
        );
    }
}
