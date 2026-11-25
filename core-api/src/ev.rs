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
//! Who calls what: a sender calls `send`, which puts the event in the log queue, and, if the bus
//! filter allows the event's severity, also in the bus queue. The event log's writer takes events
//! out of the log queue (looking at the next one with `peek` and taking it with `pop` once it has
//! room for it), and the core sends what the queue refused (`take_refused`) on as events.
//! `flush_bus` takes events out of the bus queue and publishes them to the events topic: see
//! `docs/bus.md`, "Events on the bus".

use crate::bus;
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

config::setting! {
    /// How many events the BUS queue holds, separately from the log queue above. `flush_bus`
    /// drains it into the events topic once a tick, so it only needs to hold what one tick's
    /// worth of filtered events adds, plus margin. Default 64, 1..=65535.
    pub EV_BUS_QUEUE_DEPTH: usize = 64, 1..=65535;
}

/// The event service: the log queue, the bus queue and the bus filter. Nothing is allocated.
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
    /// Events that passed `filter`, waiting for `flush_bus` to publish them to the events topic.
    /// Separate from `log`, so a full bus queue (a slow telemetry reader) never affects what the
    /// event log keeps, and vice versa.
    pub bus: queue::Queue<event::Event, EV_BUS_QUEUE_DEPTH>,
    /// The bus queue's severity floor. `None` is Off, the default: nothing reaches `bus` until a
    /// core command sets a floor (the command is not written yet, see `docs/bus.md`). `Some(s)`
    /// lets `s` and worse through.
    filter: Option<event::Severity>,
}

impl Ev {
    pub const fn new() -> Self {
        Ev {
            log: queue::Queue::new(),
            bus: queue::Queue::new(),
            filter: None,
        }
    }

    /// Sets the bus queue's severity floor. `None` turns it off (the default): no event reaches
    /// `bus` until this is called with `Some`. Will be called from the core command that switches
    /// the filter, once that command exists; nothing else should hold a handle to it.
    pub fn set_filter(&mut self, filter: Option<event::Severity>) {
        self.filter = filter;
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

        // The bus queue gets a second copy, only if the filter lets this severity through
        // (`Event` is `Copy`, so this is not the sender's event, just as `self.log.push` above
        // took its own copy).
        if let Some(floor) = self.filter
            && event.severity >= floor
        {
            #[allow(
                clippy::let_underscore_must_use,
                reason = "a refused event is dropped on purpose, and the queue has counted it"
            )]
            let _ = self.bus.push(event);
        }
    }

    /// Publishes everything waiting in the bus queue to the events topic, oldest first, then
    /// empties the queue. Called once a tick, next to the log writer's `step` (see
    /// `docs/bus.md`, "Events on the bus"); not yet called by `Core::tick`, because nothing reads
    /// the events topic until telemetry exists to size it (`N`, `D`).
    ///
    /// A publish that a `Refuse` reader's full queue turns down is already counted on that
    /// reader's own queue (`QueuedTopic::take_refused`): there is nothing left to do with it
    /// here, the same as a full `log` or `bus` queue above.
    pub fn flush_bus<const N: usize, const D: usize>(
        &mut self,
        topic: &mut bus::QueuedTopic<event::Event, N, D>,
    ) {
        while let Some(event) = self.bus.pop() {
            #[allow(
                clippy::let_underscore_must_use,
                reason = "a refused reader has already counted it on its own queue"
            )]
            let _ = topic.publish(event);
        }
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

#[cfg(test)]
mod tests {
    use super as ev;
    use crate::bus;
    use crate::event;

