use crate::types::{ClockError, TimerFire, TimerRequest};

/// A synchronous, mode-agnostic clock that owns timer scheduling state.
///
/// Implementations include `SimClock` (backtester), `TestClock` (unit tests),
/// and eventually `LiveClock` (production engine).
pub trait Clock {
    /// Returns the current logical time in milliseconds since epoch.
    fn now_ms(&self) -> i64;

    /// Register or replace a timer. If a timer with the same key already exists,
    /// the old schedule is replaced.
    fn set_timer(&mut self, req: TimerRequest) -> Result<(), ClockError>;

    /// Cancel a timer by key. Returns `true` if the timer existed and was cancelled.
    fn cancel_timer(&mut self, timer_key: &str) -> bool;

    /// Advance logical time to `now_ms` and return all timer fires that are due.
    ///
    /// Returns an error if `now_ms` is less than the current time.
    /// Fires are ordered by `(scheduled_ts_ms, timer_key)`.
    fn advance_to(&mut self, now_ms: i64) -> Result<Vec<TimerFire>, ClockError>;
}
