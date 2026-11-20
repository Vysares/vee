//! The real `Clock`, on top of `std::time::Instant`.

use crate::clock;
use std::thread;
use std::time;

/// Counts from the moment it was created. Create it once at startup, before anything reads time.
#[derive(Debug, Clone, Copy)]
pub struct StdClock {
    start: time::Instant,
}

impl StdClock {
    pub fn new() -> Self {
        Self {
            start: time::Instant::now(),
        }
    }
}

impl Default for StdClock {
    fn default() -> Self {
        Self::new()
    }
}

impl clock::Clock for StdClock {
    fn monotonic_us(&self) -> u64 {
        // `as_micros` is a `u128`. A `u64` of microseconds lasts about 584,000 years, so the
        // conversion cannot fail in practice. If it ever did, stay at the maximum, which still
        // never goes backwards.
        u64::try_from(self.start.elapsed().as_micros()).unwrap_or(u64::MAX)
    }

    fn sleep_until_us(&self, deadline_us: u64) {
        let Some(remaining_us) = deadline_us.checked_sub(self.monotonic_us()) else {
            return; // the deadline has passed
        };
        // `sleep` waits at least this long, so the clock is at the deadline or past it.
        thread::sleep(time::Duration::from_micros(remaining_us));
    }
}

#[cfg(test)]
mod tests {
    use super as std_clock;
    use crate::clock::Clock as _;
    use std::thread;
    use std::time;

    #[test]
    fn never_goes_backwards() {
        let clock = std_clock::StdClock::new();
        let mut last = clock.monotonic_us();
        for _ in 0..1000 {
            let now = clock.monotonic_us();
            assert!(now >= last);
            last = now;
        }
    }

    #[test]
    fn advances_while_time_passes() {
        let clock = std_clock::StdClock::new();
        let before = clock.monotonic_us();
        thread::sleep(time::Duration::from_millis(5));
        let after = clock.monotonic_us();
        // `sleep` waits at least this long, so at least 5000 us must have been counted.
        assert!(after - before >= 5000);
    }

    #[test]
    fn sleep_until_returns_at_or_after_the_deadline() {
        let clock = std_clock::StdClock::new();
        let deadline = clock.monotonic_us() + 5_000;
        clock.sleep_until_us(deadline);
        assert!(clock.monotonic_us() >= deadline);
    }

    #[test]
    fn sleep_until_a_deadline_in_the_past_returns_at_once() {
        let clock = std_clock::StdClock::new();
        thread::sleep(time::Duration::from_millis(2));
        let started = time::Instant::now();
        clock.sleep_until_us(0);
        assert!(started.elapsed() < time::Duration::from_millis(500));
    }

    #[test]
    fn starts_near_zero() {
        // A fresh clock counts from its creation, not from boot or from the epoch.
        let clock = std_clock::StdClock::new();
        assert!(clock.monotonic_us() < 1_000_000);
    }
}
