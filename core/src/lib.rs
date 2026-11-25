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

pub mod commands;
pub mod event_log;
pub mod events;
pub mod file_device;
pub mod file_service;
pub mod log_writer;
mod records;
pub mod reset_log;
pub mod slot;
pub mod suppressed;

use vee_core_api::cmd;
use vee_core_api::cmd::CommandTarget as _;
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
    /// own reports send into it), and the log writer takes the events out.
    pub ev: ev::Ev,
    files: P::Files,
    /// Fed by `tick`.
    watchdog: P::Watchdog,
    /// Whether the last call on the watchdog failed. It makes a streak of failures one event: only
    /// the call that starts a streak reports.
    watchdog_failing: bool,
    reset_log_path: &'static str,
    event_log_path: &'static str,
    /// Appends the events to the event log through its own file service. `None` if it could not
    /// start (then the events wait in the queue, and are dropped and counted when it is full).
    log_writer: Option<log_writer::LogWriter>,
    /// How the previous run ended. Set by `start`.
    reset: reset::ResetInfo,
    /// The core's own commands: its queue, drained in `tick`.
    commands: commands::CoreCommands,
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
            log_writer: None,
            reset: reset::ResetInfo::first_boot(),
            commands: commands::CoreCommands::new(),
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
    /// 3. The event log's writer, with its own file service and worker thread (it allocates, so
    ///    startup is where it is made).
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
        core.start_log_writer(now);
        core
    }

    /// Runs the core's shutdown, once, after the last tick. `now` is the time of any event it
    /// sends. After it, the core must not be ticked again.
    ///
    /// 1. Records a clean shutdown in the reset log (`reset_log::stop`), so the next boot sees
    ///    that this run ended cleanly, and why. Without it, the next boot sees `Unknown`. If the
    ///    reset log cannot be written, sends `ResetLogUnavailable`, as `start` does.
    /// 2. Writes every queued event to the event log, waiting for the disk (`flush_event_log`).
    ///    Then, if the queue refused events, sends the report of that, which the first write has
    ///    made room for, and writes it too. This is the one place the core blocks, and it gives up
    ///    on a write that takes longer than `log_writer::EVENT_LOG_TIMEOUT_US`.
    ///
    /// Like `start`, it does not fail.
    pub fn stop(&mut self, now: u64, reason: reset::ShutdownReason) {
        if let Err(err) = reset_log::stop(&self.files, self.reset_log_path, reason) {
            self.send_reset_log_unavailable(now, err);
        }
        self.flush_event_log(now);
        events::report_dropped(&mut self.ev, now);
        self.flush_event_log(now);
    }

    /// Writes every queued event to the event log and waits until it is on the disk. It blocks
    /// (up to `log_writer::EVENT_LOG_TIMEOUT_US` for a write), so it is for the shutdown, and for a
    /// test that needs the log to be complete.
    pub fn flush_event_log(&mut self, now: u64) {
        if let Some(writer) = self.log_writer.as_mut() {
            writer.flush(&mut self.ev, now);
        }
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

    /// Step 3 of `start`: the event log's writer.
    fn start_log_writer(&mut self, now: u64) {
        match log_writer::LogWriter::start(self.files.clone(), self.event_log_path) {
            Ok(writer) => self.log_writer = Some(writer),
            Err(err) => self.ev.send(
                core_event::SOURCE,
                now,
                core_event::CoreEvent::EventLogUnavailable,
                format_args!("event log unavailable: {err}"),
            ),
        }
    }

    /// The router: moves every command waiting in the sinks to its target's queue, and sends the
    /// received ack, or the rejected nak with the reason, for each. `now` is the time of those
    /// events. A command for the core (`cmd::CORE_TARGET`) goes to the core's own queue; any
    /// other goes to the entry in `targets` with that id (the modules', which the executive
    /// owns and lends), or is rejected as an unknown target. Nothing is run here: a target
    /// handles its commands in its own step.
    ///
    /// The executive calls it once per tick, before `tick` (so the core's own commands are handled
    /// in the same tick) and before the modules' `step`s.
    pub fn route_commands(
        &mut self,
        sinks: &mut [cmd::Sink],
        targets: &mut [(u8, &mut dyn cmd::CommandTarget)],
        now: u64,
    ) {
        for sink in sinks {
            while let Some(raw) = sink.pop() {
                let result = if raw.target == cmd::CORE_TARGET {
                    self.commands.accept(&raw)
                } else {
                    cmd::route(targets, raw)
                };
                cmd::ack(&mut self.ev, &raw, &result, now);
            }
        }
    }

    /// The core's step of a tick, at time `now`. The executive calls it after the modules'
    /// suppressed-event reports and before the modules' `step`s.
    ///
    /// 1. Commands: handles the core's own queued commands (`cmd::drain`, like a module in its own
    ///    step), which sends each one's outcome event. Done first, so those events are written
    ///    in this tick's batch.
    /// 2. Events: the log writer collects what happened to the write in flight, and, if the file
    ///    service is free, sends the next batch of queued events (one write for as many as fit).
    ///    It never waits. Then, if the queue refused any events, sends the report
    ///    (`events::report_dropped`).
    /// 3. The watchdog: feeds it. This comes last, so a tick that hangs part way (a blocked
    ///    write, say) is not fed, and the supervisor notices. The call is made every tick, and
    ///    the watchdog sends on each one. If the supervisor cannot be reached, the
    ///    first failure of a streak sends `WatchdogUnavailable`, and the call is tried again at
    ///    the next tick. The event is written by the next tick, as the dropped report is.
    pub fn tick(&mut self, now: u64) {
        cmd::drain(
            &mut self.commands.queue,
            &mut self.ev,
            now,
            commands::handle,
        );
        if let Some(writer) = self.log_writer.as_mut() {
            writer.step(&mut self.ev, now);
        }
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
}

#[cfg(test)]
mod tests {
    use super as vee_core;
    use super::commands;
    use vee_core_api::cmd;
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
    /// It has its event log writer, which `start` makes: without it nothing would write the events.
    fn core_on<F: files::Files + Clone + Send + 'static>(
        files: F,
    ) -> vee_core::Core<fake_platform::FakePlatform<F>> {
        let mut core =
            vee_core::Core::new(files, fake_watchdog::FakeWatchdog::new(), RESET_LOG, PATH);
        core.start_log_writer(0);
        core
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
    #[derive(Clone)]
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
    fn started<F: files::Files + Clone + Send + 'static>(
        files: F,
    ) -> vee_core::Core<fake_platform::FakePlatform<F>> {
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
        let event = core.ev.log.pop().unwrap();
        assert_eq!(event.source, core_event::SOURCE);
        assert_eq!(event.monotonic_us, 5);
        event
    }

    /// A fake file system where only the reset log fails, so an event about it can be written to
    /// the event log and read back.
    #[derive(Clone)]
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
        for _ in 0..ev::EV_LOG_QUEUE_DEPTH.saturating_add(3) {
            send(&mut core, 0);
        }
        core.stop(77, reset::ShutdownReason::Signal);

        assert!(core.ev.log.is_empty());
        assert_eq!(
            record_count(&core),
            ev::EV_LOG_QUEUE_DEPTH.saturating_add(1)
        );
        let report = record(&core, ev::EV_LOG_QUEUE_DEPTH);
        assert_eq!(report.code, 7);
        assert_eq!(report.monotonic_us, 77);
    }

    #[test]
    fn when_the_reset_log_cannot_be_written_stop_says_so_and_still_writes_the_events() {
        let mut core = core_on(ResetLogDown(fake_files::FakeFiles::new()));
        send(&mut core, 1);
        core.stop(50, reset::ShutdownReason::Commanded);

        assert!(core.ev.log.is_empty());
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
        assert!(core.ev.log.is_empty());
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
        assert!(core.ev.log.is_empty());
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
        assert!(core.ev.log.is_empty());
    }

    #[test]
    fn the_startup_events_are_written_to_the_event_log_by_the_first_tick() {
        let mut core = started(fake_files::FakeFiles::new());
        core.tick(10);
        core.flush_event_log(10); // the write is on the worker's thread: wait for it
        assert!(core.ev.log.is_empty());
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
        core.flush_event_log(99);
        assert!(core.ev.log.is_empty());
        assert_eq!(record_count(&core), 3);
        for (index, time) in [10, 20, 30].into_iter().enumerate() {
            assert_eq!(record(&core, index).monotonic_us, time);
        }
    }

    #[test]
    fn tick_with_nothing_queued_writes_nothing() {
        let mut core = new_core();
        core.tick(1);
        core.flush_event_log(1);
        assert_eq!(record_count(&core), 0);
        assert!(core.ev.log.is_empty());
    }

    #[test]
    fn dropped_events_are_reported_once_the_writer_has_made_room_and_the_report_is_written() {
        let mut core = new_core();
        for _ in 0..ev::EV_LOG_QUEUE_DEPTH.saturating_add(3) {
            send(&mut core, 0);
        }

        // The first tick takes a batch out of the full queue, which makes room for the report.
        core.tick(500);
        core.flush_event_log(500);
        assert!(core.ev.log.is_empty());
        assert_eq!(
            record_count(&core),
            ev::EV_LOG_QUEUE_DEPTH.saturating_add(1)
        );
        let report = record(&core, ev::EV_LOG_QUEUE_DEPTH);
        assert_eq!(report.source, core_event::SOURCE);
        assert_eq!(report.code, 7);
        assert_eq!(report.monotonic_us, 500);
        assert_eq!(
            report.text.as_bytes(),
            b"3 log events dropped: the queue was full"
        );
    }

    #[test]
    fn a_report_that_does_not_fit_in_a_full_queue_is_counted_and_made_the_next_time() {
        // No writer (it did not start), so nothing makes room: the queue stays full.
        let mut core = vee_core::Core::<Fake>::new(
            fake_files::FakeFiles::new(),
            fake_watchdog::FakeWatchdog::new(),
            RESET_LOG,
            PATH,
        );
        for _ in 0..ev::EV_LOG_QUEUE_DEPTH.saturating_add(3) {
            send(&mut core, 0);
        }
        core.tick(1); // reports 3, which the full queue refuses: one more is counted
        assert_eq!(core.ev.log.len(), ev::EV_LOG_QUEUE_DEPTH);
        assert_eq!(core.ev.log.take_refused(), 1);
    }

    #[test]
    fn when_the_event_log_cannot_be_written_the_queue_is_still_emptied() {
        let mut core = core_on(FailingFiles);
        for time in [1, 2, 3] {
            send(&mut core, time);
        }
        core.tick(99);
        core.flush_event_log(99);
        assert!(core.ev.log.is_empty());
    }

    #[test]
    fn every_tick_feeds_the_watchdog_once() {
        let mut core = new_core();
        for time in [1, 2, 3] {
            core.tick(time);
        }
        assert_eq!(core.watchdog().feed_calls(), 3);
        assert!(core.ev.log.is_empty()); // a feed that works sends no event
    }

    #[test]
    fn a_failing_watchdog_is_reported_once_per_streak_and_still_fed_every_tick() {
        let mut core = new_core();
        core.watchdog().set_failing(true);
        for time in [1, 2, 3] {
            core.tick(time);
        }
        assert_eq!(core.watchdog().feed_calls(), 3); // tried again each tick
        core.flush_event_log(3);
        assert_eq!(record_count(&core), 1); // sent by tick 1, written after it
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
        core.flush_event_log(6);
        assert_eq!(record_count(&core), 2);
        assert_eq!(record(&core, 1).monotonic_us, 5);
    }

    /// A core with no event log writer, so a test can read the events from `core.ev.log`.
    fn bare_core() -> vee_core::Core<Fake> {
        vee_core::Core::new(
            fake_files::FakeFiles::new(),
            fake_watchdog::FakeWatchdog::new(),
            RESET_LOG,
            PATH,
        )
    }

    /// A command target that keeps what it is handed.
    struct Keeper(Option<cmd::RawCmd>);

    impl cmd::CommandTarget for Keeper {
        fn accept(&mut self, raw: &cmd::RawCmd) -> Result<(), cmd::CmdErr> {
            self.0 = Some(*raw);
            Ok(())
        }
    }

    /// The text of the next event in the queue.
    fn next_text(core: &mut vee_core::Core<Fake>) -> event::Text {
        core.ev.log.pop().unwrap().text
    }

    #[test]
    fn a_command_for_a_module_is_routed_to_it_and_acked() {
        let mut core = bare_core();
        let mut sinks = [cmd::Sink::new(1)];
        sinks[0].submit(5, 3, 0x20, cmd::Args::EMPTY).unwrap();
        let mut module = Keeper(None);
        core.route_commands(&mut sinks, &mut [(3, &mut module)], 10);

        let raw = module.0.unwrap();
        assert_eq!((raw.sink, raw.seq, raw.opcode), (1, 5, 0x20));
        assert_eq!(
            next_text(&mut core).as_str(),
            "cmd 1/5 t3 op0x0020: accepted"
        );
        assert!(sinks[0].pop().is_none());
    }

    #[test]
    fn a_command_for_no_one_is_rejected() {
        let mut core = bare_core();
        let mut sinks = [cmd::Sink::new(1)];
        sinks[0].submit(5, 9, 0x20, cmd::Args::EMPTY).unwrap();
        core.route_commands(&mut sinks, &mut [], 10);
        assert_eq!(
            next_text(&mut core).as_str(),
            "cmd 1/5 t9 op0x0020: rejected: unknown command target"
        );
    }

    #[test]
    fn a_core_command_is_acked_when_routed_and_run_by_the_next_tick() {
        let mut core = bare_core();
        let mut sinks = [cmd::Sink::new(2)];
        // SetEventFilter (opcode 1), floor 1.
        sinks[0]
            .submit(8, cmd::CORE_TARGET, 1, cmd::Args::new(&[1]).unwrap())
            .unwrap();
        core.route_commands(&mut sinks, &mut [], 10);
        assert_eq!(
            next_text(&mut core).as_str(),
            "cmd 2/8 t0 op0x0001: accepted"
        );
        assert!(core.ev.log.is_empty());

        core.tick(20);
        assert_eq!(
            next_text(&mut core).as_str(),
            "cmd 2/8 t0 op0x0001: succeeded"
        );
    }

    #[test]
    fn every_sink_is_drained_in_order() {
        let mut core = bare_core();
        let mut sinks = [cmd::Sink::new(1), cmd::Sink::new(2)];
        sinks[0].submit(1, 3, 1, cmd::Args::EMPTY).unwrap();
        sinks[1].submit(2, 3, 1, cmd::Args::EMPTY).unwrap();
        let mut module = Keeper(None);
        core.route_commands(&mut sinks, &mut [(3, &mut module)], 10);
        assert_eq!(
            next_text(&mut core).as_str(),
            "cmd 1/1 t3 op0x0001: accepted"
        );
        assert_eq!(
            next_text(&mut core).as_str(),
            "cmd 2/2 t3 op0x0001: accepted"
        );
    }

    #[test]
    fn the_core_refuses_a_command_when_its_own_queue_is_full() {
        let mut core = bare_core();
        let mut sink = [cmd::Sink::new(1)];
        // Route one at a time, with no `tick` between: nothing empties the core's queue.
        for seq in 0..=u16::try_from(commands::CORE_CMD_QUEUE_DEPTH).unwrap() {
            sink[0]
                .submit(seq, cmd::CORE_TARGET, 1, cmd::Args::new(&[1]).unwrap())
                .unwrap();
            core.route_commands(&mut sink, &mut [], 10);
        }
        for _ in 0..commands::CORE_CMD_QUEUE_DEPTH {
            assert!(next_text(&mut core).as_str().ends_with("accepted"));
        }
        assert!(
            next_text(&mut core)
                .as_str()
                .ends_with("rejected: command queue is full")
        );
        assert!(core.ev.log.is_empty());
    }

    #[test]
    fn a_tick_empties_the_cores_queue_so_it_takes_commands_again() {
        let mut core = bare_core();
        let mut sink = [cmd::Sink::new(1)];
        for round in 0..2 {
            for seq in 0..commands::CORE_CMD_QUEUE_DEPTH {
                let seq = u16::try_from(seq).unwrap();
                sink[0]
                    .submit(seq, cmd::CORE_TARGET, 1, cmd::Args::new(&[1]).unwrap())
                    .unwrap();
                core.route_commands(&mut sink, &mut [], round);
            }
            core.tick(round);
        }
        let mut rejected = 0;
        let mut succeeded = 0;
        while let Some(event) = core.ev.log.pop() {
            rejected += usize::from(event.text.as_str().contains("rejected"));
            succeeded += usize::from(event.text.as_str().ends_with("succeeded"));
        }
        assert_eq!(rejected, 0);
        assert_eq!(succeeded, 2 * commands::CORE_CMD_QUEUE_DEPTH);
    }

    #[test]
    fn commands_for_the_core_and_a_module_in_one_sink_are_acked_in_order() {
        let mut core = bare_core();
        let mut sinks = [cmd::Sink::new(1)];
        sinks[0]
            .submit(1, cmd::CORE_TARGET, 1, cmd::Args::new(&[1]).unwrap())
            .unwrap();
        sinks[0].submit(2, 3, 0x20, cmd::Args::EMPTY).unwrap();
        sinks[0]
            .submit(3, cmd::CORE_TARGET, 1, cmd::Args::new(&[9]).unwrap())
            .unwrap();
        let mut module = Keeper(None);
        core.route_commands(&mut sinks, &mut [(3, &mut module)], 10);
        assert_eq!(
            next_text(&mut core).as_str(),
            "cmd 1/1 t0 op0x0001: accepted"
        );
        assert_eq!(
            next_text(&mut core).as_str(),
            "cmd 1/2 t3 op0x0020: accepted"
        );
        assert_eq!(
            next_text(&mut core).as_str(),
            "cmd 1/3 t0 op0x0001: rejected: invalid: floor must be 0..=3"
        );
    }

    #[test]
    fn routing_with_nothing_in_the_sinks_sends_nothing() {
        let mut core = bare_core();
        let mut sinks = [cmd::Sink::new(1), cmd::Sink::new(2)];
        core.route_commands(&mut sinks, &mut [], 10);
        assert!(core.ev.log.is_empty());
    }

    #[test]
    fn a_command_is_routed_once() {
        let mut core = bare_core();
        let mut sinks = [cmd::Sink::new(1)];
        sinks[0].submit(1, 3, 1, cmd::Args::EMPTY).unwrap();
        let mut module = Keeper(None);
        core.route_commands(&mut sinks, &mut [(3, &mut module)], 10);
        core.route_commands(&mut sinks, &mut [(3, &mut module)], 20);
        assert!(core.ev.log.pop().is_some());
        assert!(core.ev.log.is_empty());
    }

    #[test]
    fn a_received_ack_is_info_and_a_rejection_is_an_error_with_their_own_codes() {
        use event::Code as _;
        let mut core = bare_core();
        let mut sinks = [cmd::Sink::new(1)];
        sinks[0].submit(1, 3, 1, cmd::Args::EMPTY).unwrap();
        sinks[0].submit(2, 9, 1, cmd::Args::EMPTY).unwrap();
        let mut module = Keeper(None);
        core.route_commands(&mut sinks, &mut [(3, &mut module)], 10);

        let accepted = core.ev.log.pop().unwrap();
        assert_eq!(accepted.severity, event::Severity::Info);
        assert_eq!(accepted.code, cmd::CmdEvent::Accepted.code());
        let rejected = core.ev.log.pop().unwrap();
        assert_eq!(rejected.severity, event::Severity::Err);
        assert_eq!(rejected.code, cmd::CmdEvent::Rejected.code());
    }

    #[test]
    fn the_cores_ack_is_sent_from_the_core_at_the_given_time_even_when_it_is_a_nak() {
        let mut core = bare_core();
        let mut sinks = [cmd::Sink::new(1)];
        sinks[0].submit(1, 9, 1, cmd::Args::EMPTY).unwrap();
        core.route_commands(&mut sinks, &mut [], 1234);
        let event = core.ev.log.pop().unwrap();
        assert_eq!(event.source, core_event::SOURCE);
        assert_eq!(event.monotonic_us, 1234);
        assert_eq!(event.severity, event::Severity::Err);
    }
}
