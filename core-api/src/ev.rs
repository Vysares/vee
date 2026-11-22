//! The event service: builds events and holds them until the event log and the downlink take
//! them.
//!
//! `Ev` is plain data with no I/O and no clock, so it can be tested on its own. There is one of
//! them in the whole system, owned by the core. It is in this crate, not in the core, so that
//! `Ctx` (which modules see) can hold a reference to it. A module never gets that reference: it
//! gets `ctx.ev`, a small handle that fills in the module's `source` and the time and then calls
//! `Ev::send`, so a module cannot claim another module's id. The core's own code (such as
//! `reset_log::report`) calls `send` with `core_event::SOURCE`.
//!
//! Who calls what: a sender calls `send`, which puts the event in the log queue. The event log's
//! writer takes the events out of it (looking at the next one with `peek` and taking it with
//! `pop` once it has room for it), and the core sends what the queue refused (`take_refused`) on
//! as events. Other consumers of events (the downlink, FDIR) are subscribers on the bus, which the
//! executive feeds from the events a module sent, right after that module's step.

use crate::config;
use crate::event;
use crate::queue;
use core::fmt;
use core::fmt::Write as _;

config::setting! {
    /// How many events the LOG queue holds. Every event goes into it, and the event log's writer
    /// takes them out when its file service is free, so it is where a burst waits, and where a
    /// stalled disk is ridden out until it is full. Each slot is 80 bytes with the default
    /// `EV_TEXT_MAX_BYTES` of 64 (the 12 byte envelope and the text, rounded up to a multiple of
    /// 8), so the default of 1024 is 80 KiB. There is no per source cap, so the worst case in one
    /// tick is every source sending every one of its events `EV_MAX_PER_TICK` times.
    pub EV_LOG_QUEUE_DEPTH: usize = 1024, 1..=65535;
}

/// The event service: the log queue. Nothing is allocated.
///
/// This is the core's, not a module's. It is `pub` and lives in this crate only because `Ctx`
/// (which modules see) holds the handle `ctx.ev`, and that handle holds a `&mut Ev`. A module
/// reaches it only through `ctx.ev.send`.
///
/// The queue is `pub` so that its consumer (the log writer) uses it directly: `ev.log.peek()`,
/// `ev.log.pop()`, `ev.log.take_refused()`. A full queue refuses the new event (the oldest are
/// kept, because the first events of a problem are the most useful), and counts it.
pub struct Ev {
    /// Every event: the log is the on board record.
    pub log: queue::Queue<event::Event, EV_LOG_QUEUE_DEPTH>,
}

impl Ev {
    pub const fn new() -> Self {
        Ev {
            log: queue::Queue::new(),
        }
    }

    /// Builds an event and queues it. The event's code and severity come from `code`, and its
    /// text is `text` written into the event's fixed size text.
    ///
    /// It also writes the event to the system log (`log::`, target `event`), whether or not the
    /// queue has room. That is the one thing here besides the queue: no I/O of its own, and no
    /// clock.
    ///
    /// It never fails, because the sender can do nothing about a failure:
    /// - Text that is too long is cut off and ends in `~`. The event is still sent.
    /// - If a value in the text fails to print, the text up to that point is kept. The event is
    ///   still sent.
    /// - If the queue is full, the new event is not put in it, and the queue counts it
    ///   (`take_refused`).
    pub fn send<C: event::Code>(
        &mut self,
        source: u8,
        monotonic_us: u64,
        code: C,
        text: fmt::Arguments<'_>,
    ) {
        let mut written = event::Text::EMPTY;
        // `FixedStr` never fails to take text. Only a value's own `Display` can, and then the
        // text so far is what the ground gets, which is better than losing the event.
        #[allow(
            clippy::let_underscore_must_use,
            reason = "a failing `Display` keeps the text so far; the event is still sent"
        )]
        let _ = written.write_fmt(text);

        let event = event::Event {
            source,
            code: code.code(),
            severity: code.severity(),
            monotonic_us,
            text: written,
        };
        // The system log gets the event now, before the queue, so an event the queue cannot take
        // is still seen there. Its own time is in the line, because the line's time is when it
        // was written, which can be later.
        log::log!(
            target: "event",
            log_level(event.severity),
            "src={} code={} t={} us: {}",
            event.source,
            event.code,
            event.monotonic_us,
            event.text
        );
        // `push` gives the event back if the queue is full, and counts the refusal. Here the
        // event is dropped, so the count is all that is left of it.
        #[allow(
            clippy::let_underscore_must_use,
            reason = "a refused event is dropped on purpose, and the queue has counted it"
        )]
        let _ = self.log.push(event);
    }
}

/// The system log's level for an event's severity: `Err` is `Error`, the others have the same name.
fn log_level(severity: event::Severity) -> log::Level {
    match severity {
        event::Severity::Info => log::Level::Info,
        event::Severity::Warn => log::Level::Warn,
        event::Severity::Err => log::Level::Error,
    }
}

impl Default for Ev {
    fn default() -> Self {
        Self::new()
    }
}
