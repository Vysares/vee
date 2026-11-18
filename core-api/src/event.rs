//! The event envelope: what every event looks like once it has been built, whatever module it
//! came from.
//!
//! Modules do not make an `Event` themselves. They send a typed event through `ctx.ev`, which
//! encodes its args and fills in `source` and `monotonic_us`. The core then queues the `Event`
//! for the event log and the downlink.

/// How important an event is. Each event code has one severity, fixed in the module's types
/// crate, so the caller of `ctx.ev.send` never picks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warn,
    Err,
}

/// How many bytes of args an event can carry. Every event is the same size, so an event that
/// uses fewer bytes is zero padded.
pub const EV_ARGS_MAX_BYTES: usize = 32;

// `Event::len` is a `u8`, so the maximum must fit in one.
const _: () = assert!(EV_ARGS_MAX_BYTES <= u8::MAX as usize);

/// Why an `Event` could not be made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The args are longer than `EV_ARGS_MAX_BYTES`.
    ArgsTooLong,
}

/// One event. `len` and `args` are private so they always agree: `len` is at most
/// `EV_ARGS_MAX_BYTES`, and every byte of `args` from `len` on is zero. Because of that, the
/// derived `==` (which compares all of `args`) means the same as comparing only the used bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    /// The module that sent the event. 0 is the core.
    pub source: u8,
    /// Which event this is. Unique within its source.
    pub code: u16,
    pub severity: Severity,
    /// Microseconds since this boot, from the monotonic clock. Restarts at every boot.
    pub monotonic_us: u64,
    /// How many bytes of `args` are used.
    len: u8,
    args: [u8; EV_ARGS_MAX_BYTES],
}

impl Event {
    /// Builds an event. `args` are the used bytes only (at most `EV_ARGS_MAX_BYTES`). They are
    /// copied in and zero padded to the full size.
    pub fn new(
        source: u8,
        code: u16,
        severity: Severity,
        monotonic_us: u64,
        args: &[u8],
    ) -> Result<Self, Error> {
        if args.len() > EV_ARGS_MAX_BYTES {
            return Err(Error::ArgsTooLong);
        }
        let mut padded = [0u8; EV_ARGS_MAX_BYTES];
        padded[..args.len()].copy_from_slice(args);
        Ok(Event {
            source,
            code,
            severity,
            monotonic_us,
            len: args.len() as u8, // fits: `EV_ARGS_MAX_BYTES` is at most `u8::MAX`, asserted above
            args: padded,
        })
    }

    /// The used bytes of the args, without the padding.
    pub fn args(&self) -> &[u8] {
        &self.args[..usize::from(self.len)]
    }
}

#[cfg(test)]
mod tests {
    use super as event;

    fn make(args: &[u8]) -> Result<event::Event, event::Error> {
        event::Event::new(2, 0x0102, event::Severity::Warn, 1_000_000, args)
    }

    #[test]
    fn new_keeps_the_fields() {
        let ev = make(&[1, 2, 3]).unwrap();
        assert_eq!(ev.source, 2);
        assert_eq!(ev.code, 0x0102);
        assert_eq!(ev.severity, event::Severity::Warn);
        assert_eq!(ev.monotonic_us, 1_000_000);
    }

    #[test]
    fn args_are_the_used_bytes_only() {
        let ev = make(&[1, 2, 3]).unwrap();
        assert_eq!(ev.args(), [1, 2, 3]);
    }

    #[test]
    fn the_rest_of_the_args_is_zero_padding() {
        let ev = make(&[1, 2, 3]).unwrap();
        assert!(ev.args[3..].iter().all(|&byte| byte == 0));
    }

    #[test]
    fn no_args_is_allowed() {
        let ev = make(&[]).unwrap();
        assert_eq!(ev.args(), []);
        assert_eq!(ev.args, [0; event::EV_ARGS_MAX_BYTES]);
    }

    #[test]
    fn args_exactly_the_maximum_fit() {
        let full = [7u8; event::EV_ARGS_MAX_BYTES];
        let ev = make(&full).unwrap();
        assert_eq!(ev.args(), full);
    }

    #[test]
    fn args_one_over_the_maximum_are_rejected() {
        let too_long = [7u8; event::EV_ARGS_MAX_BYTES + 1];
        assert_eq!(make(&too_long), Err(event::Error::ArgsTooLong));
    }

    #[test]
    fn events_with_the_same_contents_are_equal() {
        assert_eq!(make(&[1, 2]), make(&[1, 2]));
    }

    #[test]
    fn events_with_different_args_are_not_equal() {
        assert_ne!(make(&[1, 2]), make(&[1, 3]));
        assert_ne!(make(&[1, 2]), make(&[1, 2, 0]));
    }
}
