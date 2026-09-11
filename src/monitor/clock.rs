//! Testable clock/sleep abstraction.
//!
//! The polling loop needs to "wait between polls" without unit tests ever
//! blocking on a real sleep. [`Clock`] is the seam: production code uses
//! [`SystemClock`], tests use [`FakeClock`], which records sleep requests
//! instead of blocking so a whole polling run finishes instantly.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Abstraction over "what time is it" and "wait for a while".
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
    fn sleep(&self, duration: Duration);
}

/// Real wall-clock time, real sleeping. Used in production.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// A clock for tests: `now()` advances only by the durations passed to
/// `sleep`, and `sleep` never actually blocks — it just records the
/// requested duration so tests can assert on polling cadence and finish
/// instantly regardless of configured intervals.
#[derive(Debug, Clone)]
pub struct FakeClock {
    inner: Arc<Mutex<FakeClockState>>,
}

#[derive(Debug)]
struct FakeClockState {
    epoch: Instant,
    elapsed: Duration,
    sleeps: Vec<Duration>,
}

impl FakeClock {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(FakeClockState {
                epoch: Instant::now(),
                elapsed: Duration::ZERO,
                sleeps: Vec::new(),
            })),
        }
    }

    /// All durations previously passed to `sleep`, in order.
    pub fn recorded_sleeps(&self) -> Vec<Duration> {
        self.inner
            .lock()
            .expect("fake clock mutex poisoned")
            .sleeps
            .clone()
    }

    /// Number of times `sleep` has been called.
    pub fn sleep_count(&self) -> usize {
        self.recorded_sleeps().len()
    }
}

impl Default for FakeClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Instant {
        let state = self.inner.lock().expect("fake clock mutex poisoned");
        state.epoch + state.elapsed
    }

    fn sleep(&self, duration: Duration) {
        let mut state = self.inner.lock().expect("fake clock mutex poisoned");
        state.elapsed += duration;
        state.sleeps.push(duration);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_clock_sleep_does_not_block_and_advances_now() {
        let clock = FakeClock::new();
        let before = clock.now();
        clock.sleep(Duration::from_secs(3600));
        let after = clock.now();
        assert_eq!(after - before, Duration::from_secs(3600));
        assert_eq!(clock.recorded_sleeps(), vec![Duration::from_secs(3600)]);
    }

    #[test]
    fn fake_clock_records_every_sleep_call() {
        let clock = FakeClock::new();
        clock.sleep(Duration::from_secs(1));
        clock.sleep(Duration::from_secs(2));
        assert_eq!(clock.sleep_count(), 2);
    }
}
