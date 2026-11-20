//! The panic hook: what runs when any thread panics, before the process aborts (in flight builds,
//! `panic = "abort"`). It is the "last gasp": one system log line saying which thread panicked,
//! where, and why. The line is built by `panic_report`, which uses only `core`. This file is the
//! part that needs std: it gets the facts out of std's panic information and writes the line.
//!
//! Who calls what:
//! - The executive's `main` calls `install` first thing, next to `std_syslog::init`, and only in
//!   flight builds (so a test that expects a panic does not leave `PANICKING` set). A panic
//!   before `install` runs std's default hook, which prints the same facts in its own format.
//! - After `install`, std calls `hook` (the function below) on the panicking thread, for every
//!   panic in the process. It is not called by any Vee code.
//! - Health's watchdog feed calls `is_panicking`, and stops feeding once it is true.
//!
//! The rules for the hook (see "Faults" in the design): it runs in an unknown state, so it does not
//! allocate, never panics, and writes one bounded line in one write. A panic inside the hook
//! aborts the process at once (std does that), which is the right outcome.
//!
//! Two threads can panic at once. Both run the hook, and each writes its line in one write, so the
//! lines do not mix. Whichever finishes first aborts the process, so the other's line may be
//! missing. That is accepted: it is usually one cause and its consequence.

use crate::clock::Clock as _;
use crate::linux::std_clock;
use crate::panic_report;
use std::io;
use std::io::Write as _;
use std::panic;
use std::sync;
use std::sync::atomic;
use std::thread;

/// Set by the hook as its first act, never cleared. The watchdog feed reads it (`is_panicking`),
/// so a hook that hangs, on a thread other than the one that feeds, stops the feed and the
/// supervisor restarts the process. Without it the main loop would keep feeding a process that
/// is already dying.
static PANICKING: atomic::AtomicBool = atomic::AtomicBool::new(false);

/// The clock the report's time comes from. The hook is a plain function, so it cannot hold the
/// clock itself: `install` leaves a copy here. It must count from the same start as the system
/// log's clock and the events' `monotonic_us`, so the line fits among them.
static CLOCK: sync::OnceLock<std_clock::StdClock> = sync::OnceLock::new();

/// Why `install` did not install the hook.
#[derive(Debug, PartialEq, Eq)]
pub enum InstallError {
    /// `install` ran before. There is one hook for the process, and the first one stays.
    AlreadyInstalled,
}

/// Whether a panic has started. Once true it stays true: the process is going to abort.
pub fn is_panicking() -> bool {
    PANICKING.load(atomic::Ordering::Acquire)
}

/// Installs the hook. `clock` times the report line: give it a copy of the clock the system log
/// got, so both count from the same start.
///
/// Installing allocates nothing. `Box::new(hook)` would allocate for a closure that holds data,
/// but `hook` is a plain function with no data, a zero sized type, and boxing one allocates
/// nothing.
pub fn install(clock: std_clock::StdClock) -> Result<(), InstallError> {
    CLOCK
        .set(clock)
        .map_err(|_| InstallError::AlreadyInstalled)?;
    panic::set_hook(Box::new(hook));
    Ok(())
}

/// The hook itself. It only gathers the facts and writes the line: building the line is
/// `panic_report::line`, which has its own tests.
///
/// The message is `payload_as_str`: the text of the panic, formatted, for a `panic!` with a
/// message. A panic with some other payload (`panic_any(42)`) has none, and the report says so.
#[allow(
    clippy::let_underscore_must_use,
    reason = "if stderr cannot be written to there is nowhere else to report it, and the process \
              aborts right after this either way"
)]
fn hook(info: &panic::PanicHookInfo<'_>) {
    PANICKING.store(true, atomic::Ordering::Release);
    // `install` fills `CLOCK` before it installs this hook, so it is always there. A line with
    // time 0 in the impossible case is better than no line.
    let time_us = CLOCK.get().map_or(0, std_clock::StdClock::monotonic_us);
    let thread = thread::current();
    let location = info.location().map(|at| panic_report::Location {
        file: at.file(),
        line: at.line(),
        column: at.column(),
    });
    let line = panic_report::line(
        time_us,
        thread.name(),
        location.as_ref(),
        info.payload_as_str(),
    );
    let _ = io::stderr().write_all(line.as_bytes());
}
