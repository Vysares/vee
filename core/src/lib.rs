//! The core: the services every module uses (events, and later the bus, command router, tables
//! and files), and the one `tick` that runs them. See `docs/design.md`, "Core services".
//!
//! `slot::Slot` is what the executive keeps for each module (the same for all of them).
//!
//! `Core` is scheduled by the executive like a module, but it is not a `Module`: the executive
//! calls `core.tick(now)` every tick, in the order the design gives. It does not know about
//! modules or their rates. The modules are the executive's.
#![forbid(unsafe_code)]
#![no_std]

pub mod event_log;
pub mod events;
mod records;
pub mod reset_log;
pub mod slot;
pub mod suppressed;

use vee_core_api::ev;
use vee_core_api::reset;
use vee_core_types::core_event;
use vee_osal::files;
use vee_osal::platform;
use vee_osal::watchdog;
use vee_osal::watchdog::Watchdog as _;

/// The core's state. So far: the event queue, the event log it is written to, how the previous
/// run ended, and the watchdog.
///
/// `P` is the OSAL's platform (`platform::Platform`): the real services in flight
/// (`Core::<LinuxPlatform>`), and fakes in tests (`Core::<FakePlatform>`). It is one type
/// parameter however many services the core comes to use. Callers name it, because it cannot be
/// worked out from the services handed to `start`.
pub struct Core<P: platform::Platform> {
    /// The event queue. The executive lends it to every `Ctx` it builds (modules and the core's
    /// own reports send into it), and `tick` takes the events out.
    pub ev: ev::Ev,
    files: P::Files,
    /// Fed by `tick`.
    watchdog: P::Watchdog,
    /// Whether the last call on the watchdog failed. It makes a streak of failures one event: only
    /// the call that starts a streak reports.
    watchdog_failing: bool,
    reset_log_path: &'static str,
    event_log_path: &'static str,
    /// How the previous run ended. Set by `start`.
    reset: reset::ResetInfo,
}

impl<P: platform::Platform> Core<P> {
    /// The core with nothing started: an empty queue and no history. `start` is the way to make
    /// one for real.
    fn new(
        files: P::Files,
        watchdog: P::Watchdog,
        reset_log_path: &'static str,
        event_log_path: &'static str,
    ) -> Self {
        Core {
            ev: ev::Ev::new(),
            files,
            watchdog,
            watchdog_failing: false,
            reset_log_path,
            event_log_path,
            reset: reset::ResetInfo::first_boot(),
        }
    }

    /// Runs the core's startup, once, before any module's `init`, and returns the core. `now` is
    /// the time of the startup events.
    ///
    /// 1. The reset log (`reset_log::start`): works out how the last run ended, records this boot,
    ///    and sends the reset report, the first event of every run, and one event for each
    ///    problem found in the old log.
    /// 2. The event log (`event_log::start`): rotates it if it is full or ends in a partial
    ///    record, and sends an event if it was partial. This is done before the first `tick`
    ///    writes anything to it.
    ///
    /// Startup does not fail. If a log cannot be opened, the core carries on without it and sends
    /// an event that says so (`ResetLogUnavailable`, `EventLogUnavailable`). Vee panics only when
    /// there is no alternative, and a broken disk is not a reason to stop a flight computer.
    /// With the reset log down, modules get `ResetInfo::first_boot()`.
    pub fn start(
        files: P::Files,
        watchdog: P::Watchdog,
        reset_log_path: &'static str,
        event_log_path: &'static str,
        now: u64,
    ) -> Self {
        let mut core = Core::new(files, watchdog, reset_log_path, event_log_path);
        core.reset = core.start_reset_log(now);
        core.start_event_log(now);
        core
    }

    /// Runs the core's shutdown, once, after the last tick. `now` is the time of any event it
    /// sends. After it, the core must not be ticked again.
    ///
    /// 1. Records a clean shutdown in the reset log (`reset_log::stop`), so the next boot sees
    ///    that this run ended cleanly, and why. Without it, the next boot sees `Unknown`. If the
    ///    reset log cannot be written, sends `ResetLogUnavailable`, as `start` does.
    /// 2. Writes every queued event to the event log. This is two ticks: the first writes the
    ///    queue and, if events were dropped, queues the report of that, and the second writes
    ///    the report.
    ///
    /// Like `start`, it does not fail.
    pub fn stop(&mut self, now: u64, reason: reset::ShutdownReason) {
        if let Err(err) = reset_log::stop(&self.files, self.reset_log_path, reason) {
            self.send_reset_log_unavailable(now, err);
        }
        self.tick(now);
        self.tick(now);
    }

