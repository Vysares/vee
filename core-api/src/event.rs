//! The event envelope: what every event looks like once it has been built, whatever module it
//! came from.
//!
//! An event is a code and a short text. The code says which event it is, and fixes its severity.
//! `Code` is the trait for an enum of codes, and `#[derive(event::Code)]` writes it.
//!
//! Modules do not make an `Event` themselves. They send a code and a text through `ctx.ev`,
//! which fills in `source` and `monotonic_us`. The core then queues the `Event` for the event log
//! and the downlink.

use crate::config;
use crate::fixed_str;

/// The derive of the same name: `#[derive(event::Code)]` writes `impl Code`. (A trait and a
/// derive can share a name, because they live in different namespaces.)
pub use vee_core_derive::Code;

/// How important an event is. Each event code has one severity, fixed where the code is defined,
/// so the caller of `ctx.ev.send` never picks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warn,
    Err,
}

config::setting! {
    /// How many bytes of text an event can carry. Every event is the same size, so shorter text is
    /// zero padded, and longer text is cut off and ends in `fixed_str::TRUNCATED_MARK`. It sets
    /// the size of an event log record, so changing it makes existing event logs unreadable
    /// (they are rotated away at startup, not trusted).
    pub EV_TEXT_MAX_BYTES: usize = 50, 1..=4096;
}

/// An event's text.
pub type Text = fixed_str::FixedStr<EV_TEXT_MAX_BYTES>;

/// One event. All its fields are public: any combination is a valid event, because `Text` always
/// holds valid text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    /// The module that sent the event. 0 is the core.
    pub source: u8,
    /// Which event this is. Unique within its source.
    pub code: u16,
    pub severity: Severity,
    /// Microseconds since this boot, from the monotonic clock. Restarts at every boot.
    pub monotonic_us: u64,
    pub text: Text,
}

/// An enum of event codes, one module's (or the core's). `#[derive(event::Code)]` writes this.
///
/// Who calls what: `ctx.ev.send` calls `code` and `severity` on the code it is given, and puts
/// them in the `Event` it builds. `Throttle` calls `index` and `from_index`.
pub trait Code {
    /// Unique within the enum. It is the value given to the variant.
    fn code(&self) -> u16;

    fn severity(&self) -> Severity;

    /// The variant's position among the variants, in the order written: 0 for the first, 1 for
    /// the second, and so on. It is not the code, which can be any value. It is the index into
    /// an array with one slot per event (see `throttle`).
    fn index(&self) -> usize;

    /// The reverse of `index`: the variant at a position, or `None` if there is no such
    /// variant.
    fn from_index(index: usize) -> Option<Self>
    where
        Self: Sized;
}

#[cfg(test)]
mod tests {
    use super as event;
    use crate::fixed_str;

    fn make(text: &str) -> event::Event {
        event::Event {
            source: 2,
            code: 0x0102,
            severity: event::Severity::Warn,
            monotonic_us: 1_000_000,
            text: fixed_str::FixedStr::truncated(text).unwrap(),
        }
    }

    #[test]
    fn events_with_the_same_contents_are_equal() {
        assert_eq!(make("a"), make("a"));
    }

    #[test]
    fn events_with_different_text_are_not_equal() {
        assert_ne!(make("a"), make("b"));
    }
}
