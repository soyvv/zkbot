use thiserror::Error;

/// Request to register or replace a timer.
#[derive(Debug, Clone)]
pub struct TimerRequest {
    /// Unique key identifying this timer. Re-using a key replaces the old timer.
    pub timer_key: String,
    /// When and how often the timer should fire.
    pub schedule: TimerSchedule,
    /// How to handle overdue recurring fires.
    pub coalesce: TimerCoalescePolicy,
}

/// Describes when a timer should fire.
#[derive(Debug, Clone)]
pub enum TimerSchedule {
    /// Fire exactly once at the given timestamp.
    OnceAt { fire_at_ms: i64 },
    /// Fire repeatedly at a fixed interval.
    Interval {
        every_ms: i64,
        start_ms: Option<i64>,
        end_ms: Option<i64>,
    },
    /// Fire according to a 6-field cron expression.
    Cron {
        expr: String,
        start_ms: Option<i64>,
        end_ms: Option<i64>,
    },
}

/// Policy for handling multiple overdue recurring fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerCoalescePolicy {
    /// Emit every missed occurrence individually.
    FireAll,
    /// Coalesce all missed occurrences into a single fire at the latest overdue time.
    FireOnce,
    /// Skip all missed occurrences and advance the schedule past the current time.
    SkipMissed,
}

/// A timer fire event produced by `advance_to`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimerFire {
    /// The timer key that fired.
    pub timer_key: String,
    /// The scheduled timestamp for this fire.
    pub scheduled_ts_ms: i64,
    /// The logical time at which the fire was dispatched (the `advance_to` target).
    pub dispatch_ts_ms: i64,
    /// `dispatch_ts_ms - scheduled_ts_ms`.
    pub lag_ms: i64,
}

/// Errors produced by clock operations.
#[derive(Debug, Clone, PartialEq, Eq)]
#[derive(Error)]
pub enum ClockError {
    /// The cron expression could not be parsed.
    #[error("invalid cron expression: {0}")]
    InvalidCronExpression(String),
    /// The interval configuration is invalid (e.g. `every_ms <= 0`).
    #[error("invalid interval: {0}")]
    InvalidInterval(String),
    /// Attempted to advance time backward.
    #[error("time moved backward: current={current_ms}, requested={requested_ms}")]
    TimeMovedBackward { current_ms: i64, requested_ms: i64 },
}