    /// How the previous run ended. What modules see as `ctx.reset`.
    pub fn reset(&self) -> reset::ResetInfo {
        self.reset
    }

    /// The file system, so a test can read back what the core wrote.
    pub fn files(&self) -> &P::Files {
        &self.files
    }

    /// The watchdog, so a test can read back what the core did with it.
    pub fn watchdog(&self) -> &P::Watchdog {
        &self.watchdog
    }

    /// Step 1 of `start`. Returns the reset info for this run.
    fn start_reset_log(&mut self, now: u64) -> reset::ResetInfo {
        let max_records = reset_log::RESET_LOG_MAX_RECORDS;
        match reset_log::start(&self.files, self.reset_log_path, max_records) {
            Ok(started) => {
                reset_log::report(&started, |code, text| {
                    self.ev.send(core_event::SOURCE, now, code, text);
                });
                started.info
            }
            Err(err) => {
                self.send_reset_log_unavailable(now, err);
                reset::ResetInfo::first_boot()
            }
        }
    }

    /// Sends `ResetLogUnavailable` with the file error. Used by `start` and `stop`.
    fn send_reset_log_unavailable(&mut self, now: u64, err: files::FileError) {
        self.ev.send(
            core_event::SOURCE,
            now,
            core_event::CoreEvent::ResetLogUnavailable,
            format_args!("reset log unavailable: {err}"),
        );
    }

    /// Step 2 of `start`.
    fn start_event_log(&mut self, now: u64) {
        match event_log::start(
            &self.files,
            self.event_log_path,
            event_log::EVENT_LOG_MAX_RECORDS,
        ) {
            Ok(started) => event_log::report(&started, |code, text| {
                self.ev.send(core_event::SOURCE, now, code, text);
            }),
            Err(err) => self.ev.send(
                core_event::SOURCE,
                now,
                core_event::CoreEvent::EventLogUnavailable,
                format_args!("event log unavailable: {err}"),
            ),
        }
    }

    /// The core's step of a tick, at time `now`. The executive calls it after the modules'
    /// suppressed-event reports and before the modules' `step`s, so this tick's reports are
    /// written in the same tick, and the modules' events in the next.
    ///
    /// 1. Events: takes every event out of the queue and writes it to the event log. Then, if
    ///    the queue dropped any events, sends the report (`events::report_dropped`). The report
    ///    comes after the queue has been emptied, so there is room for it.
    /// 2. The watchdog: feeds it. This comes last, so a tick that hangs part way (a blocked
    ///    write, say) is not fed, and the supervisor notices. The call is made every tick, and
    ///    the watchdog sends on each one. If the supervisor cannot be reached, the
    ///    first failure of a streak sends `WatchdogUnavailable`, and the call is tried again at
    ///    the next tick. The event is written by the next tick, as the dropped report is.
    pub fn tick(&mut self, now: u64) {
        self.write_events();
        events::report_dropped(&mut self.ev, now);
        let result = self.watchdog.feed();
        self.note_watchdog(now, result);
    }

    /// Reports the result of a feed. A failure sends `WatchdogUnavailable` only if the feed before
    /// it did not fail, so a streak is one event. A success ends the streak.
    fn note_watchdog(&mut self, now: u64, result: Result<(), watchdog::WatchdogError>) {
        match result {
            Ok(()) => self.watchdog_failing = false,
            Err(err) if !self.watchdog_failing => {
                self.watchdog_failing = true;
                self.ev.send(
                    core_event::SOURCE,
                    now,
                    core_event::CoreEvent::WatchdogUnavailable,
                    format_args!("watchdog: {err}"),
                );
            }
            Err(_) => {}
        }
    }

