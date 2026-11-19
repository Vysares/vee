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
//! Who calls what: a sender calls `send`. The event log and the downlink call `pop` to take the
//! oldest event, and the log calls `take_dropped` to find out how many events did not fit.

use crate::config;
use crate::event;
use crate::queue;
use core::fmt;
use core::fmt::Write as _;

config::setting! {
    /// How many events the event queue holds. Events wait in it until the event log and the
    /// downlink take them, so it has to cover a burst between two turns of the loop. It is the
    /// same for the whole system, because there is only one queue. Each slot is 80 bytes with the
    /// default `EV_TEXT_MAX_BYTES` of 64 (the 12 byte envelope and the text, rounded up to a
    /// multiple of 8), so the default of 1024 is 80 KiB. There is no per source cap, so the worst
    /// case in one tick is
    /// every source sending every one of its events `EV_MAX_PER_TICK` times; size for that.
    pub EV_QUEUE_DEPTH: usize = 1024, 1..=65535;
}

/// A first-in first-out queue of up to `EV_QUEUE_DEPTH` events, and a count of the events that
/// did not fit. Nothing is allocated.
///
/// This is the core's, not a module's. It is `pub` and lives in this crate only because `Ctx`
/// (which modules see) holds the handle `ctx.ev`, and that handle holds a `&mut Ev`. A module
/// reaches it only through `ctx.ev.send`.
pub struct Ev {
    queue: queue::Queue<event::Event, EV_QUEUE_DEPTH>,
    /// Events that were sent while the queue was full, since `take_dropped` was last called.
    dropped: u32,
}

impl Ev {
    pub const fn new() -> Self {
        Ev {
            queue: queue::Queue::new(),
            dropped: 0,
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
    /// - If the queue is full, the new event is dropped (the oldest are kept, because the first
    ///   events of a problem are the most useful) and counted in `dropped`.
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
        // `push` gives the event back if the queue is full. Here it is dropped and counted.
        if self.queue.push(event).is_err() {
            self.dropped = self.dropped.saturating_add(1);
        }
    }

    /// Takes the oldest event out of the queue, or `None` if it is empty.
    pub fn pop(&mut self) -> Option<event::Event> {
        self.queue.pop()
    }

    /// How many events are waiting.
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// How many events were dropped because the queue was full, since this was last called. The
    /// count starts again from 0. The event log sends it on as an event, so a loss is never
    /// silent.
    pub fn take_dropped(&mut self) -> u32 {
        core::mem::take(&mut self.dropped)
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
