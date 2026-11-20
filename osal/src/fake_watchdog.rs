//! A `Watchdog` for tests: it counts the feeds it gets, and can be told to fail them, so a test can
//! check what the core does about a failing supervisor.

use crate::watchdog;
use std::sync::atomic;

/// Public (not test-only) because `vee-core`'s tests use it too. The state is atomics, so the
/// `&self` methods need no lock.
#[derive(Debug, Default)]
pub struct FakeWatchdog {
    feed_calls: atomic::AtomicU32,
    failing: atomic::AtomicBool,
}

impl FakeWatchdog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Test setup: from now on `feed` fails with `WatchdogError::Other` (while true),
    /// or succeeds again (while false). A failed call is still counted.
    pub fn set_failing(&self, failing: bool) {
        self.failing.store(failing, atomic::Ordering::Relaxed);
    }

    /// How many times `feed` was called, failed or not.
    pub fn feed_calls(&self) -> u32 {
        self.feed_calls.load(atomic::Ordering::Relaxed)
    }

    fn result(&self) -> Result<(), watchdog::WatchdogError> {
        if self.failing.load(atomic::Ordering::Relaxed) {
            Err(watchdog::WatchdogError::Other)
        } else {
            Ok(())
        }
    }
}

impl watchdog::Watchdog for FakeWatchdog {
    fn feed(&self) -> Result<(), watchdog::WatchdogError> {
        self.feed_calls.fetch_add(1, atomic::Ordering::Relaxed);
        self.result()
    }
}

#[cfg(test)]
mod tests {
    use super as fake_watchdog;
    use crate::watchdog::Watchdog as _;
    use crate::watchdog::WatchdogError;

    #[test]
    fn a_new_fake_has_seen_no_calls() {
        let fake = fake_watchdog::FakeWatchdog::new();
        assert_eq!(fake.feed_calls(), 0);
    }

    #[test]
    fn feeds_are_counted() {
        let fake = fake_watchdog::FakeWatchdog::new();
        assert_eq!(fake.feed(), Ok(()));
        assert_eq!(fake.feed(), Ok(()));
        assert_eq!(fake.feed_calls(), 2);
    }

    #[test]
    fn a_failing_fake_fails_the_feed_and_still_counts_it() {
        let fake = fake_watchdog::FakeWatchdog::new();
        fake.set_failing(true);
        assert_eq!(fake.feed(), Err(WatchdogError::Other));
        assert_eq!(fake.feed_calls(), 1);
    }

    #[test]
    fn a_fake_that_stops_failing_succeeds_again() {
        let fake = fake_watchdog::FakeWatchdog::new();
        fake.set_failing(true);
        assert!(fake.feed().is_err());
        fake.set_failing(false);
        assert_eq!(fake.feed(), Ok(()));
    }
}