    /// An enum of codes written out by hand (the derive's output does not resolve inside this
    /// crate: see the same note in `throttle.rs`'s tests), with a different severity per variant
    /// so the filter tests can tell them apart.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Sample {
        Info,
        Warn,
        Err,
    }

    impl event::Code for Sample {
        fn code(&self) -> u16 {
            *self as u16
        }
        fn severity(&self) -> event::Severity {
            match self {
                Sample::Info => event::Severity::Info,
                Sample::Warn => event::Severity::Warn,
                Sample::Err => event::Severity::Err,
            }
        }
        fn index(&self) -> usize {
            *self as usize
        }
        fn from_index(index: usize) -> Option<Self> {
            [Sample::Info, Sample::Warn, Sample::Err]
                .get(index)
                .copied()
        }
    }

    #[test]
    fn the_bus_queue_is_off_by_default_and_stays_empty() {
        let mut sent = ev::Ev::new();
        sent.send(1, 0, Sample::Err, format_args!("a"));
        assert!(sent.bus.is_empty());
        assert!(!sent.log.is_empty()); // the log queue is unaffected by the filter
    }

    #[test]
    fn a_filter_lets_through_its_floor_and_worse_only() {
        let mut sent = ev::Ev::new();
        sent.set_filter(Some(event::Severity::Warn));
        sent.send(1, 0, Sample::Info, format_args!("info"));
        sent.send(1, 0, Sample::Warn, format_args!("warn"));
        sent.send(1, 0, Sample::Err, format_args!("err"));
        assert_eq!(sent.bus.len(), 2);
        assert_eq!(sent.bus.pop().unwrap().text.as_str(), "warn");
        assert_eq!(sent.bus.pop().unwrap().text.as_str(), "err");
    }

    #[test]
    fn a_full_bus_queue_refuses_and_counts_separately_from_the_log_queue() {
        let mut sent = ev::Ev::new();
        sent.set_filter(Some(event::Severity::Info)); // everything passes
        for _ in 0..ev::EV_BUS_QUEUE_DEPTH.saturating_add(3) {
            sent.send(1, 0, Sample::Info, format_args!("x"));
        }
        assert_eq!(sent.bus.len(), ev::EV_BUS_QUEUE_DEPTH);
        assert_eq!(sent.bus.take_refused(), 3);
        // The log queue (depth 1024 by default) took every one of them: its own refusal count
        // is unaffected by the bus queue filling up.
        assert_eq!(sent.log.take_refused(), 0);
    }

    #[test]
    fn flush_bus_publishes_everything_waiting_in_order_and_empties_the_queue() {
        let mut sent = ev::Ev::new();
        sent.set_filter(Some(event::Severity::Info));
        sent.send(1, 10, Sample::Info, format_args!("one"));
        sent.send(1, 20, Sample::Warn, format_args!("two"));

        let mut topic = bus::QueuedTopic::<event::Event, 1, 4>::new([bus::Policy::Drop]);
        sent.flush_bus(&mut topic);

        assert!(sent.bus.is_empty());
        let first = topic.pop(0).unwrap();
        assert_eq!(first.text.as_str(), "one");
        let second = topic.pop(0).unwrap();
        assert_eq!(second.text.as_str(), "two");
        assert_eq!(topic.pop(0), None);
    }

    #[test]
    fn flush_bus_does_not_stop_at_a_reader_that_refuses() {
        let mut sent = ev::Ev::new();
        sent.set_filter(Some(event::Severity::Info));
        sent.send(1, 10, Sample::Info, format_args!("one"));
        sent.send(1, 20, Sample::Info, format_args!("two"));

        // Depth 1, Refuse: the second publish is turned down, but flush keeps going (it does not
        // see or care about the `Err`, the same as `Ev::send` does not retry a full `log`).
        let mut topic = bus::QueuedTopic::<event::Event, 1, 1>::new([bus::Policy::Refuse]);
        sent.flush_bus(&mut topic);

        assert!(sent.bus.is_empty());
        let first = topic.pop(0).unwrap();
        assert_eq!(first.text.as_str(), "one");
        assert_eq!(topic.pop(0), None); // "two" was lost: the queue was already full with "one"
    }

    #[test]
    fn flush_bus_with_nothing_waiting_publishes_nothing_and_does_not_fail() {
        let mut sent = ev::Ev::new();
        let mut topic = bus::QueuedTopic::<event::Event, 1, 2>::new([bus::Policy::Drop]);
        sent.flush_bus(&mut topic);
        assert_eq!(topic.len(0), 0);
        // With the filter on but nothing sent, the same.
        sent.set_filter(Some(event::Severity::Info));
        sent.flush_bus(&mut topic);
        assert_eq!(topic.len(0), 0);
    }

    #[test]
    fn flush_bus_into_a_topic_with_no_readers_empties_the_queue_and_does_not_panic() {
        let mut sent = ev::Ev::new();
        sent.set_filter(Some(event::Severity::Info));
        sent.send(1, 0, Sample::Info, format_args!("one"));
        let mut topic = bus::QueuedTopic::<event::Event, 0, 2>::new([]);
        sent.flush_bus(&mut topic);
        assert!(sent.bus.is_empty());
    }

    #[test]
    fn a_flush_gives_a_drop_reader_exactly_its_depth_and_counts_the_one_more() {
        let mut sent = ev::Ev::new();
        sent.set_filter(Some(event::Severity::Info));
        for time in 0..3 {
            sent.send(1, time, Sample::Info, format_args!("e"));
        }
        // A reader that holds 2: the third is dropped, and counted on the reader's own queue.
        let mut topic = bus::QueuedTopic::<event::Event, 1, 2>::new([bus::Policy::Drop]);
        sent.flush_bus(&mut topic);
        assert_eq!(topic.len(0), 2);
        assert_eq!(topic.take_refused(0), 1);
        assert_eq!(topic.pop(0).map(|event| event.monotonic_us), Some(0));
        assert_eq!(topic.pop(0).map(|event| event.monotonic_us), Some(1));
    }

    #[test]
    fn turning_the_filter_off_keeps_what_is_queued_and_a_filter_on_later_misses_what_came_before() {
        let mut sent = ev::Ev::new();
        // Off at first: this event is not queued for the bus.
        sent.send(1, 0, Sample::Err, format_args!("early"));
        sent.set_filter(Some(event::Severity::Info));
        sent.send(1, 1, Sample::Info, format_args!("kept"));
        // Off again: what is already in the queue stays, and a flush still delivers it.
        sent.set_filter(None);
        sent.send(1, 2, Sample::Err, format_args!("late"));

        let mut topic = bus::QueuedTopic::<event::Event, 1, 4>::new([bus::Policy::Drop]);
        sent.flush_bus(&mut topic);
        assert_eq!(topic.len(0), 1);
        assert_eq!(topic.pop(0).map(|event| event.monotonic_us), Some(1));
        // The log queue got all three, whatever the filter did.
        assert_eq!(sent.log.len(), 3);
    }

    #[test]
    fn every_severity_meets_every_filter_the_right_way() {
        let filters = [
            None,
            Some(event::Severity::Info),
            Some(event::Severity::Warn),
            Some(event::Severity::Err),
        ];
        let events = [
            (Sample::Info, event::Severity::Info),
            (Sample::Warn, event::Severity::Warn),
            (Sample::Err, event::Severity::Err),
        ];
        for filter in filters {
            for (sample, severity) in events {
                let mut sent = ev::Ev::new();
                sent.set_filter(filter);
                sent.send(1, 0, sample, format_args!("x"));
                let reaches = filter.is_some_and(|floor| severity >= floor);
                assert_eq!(
                    sent.bus.len(),
                    usize::from(reaches),
                    "{filter:?} {severity:?}"
                );
            }
        }
    }
}
