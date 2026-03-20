use crate::clock::Clock;
use crate::scheduler::Scheduler;
use crate::types::{ClockError, TimerFire, TimerRequest};

/// Simulation clock for deterministic backtesting.
///
/// Time advances only when explicitly called via `advance_to`.
/// All timer fires use the advancement target as `dispatch_ts_ms`.
pub struct SimClock {
    current_ms: i64,
    pub(crate) scheduler: Scheduler,
}

impl SimClock {
    /// Create a new `SimClock` starting at the given timestamp.
    pub fn new(start_ms: i64) -> Self {
        Self {
            current_ms: start_ms,
            scheduler: Scheduler::new(),
        }
    }
}

impl Clock for SimClock {
    fn now_ms(&self) -> i64 {
        self.current_ms
    }

    fn set_timer(&mut self, req: TimerRequest) -> Result<(), ClockError> {
        self.scheduler.register(req, self.current_ms)
    }

    fn cancel_timer(&mut self, timer_key: &str) -> bool {
        self.scheduler.cancel(timer_key)
    }

    fn advance_to(&mut self, now_ms: i64) -> Result<Vec<TimerFire>, ClockError> {
        if now_ms < self.current_ms {
            return Err(ClockError::TimeMovedBackward {
                current_ms: self.current_ms,
                requested_ms: now_ms,
            });
        }
        self.current_ms = now_ms;
        Ok(self.scheduler.drain_due(now_ms))
    }
}
