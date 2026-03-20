use crate::clock::Clock;
use crate::sim_clock::SimClock;
use crate::types::{ClockError, TimerFire, TimerRequest};

/// Test clock with convenience helpers for unit and integration tests.
///
/// Wraps `SimClock` and adds `tick()` for relative time advancement
/// and `peek_next_fire_ms()` for schedule inspection.
pub struct TestClock {
    inner: SimClock,
}

impl TestClock {
    /// Create a new `TestClock` starting at the given timestamp.
    pub fn new(start_ms: i64) -> Self {
        Self {
            inner: SimClock::new(start_ms),
        }
    }

    /// Advance time by `delta_ms` relative to the current time.
    pub fn tick(&mut self, delta_ms: i64) -> Result<Vec<TimerFire>, ClockError> {
        let target = self.inner.now_ms() + delta_ms;
        self.advance_to(target)
    }

    /// Peek at the next scheduled fire time without advancing.
    /// Returns `None` if no timers are registered.
    pub fn peek_next_fire_ms(&self) -> Option<i64> {
        self.inner.scheduler.peek_next_fire_ms()
    }

    /// Returns true if there are no active timers.
    pub fn is_empty(&self) -> bool {
        self.inner.scheduler.is_empty()
    }
}

impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.inner.now_ms()
    }

    fn set_timer(&mut self, req: TimerRequest) -> Result<(), ClockError> {
        self.inner.set_timer(req)
    }

    fn cancel_timer(&mut self, timer_key: &str) -> bool {
        self.inner.cancel_timer(timer_key)
    }

    fn advance_to(&mut self, now_ms: i64) -> Result<Vec<TimerFire>, ClockError> {
        self.inner.advance_to(now_ms)
    }
}