    /// Takes every event out of the queue and appends it to the event log.
    ///
    /// TEMPORARY. `append` blocks until the record is on the disk, and nothing may block in the
    /// tick loop. It is here so the events have somewhere to go before the file service exists.
    /// The file service will replace the `append` call with a request to its thread, and this
    /// loop stays the same.
    ///
    /// If an `append` fails, the event is lost (there is nothing else to do with it) and the
    /// queue is still emptied: a queue that stopped draining would drop every later event too.
    /// The failures are counted and reported once, to the system log, with the first error.
    fn write_events(&mut self) {
        let mut failed: u32 = 0;
        let mut first_error = None;
        while let Some(event) = self.ev.pop() {
            if let Err(err) = event_log::append(&self.files, self.event_log_path, &event) {
                failed = failed.saturating_add(1);
                first_error.get_or_insert(err);
            }
        }
        if let Some(err) = first_error {
            log::error!("event log: {failed} events not written, the first error was: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super as vee_core;
    use vee_core_api::ev;
    use vee_core_api::event;
    use vee_core_api::reset;
    use vee_core_types::core_event;
    use vee_core_types::event_log as log_types;
    use vee_osal::fake_files;
    use vee_osal::fake_platform;
    use vee_osal::fake_watchdog;
    use vee_osal::files;
    use vee_osal::platform;

    const PATH: &str = "events.log";

    /// The usual test platform: a fake file system and a fake watchdog.
    type Fake = fake_platform::FakePlatform;

    /// A core that has not been started (no startup events), on the file system `files`.
    fn core_on<F: files::Files>(files: F) -> vee_core::Core<fake_platform::FakePlatform<F>> {
        vee_core::Core::new(files, fake_watchdog::FakeWatchdog::new(), RESET_LOG, PATH)
    }

    fn new_core() -> vee_core::Core<Fake> {
        core_on(fake_files::FakeFiles::new())
    }

    /// Sends one event at `time`. Which event does not matter.
    fn send<P: platform::Platform>(core: &mut vee_core::Core<P>, time: u64) {
        core.ev.send(
            1,
            time,
            core_event::CoreEvent::ResetReport,
            format_args!("e"),
        );
    }

    /// How many records are in the event log.
    fn record_count(core: &vee_core::Core<Fake>) -> usize {
        let bytes = core.files().get(PATH).map_or(0, |bytes| bytes.len());
        bytes / log_types::RECORD_LEN
    }

    /// The `index`th record in the event log, decoded.
    fn record(core: &vee_core::Core<Fake>, index: usize) -> event::Event {
        let bytes = core.files().get(PATH).unwrap();
        let record = bytes.chunks(log_types::RECORD_LEN).nth(index).unwrap();
        log_types::decode(record).unwrap()
    }

    /// A file system where every call fails.
    struct FailingFiles;

    impl files::Files for FailingFiles {
        fn len(&self, _: &str) -> Result<Option<u64>, files::FileError> {
            Err(files::FileError::StorageFull)
        }
        fn read_at(&self, _: &str, _: u64, _: &mut [u8]) -> Result<(), files::FileError> {
            Err(files::FileError::StorageFull)
        }
        fn read_file(&self, _: &str, _: &mut [u8]) -> Result<usize, files::FileError> {
            Err(files::FileError::StorageFull)
        }
        fn write_file(&self, _: &str, _: &[u8]) -> Result<(), files::FileError> {
            Err(files::FileError::StorageFull)
        }
        fn append(&self, _: &str, _: &[u8]) -> Result<(), files::FileError> {
            Err(files::FileError::StorageFull)
        }
        fn rotate(&self, _: &str) -> Result<(), files::FileError> {
            Err(files::FileError::StorageFull)
        }
    }

    const RESET_LOG: &str = "reset.log";

    /// The core after `start` on `files`, with the startup events at time 5.
    fn started<F: files::Files>(files: F) -> vee_core::Core<fake_platform::FakePlatform<F>> {
        vee_core::Core::start(
            files,
            fake_watchdog::FakeWatchdog::new(),
            RESET_LOG,
            PATH,
            5,
        )
    }

    /// The next event in the queue, checked to be from the core at time 5.
    fn next_core_event<P: platform::Platform>(core: &mut vee_core::Core<P>) -> event::Event {
        let event = core.ev.pop().unwrap();
        assert_eq!(event.source, core_event::SOURCE);
        assert_eq!(event.monotonic_us, 5);
        event
    }

    /// A fake file system where only the reset log fails, so an event about it can be written to
    /// the event log and read back.
    struct ResetLogDown(fake_files::FakeFiles);

    impl files::Files for ResetLogDown {
        fn len(&self, path: &str) -> Result<Option<u64>, files::FileError> {
            self.0.len(path)
        }
        fn read_at(&self, path: &str, offset: u64, buf: &mut [u8]) -> Result<(), files::FileError> {
            self.0.read_at(path, offset, buf)
        }
        fn read_file(&self, path: &str, buf: &mut [u8]) -> Result<usize, files::FileError> {
            self.0.read_file(path, buf)
        }
        fn write_file(&self, path: &str, bytes: &[u8]) -> Result<(), files::FileError> {
            self.0.write_file(path, bytes)
        }
        fn append(&self, path: &str, bytes: &[u8]) -> Result<(), files::FileError> {
            if path == RESET_LOG {
                return Err(files::FileError::StorageFull);
            }
            self.0.append(path, bytes)
        }
        fn rotate(&self, path: &str) -> Result<(), files::FileError> {
            self.0.rotate(path)
        }
    }

    #[test]
    fn a_clean_stop_is_seen_by_the_next_start_as_a_clean_end_with_its_reason() {
        let mut core = started(fake_files::FakeFiles::new());
        core.stop(9, reset::ShutdownReason::Commanded);

        // The next boot, on a copy of what this run left in the reset log.
        let files = fake_files::FakeFiles::new();
        files.put(RESET_LOG, &core.files().get(RESET_LOG).unwrap());
        let next = started(files);
        assert_eq!(
            next.reset(),
            reset::ResetInfo {
                count: 2,
                previous: reset::PreviousEnd::Clean(reset::ShutdownReason::Commanded),
                consecutive_unclean: 0,
            }
        );
    }

    #[test]
    fn stop_writes_every_queued_event_including_the_report_of_dropped_ones() {
        let mut core = new_core();
        for _ in 0..ev::EV_QUEUE_DEPTH.saturating_add(3) {
            send(&mut core, 0);
        }
        core.stop(77, reset::ShutdownReason::Signal);

        assert!(core.ev.is_empty());
        assert_eq!(record_count(&core), ev::EV_QUEUE_DEPTH.saturating_add(1));
        let report = record(&core, ev::EV_QUEUE_DEPTH);
        assert_eq!(report.code, 7);
        assert_eq!(report.monotonic_us, 77);
    }

    #[test]
    fn when_the_reset_log_cannot_be_written_stop_says_so_and_still_writes_the_events() {
        let mut core = core_on(ResetLogDown(fake_files::FakeFiles::new()));
        send(&mut core, 1);
        core.stop(50, reset::ShutdownReason::Commanded);

        assert!(core.ev.is_empty());
        let bytes = core.files().0.get(PATH).unwrap();
        let mut records = bytes.chunks(log_types::RECORD_LEN);
        assert_eq!(log_types::decode(records.next().unwrap()).unwrap().code, 0); // the event sent before
        let unavailable = log_types::decode(records.next().unwrap()).unwrap();
        assert_eq!(unavailable.code, 8);
        assert_eq!(unavailable.monotonic_us, 50);
        assert_eq!(
            unavailable.text.as_bytes(),
            b"reset log unavailable: no storage space"
        );
        assert!(records.next().is_none());
    }

    #[test]
    fn start_on_a_first_boot_sends_the_reset_report_and_records_the_boot() {
        let mut core = started(fake_files::FakeFiles::new());
        assert_eq!(core.reset(), reset::ResetInfo::first_boot());

        let report = next_core_event(&mut core);
        assert_eq!(report.code, 0);
        assert_eq!(
            report.text.as_bytes(),
            b"reset 1: prev end unknown, 0 unclean in a row"
        );
        assert!(core.ev.is_empty());
        assert!(core.files().get(RESET_LOG).is_some());
    }

    #[test]
    fn start_after_a_run_that_never_shut_down_counts_it_as_unclean() {
        let files = fake_files::FakeFiles::new();
        vee_core::reset_log::start(&files, RESET_LOG, 1000).unwrap(); // an earlier boot, never stopped
        let core = started(files);
        assert_eq!(
            core.reset(),
            reset::ResetInfo {
                count: 2,
                previous: reset::PreviousEnd::Unknown,
                consecutive_unclean: 1,
            }
        );
    }

    #[test]
    fn a_partial_event_log_is_rotated_and_reported_after_the_reset_report() {
        let files = fake_files::FakeFiles::new();
        files.put(PATH, &[0; log_types::RECORD_LEN + 3]);
        let mut core = started(files);

        assert_eq!(next_core_event(&mut core).code, 0); // the reset report is first
        let partial = next_core_event(&mut core);
        assert_eq!(partial.code, 5);
        assert_eq!(
            partial.text.as_bytes(),
            b"event log ends in a partial record"
        );
        assert!(core.ev.is_empty());
        assert!(core.files().get("events.log.1").is_some()); // rotated
    }

    #[test]
    fn when_neither_log_can_be_opened_the_core_starts_with_no_history_and_says_so() {
        let mut core = started(FailingFiles);
        assert_eq!(core.reset(), reset::ResetInfo::first_boot());

        let reset_log = next_core_event(&mut core);
        assert_eq!(reset_log.code, 8);
        assert_eq!(reset_log.severity, event::Severity::Err);
        assert_eq!(
            reset_log.text.as_bytes(),
            b"reset log unavailable: no storage space"
        );
        let event_log = next_core_event(&mut core);
        assert_eq!(event_log.code, 9);
        assert_eq!(
            event_log.text.as_bytes(),
            b"event log unavailable: no storage space"
        );
        assert!(core.ev.is_empty());
    }

    #[test]
    fn the_startup_events_are_written_to_the_event_log_by_the_first_tick() {
        let mut core = started(fake_files::FakeFiles::new());
        core.tick(10);
        assert!(core.ev.is_empty());
        assert_eq!(record_count(&core), 1);
        assert_eq!(record(&core, 0).code, 0);
    }

    #[test]
    fn tick_writes_each_queued_event_to_the_event_log_in_order_and_empties_the_queue() {
        let mut core = new_core();
        for time in [10, 20, 30] {
            send(&mut core, time);
        }
        core.tick(99);
        assert!(core.ev.is_empty());
        assert_eq!(record_count(&core), 3);
        for (index, time) in [10, 20, 30].into_iter().enumerate() {
            assert_eq!(record(&core, index).monotonic_us, time);
        }
    }

    #[test]
    fn tick_with_nothing_queued_writes_nothing() {
        let mut core = new_core();
        core.tick(1);
        assert_eq!(record_count(&core), 0);
        assert!(core.ev.is_empty());
    }

    #[test]
    fn dropped_events_are_reported_after_the_queue_is_emptied() {
        let mut core = new_core();
        for _ in 0..ev::EV_QUEUE_DEPTH.saturating_add(3) {
            send(&mut core, 0);
        }

        // The queue's events are written, and then the report is sent: it is queued, not written.
        core.tick(500);
        assert_eq!(record_count(&core), ev::EV_QUEUE_DEPTH);
        assert_eq!(core.ev.len(), 1);

        // The next tick writes it.
        core.tick(600);
        assert_eq!(record_count(&core), ev::EV_QUEUE_DEPTH.saturating_add(1));
        let report = record(&core, ev::EV_QUEUE_DEPTH);
        assert_eq!(report.source, core_event::SOURCE);
        assert_eq!(report.code, 7);
        assert_eq!(report.monotonic_us, 500);
        assert_eq!(
            report.text.as_bytes(),
            b"3 events dropped: the queue was full"
        );
    }

    #[test]
    fn when_the_event_log_cannot_be_written_the_queue_is_still_emptied() {
        let mut core = core_on(FailingFiles);
        for time in [1, 2, 3] {
            send(&mut core, time);
        }
        core.tick(99);
        assert!(core.ev.is_empty());
    }

    #[test]
    fn every_tick_feeds_the_watchdog_once() {
        let mut core = new_core();
        for time in [1, 2, 3] {
            core.tick(time);
        }
        assert_eq!(core.watchdog().feed_calls(), 3);
        assert!(core.ev.is_empty()); // a feed that works sends no event
    }

    #[test]
    fn a_failing_watchdog_is_reported_once_per_streak_and_still_fed_every_tick() {
        let mut core = new_core();
        core.watchdog().set_failing(true);
        for time in [1, 2, 3] {
            core.tick(time);
        }
        assert_eq!(core.watchdog().feed_calls(), 3); // tried again each tick
        assert_eq!(record_count(&core), 1); // sent by tick 1, written by tick 2
        let event = record(&core, 0);
        assert_eq!(event.source, core_event::SOURCE);
        assert_eq!(event.code, 10);
        assert_eq!(event.severity, event::Severity::Err);
        assert_eq!(event.monotonic_us, 1);
        assert_eq!(event.text.as_bytes(), b"watchdog: other error");

        // A feed that works ends the streak, so the next failure is reported again.
        core.watchdog().set_failing(false);
        core.tick(4);
        core.watchdog().set_failing(true);
        core.tick(5);
        core.tick(6);
        assert_eq!(record_count(&core), 2);
        assert_eq!(record(&core, 1).monotonic_us, 5);
    }
}
