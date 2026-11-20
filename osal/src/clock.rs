//! The clock the core and the executive read time from.

/// A monotonic clock: it only counts up and never jumps when someone sets the wall clock.
///
/// The method takes `&self`, like `Files`, so one clock can be shared.
pub trait Clock {
    /// Microseconds since this clock started. This is NOT mission time: the count restarts at 0
    /// each boot.
    fn monotonic_us(&self) -> u64;

    /// Waits until `monotonic_us()` is at least `deadline_us`. If it already is, returns at once
    /// (a deadline in the past is not an error: the caller was late). This is how the
    /// executive waits for the start of the next tick. It is the one call in Vee that is meant to
    /// block, and only the executive's loop makes it.
    fn sleep_until_us(&self, deadline_us: u64);
}
