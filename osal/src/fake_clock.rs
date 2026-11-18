//! A `Clock` that only moves when a test says so, so scheduling tests are exact.

use crate::clock;
use std::sync::atomic;

/// Starts at 0. `Clock::monotonic_us` takes `&self`, but this fake has to change, so the time is
/// an atomic (no lock needed for a single number). Public (not test-only) because `vee-core`'s
/// tests use it too.
#[derive(Debug, Default)]
pub struct FakeClock {
    now_us: atomic::AtomicU64,
}

impl FakeClock {
    pub fn new() -> Self {
        Self::default()
    }

    /// Test setup: jumps to this time. The test is responsible for not going backwards.
    pub fn set(&self, now_us: u64) {
        self.now_us.store(now_us, atomic::Ordering::Relaxed);
    }

    /// Test setup: moves time forward. (Wraps past `u64::MAX`; a test will not get there.)
    pub fn advance(&self, delta_us: u64) {
        self.now_us.fetch_add(delta_us, atomic::Ordering::Relaxed);
    }
}

impl clock::Clock for FakeClock {
    fn monotonic_us(&self) -> u64 {
        self.now_us.load(atomic::Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super as fake_clock;
    use crate::clock::Clock as _;

    #[test]
    fn starts_at_zero() {
        let fake = fake_clock::FakeClock::new();
        assert_eq!(fake.monotonic_us(), 0);
    }

    #[test]
    fn does_not_move_by_itself() {
        let fake = fake_clock::FakeClock::new();
        fake.set(500);
        assert_eq!(fake.monotonic_us(), 500);
        assert_eq!(fake.monotonic_us(), 500);
    }

    #[test]
    fn advance_adds() {
        let fake = fake_clock::FakeClock::new();
        fake.advance(10_000);
        fake.advance(10_000);
        assert_eq!(fake.monotonic_us(), 20_000);
    }

    #[test]
    fn set_replaces_the_time() {
        let fake = fake_clock::FakeClock::new();
        fake.advance(10);
        fake.set(7);
        assert_eq!(fake.monotonic_us(), 7);
    }
}
