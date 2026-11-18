//! The clock the core and the executive read time from.

/// A monotonic clock: it only counts up and never jumps when someone sets the wall clock.
///
/// The method takes `&self`, like `Files`, so one clock can be shared.
pub trait Clock {
    /// Microseconds since this clock started. This is NOT mission time: the count restarts at 0
    /// each boot.
    fn monotonic_us(&self) -> u64;
}
