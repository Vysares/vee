//! The sample deployment: a Vee executable, written by hand. It is what `exec!` will one day
//! generate, so it is kept plain and complete, and it grows with Vee.
//!
//! Run it from this directory, so that `.cargo/config.toml` (the build-time settings) applies:
//! `cd sample-deployment && cargo run`. It writes its logs to the current directory.
//!
//! It starts Vee (see `start`) and its one module (`vee-example`), and then runs the loop (see
//! `Exec::run`) at 100 Hz until it is killed. Set `VEE_SAMPLE_TICKS=250` (any number) to run that
//! many ticks and then stop cleanly, which is how to see a clean shutdown (and the next boot
//! report it).
#![forbid(unsafe_code)]

use vee_core::Core;
use vee_core::slot;
use vee_core_api::reset;
use vee_core_types::core_event;
use vee_example as example;
use vee_osal::clock::Clock as _;
use vee_osal::linux;
use vee_osal::linux::std_alloc_guard;
use vee_osal::linux::std_clock;
use vee_osal::linux::std_files;
use vee_osal::linux::std_panic;
use vee_osal::linux::std_syslog;
use vee_osal::linux::std_watchdog;

/// The allocation guard, in builds that ask for it (`--features alloc-guard`): every allocation is
/// counted, and once `start` has called `std_alloc_guard::arm()`, any allocation aborts. A program has
/// one global allocator, so this is the one place it is chosen.
#[cfg(feature = "alloc-guard")]
#[global_allocator]
static GUARD: std_alloc_guard::GuardAlloc = std_alloc_guard::GuardAlloc;

/// Where the reset log and the event log are: the current directory, for a sample.
const RESET_LOG: &str = "reset.log";
const EVENT_LOG: &str = "events.log";

/// The modules' ids, which go into every event they send. 0 is the core's.
const EXAMPLE_SOURCE: u8 = 1;

/// The time between the start of one tick and the start of the next: a tick rate of 100 Hz. Ticks
/// start at multiples of this on the clock, so a tick has a number: its start time divided by this.
const TICK_US: u64 = 10_000;

/// The example module steps on every this many ticks. With a tick rate of 100 Hz that is once a
/// second.
const EXAMPLE_PERIOD_TICKS: u64 = 100;

/// The system log keeps messages at this level and more severe.
const LOG_LEVEL: log::LevelFilter = log::LevelFilter::Info;

/// Why Vee could not start. Only for failures that are bugs in this start function, because
/// startup does not fail for anything else: the core and `start` carry on and report. `main`
/// returns it, and Rust then prints it to stderr and exits with an error.
#[derive(Debug)]
enum StartError {
    /// A system log is installed already, or the system log was not installed.
    SystemLog(log::SetLoggerError),
    /// A panic hook is installed already.
    PanicHook(std_panic::InstallError),
    /// `VEE_SAMPLE_TICKS` is set and is not a whole number.
    InvalidTickLimit,
}

impl From<log::SetLoggerError> for StartError {
    fn from(error: log::SetLoggerError) -> Self {
        StartError::SystemLog(error)
    }
}

impl From<std_panic::InstallError> for StartError {
    fn from(error: std_panic::InstallError) -> Self {
        StartError::PanicHook(error)
    }
}

/// The start of the first tick after `time_us`: the next multiple of `TICK_US` that is later.
/// (If the sum could overflow, which a clock in microseconds will not do in 584,000 years, it is
/// the largest time.)
fn next_boundary_after(time_us: u64) -> u64 {
    time_us
        .checked_div(TICK_US)
        .and_then(|ticks| ticks.checked_add(1))
        .and_then(|ticks| ticks.checked_mul(TICK_US))
        .unwrap_or(u64::MAX)
}

/// How many tick starts were missed: a tick that began at `start_us` should be followed by one at
/// `start_us + TICK_US`, and the next one began at `next_start_us`.
fn ticks_skipped(start_us: u64, next_start_us: u64) -> u64 {
    next_start_us
        .saturating_sub(start_us.saturating_add(TICK_US))
        .checked_div(TICK_US)
        .unwrap_or(0)
}

/// The executive: the core, the clock and one `Slot` for each module. This is the part `exec!`
/// will generate (a field for each module and the calls in order). `Slot` does the work that is
/// the same for every module (see `vee_core::slot`).
struct Exec {
    clock: std_clock::StdClock,
    core: Core<linux::LinuxPlatform>,
    example: slot::Slot<example::Example>,
}

impl Exec {
    /// Tick number `tick`, in the order the design gives (see "Executive Requirements"):
    /// 1. Each module's suppressed events from the tick before, and a new count for each.
    /// 2. The core's tick: the event queue is written to the event log, and the watchdog is fed.
    /// 3. Each module that is due steps, in order.
    fn tick(&mut self, tick: u64) {
        let now = self.clock.monotonic_us();
        let reset = self.core.reset();

        self.example.report_suppressed(&mut self.core.ev, now);
        self.core.tick(now);
        self.example
            .step(tick, &mut self.core.ev, &self.clock, reset);
    }

    /// The loop: runs a tick at the start of each period, for `tick_limit` ticks, or for ever if
    /// there is no limit.
    ///
    /// A tick starts at the first boundary after the clock was read, and the next one at the
    /// first boundary after this one is done. A tick that takes longer than its period (an
    /// overrun) is not made up for: the boundaries it missed are skipped, nothing runs twice to
    /// catch up, and a module that was due on a skipped tick does not step. The skipped ticks are
    /// logged. (They will become an event when the executive's overrun events are designed.)
    fn run(&mut self, tick_limit: Option<u64>) {
        let mut start_us = next_boundary_after(self.clock.monotonic_us());
        let mut ticks_run: u64 = 0;
        loop {
            self.clock.sleep_until_us(start_us);
            self.tick(start_us.checked_div(TICK_US).unwrap_or(0));
            ticks_run = ticks_run.saturating_add(1);
            if tick_limit.is_some_and(|limit| ticks_run >= limit) {
                return;
            }

            let next_start_us = next_boundary_after(self.clock.monotonic_us());
            let skipped = ticks_skipped(start_us, next_start_us);
            if skipped > 0 {
                log::warn!("overrun: the tick at {start_us} us took too long, skipped {skipped}");
            }
            start_us = next_start_us;
        }
    }

    /// Shutdown, after the last tick.
    fn stop(&mut self) {
        let now = self.clock.monotonic_us();
        self.core.stop(now, reset::ShutdownReason::Commanded);
    }
}

/// Vee's startup, in the order the design gives (see "Executive Requirements"). `clock` times
/// everything, so every time in the logs and the events counts from the same start.
///
/// 1. The system log, first, so that everything after it can be seen.
/// 2. The panic hook, in builds that abort on a panic (the release build).
/// 3. The watchdog, from the environment. If that fails, a watchdog that does nothing takes its
///    place and the error is reported once the core exists, because startup does not fail.
/// 4. The core (`Core::start`): the reset log and the event log.
/// 5. Each module's `init`, in order.
/// 6. The system log's queue, and then the allocation guard (`arm`). The queue's thread
///    allocates, so it comes before `arm`.
fn start(clock: std_clock::StdClock) -> Result<Exec, StartError> {
    let began_us = clock.monotonic_us();

    std_syslog::init(clock, LOG_LEVEL)?;
    if cfg!(panic = "abort") {
        std_panic::install(clock)?;
    }

    let (watchdog, watchdog_error) = match std_watchdog::StdWatchdog::from_env() {
        Ok(watchdog) => (watchdog, None),
        Err(error) => (std_watchdog::StdWatchdog::disabled(), Some(error)),
    };

    let mut core = Core::<linux::LinuxPlatform>::start(
        std_files::StdFiles,
        watchdog,
        RESET_LOG,
        EVENT_LOG,
        clock.monotonic_us(),
    );
    if let Some(error) = watchdog_error {
        log::error!("the watchdog is off: {error}");
        core.ev.send(
            core_event::SOURCE,
            clock.monotonic_us(),
            core_event::CoreEvent::WatchdogUnavailable,
            format_args!("watchdog: {error}"),
        );
    }

    let mut example = slot::Slot::new(
        example::Example::new(),
        EXAMPLE_SOURCE,
        EXAMPLE_PERIOD_TICKS,
    );
    let reset = core.reset();
    example.init(&mut core.ev, &clock, reset);

    if let Err(error) = std_syslog::start_queue() {
        log::error!("the system log stays in direct mode: {error}");
        core.ev.send(
            core_event::SOURCE,
            clock.monotonic_us(),
            core_event::CoreEvent::SystemLogQueueUnavailable,
            format_args!("system log queue: {error}"),
        );
    }
    std_alloc_guard::arm();

    let startup_us = clock.monotonic_us().saturating_sub(began_us);
    log::info!("started in {startup_us} us");
    Ok(Exec {
        clock,
        core,
        example,
    })
}

/// How many ticks to run, from `VEE_SAMPLE_TICKS`: `None` (run until killed) if it is not set.
fn tick_limit() -> Result<Option<u64>, StartError> {
    match std::env::var("VEE_SAMPLE_TICKS") {
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(StartError::InvalidTickLimit),
        Ok(text) => text
            .parse()
            .map(Some)
            .map_err(|_| StartError::InvalidTickLimit),
    }
}

fn main() -> Result<(), StartError> {
    let tick_limit = tick_limit()?;
    let clock = std_clock::StdClock::new();
    let mut exec = start(clock)?;
    exec.run(tick_limit);
    exec.stop();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super as sample;

    #[test]
    fn the_next_boundary_is_the_next_multiple_of_the_tick_after_the_time() {
        assert_eq!(sample::next_boundary_after(0), sample::TICK_US);
        assert_eq!(sample::next_boundary_after(1), sample::TICK_US);
        assert_eq!(
            sample::next_boundary_after(sample::TICK_US - 1),
            sample::TICK_US
        );
        // Exactly on a boundary is not "after" it: the next tick is the one after.
        assert_eq!(
            sample::next_boundary_after(sample::TICK_US),
            2 * sample::TICK_US
        );
    }

    #[test]
    fn a_tick_that_finishes_in_time_skips_nothing() {
        let start = 3 * sample::TICK_US;
        let done = start + sample::TICK_US - 1;
        let next = sample::next_boundary_after(done);
        assert_eq!(next, 4 * sample::TICK_US);
        assert_eq!(sample::ticks_skipped(start, next), 0);
    }

    #[test]
    fn a_tick_that_overruns_skips_the_boundaries_it_missed() {
        let start = 3 * sample::TICK_US;
        // Done a little after the boundary at 4 and at 5: the tick after it starts at 6, so 4 and
        // 5 are skipped.
        let done = 5 * sample::TICK_US + 1;
        let next = sample::next_boundary_after(done);
        assert_eq!(next, 6 * sample::TICK_US);
        assert_eq!(sample::ticks_skipped(start, next), 2);
    }

    #[test]
    fn a_clock_at_the_end_of_time_does_not_overflow() {
        assert_eq!(sample::next_boundary_after(u64::MAX), u64::MAX);
        assert_eq!(sample::ticks_skipped(u64::MAX, u64::MAX), 0);
    }
}
