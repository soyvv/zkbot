use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::str::FromStr;

use chrono::{TimeZone, Utc};
use cron::Schedule;

use crate::types::{ClockError, TimerCoalescePolicy, TimerFire, TimerRequest, TimerSchedule};

/// Maximum number of cron occurrences to enumerate in a single `drain_due` call
/// per timer. Prevents unbounded backlog replay for high-frequency crons with
/// large time jumps (e.g., per-second cron advanced by days).
const MAX_CRON_FIRES_PER_DRAIN: usize = 10_000;

// ---------------------------------------------------------------------------
// Internal state for each registered timer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum InternalSchedule {
    OnceAt,
    Interval {
        every_ms: i64,
        start_ms: i64,
        end_ms: Option<i64>,
    },
    Cron {
        schedule: Box<Schedule>,
        end_ms: Option<i64>,
    },
}

#[derive(Debug, Clone)]
struct TimerState {
    schedule: InternalSchedule,
    coalesce: TimerCoalescePolicy,
    /// Monotonically increasing generation counter to invalidate stale heap entries.
    generation: u64,
}

// ---------------------------------------------------------------------------
// Scheduler -- shared scheduling engine used by SimClock and TestClock
// ---------------------------------------------------------------------------

pub(crate) struct Scheduler {
    heap: BinaryHeap<Reverse<(i64, String, u64)>>, // (fire_ms, key, generation)
    timers: HashMap<String, TimerState>,
    cancelled: HashSet<String>,
    next_generation: u64,
}

impl Scheduler {
    pub(crate) fn new() -> Self {
        Self {
            heap: BinaryHeap::new(),
            timers: HashMap::new(),
            cancelled: HashSet::new(),
            next_generation: 0,
        }
    }

    /// Register or replace a timer. Returns error for invalid schedules.
    pub(crate) fn register(
        &mut self,
        req: TimerRequest,
        now_ms: i64,
    ) -> Result<(), ClockError> {
        let generation = self.next_generation;
        self.next_generation += 1;

        // Remove old timer if exists (by invalidating its generation)
        self.cancelled.remove(&req.timer_key);

        let (internal_schedule, first_fire_ms) = match req.schedule {
            TimerSchedule::OnceAt { fire_at_ms } => (InternalSchedule::OnceAt, fire_at_ms),
            TimerSchedule::Interval {
                every_ms,
                start_ms,
                end_ms,
            } => {
                if every_ms <= 0 {
                    return Err(ClockError::InvalidInterval(format!(
                        "every_ms must be positive, got {every_ms}"
                    )));
                }
                let start = start_ms.unwrap_or(now_ms);
                // Compute the first fire at or after `start`
                let first = if start >= now_ms {
                    start
                } else {
                    // Advance to the next occurrence at or after now_ms
                    let elapsed = now_ms - start;
                    let periods = elapsed / every_ms;
                    let candidate = start + periods * every_ms;
                    if candidate < now_ms {
                        candidate + every_ms
                    } else {
                        candidate
                    }
                };
                if let Some(end) = end_ms {
                    if first > end {
                        // Timer would never fire
                        return Ok(());
                    }
                }
                (
                    InternalSchedule::Interval {
                        every_ms,
                        start_ms: start,
                        end_ms,
                    },
                    first,
                )
            }
            TimerSchedule::Cron {
                expr,
                start_ms,
                end_ms,
            } => {
                let schedule = Schedule::from_str(&expr).map_err(|e| {
                    ClockError::InvalidCronExpression(format!("'{expr}': {e}"))
                })?;
                let start = start_ms.unwrap_or(now_ms);
                let start_dt = Utc.timestamp_millis_opt(start).unwrap();
                let first = match schedule.after(&start_dt).next() {
                    Some(dt) => dt.timestamp_millis(),
                    None => return Ok(()), // no future occurrences
                };
                if let Some(end) = end_ms {
                    if first > end {
                        return Ok(());
                    }
                }
                (InternalSchedule::Cron { schedule: Box::new(schedule), end_ms }, first)
            }
        };

        self.timers.insert(
            req.timer_key.clone(),
            TimerState {
                schedule: internal_schedule,
                coalesce: req.coalesce,
                generation,
            },
        );

        self.heap
            .push(Reverse((first_fire_ms, req.timer_key, generation)));

        Ok(())
    }

    /// Cancel a timer by key. Returns true if it was active.
    pub(crate) fn cancel(&mut self, key: &str) -> bool {
        if self.timers.remove(key).is_some() {
            self.cancelled.insert(key.to_string());
            true
        } else {
            false
        }
    }

    /// Drain all fires due at or before `now_ms`.
    /// Returns fires sorted by `(scheduled_ts_ms, timer_key)`.
    pub(crate) fn drain_due(&mut self, now_ms: i64) -> Vec<TimerFire> {
        let mut fires: Vec<TimerFire> = Vec::new();

        while let Some(&Reverse((fire_ms, _, _))) = self.heap.peek() {
            if fire_ms > now_ms {
                break;
            }
            let Reverse((fire_ms, key, gen)) = self.heap.pop().unwrap();

            // Skip cancelled or superseded entries
            if self.cancelled.contains(&key) {
                self.cancelled.remove(&key);
                continue;
            }
            let state = match self.timers.get(&key) {
                Some(s) if s.generation == gen => s,
                _ => continue, // stale generation
            };

            let coalesce = state.coalesce;

            match &state.schedule {
                InternalSchedule::OnceAt => {
                    fires.push(TimerFire {
                        timer_key: key.clone(),
                        scheduled_ts_ms: fire_ms,
                        dispatch_ts_ms: now_ms,
                        lag_ms: now_ms - fire_ms,
                    });
                    self.timers.remove(&key);
                }
                InternalSchedule::Interval {
                    every_ms,
                    start_ms,
                    end_ms,
                } => {
                    let every = *every_ms;
                    let start = *start_ms;
                    let end = *end_ms;

                    match coalesce {
                        TimerCoalescePolicy::FireAll => {
                            // Emit every missed occurrence from fire_ms up to now_ms
                            let mut t = fire_ms;
                            while t <= now_ms {
                                if end.is_none_or(|e| t <= e) {
                                    fires.push(TimerFire {
                                        timer_key: key.clone(),
                                        scheduled_ts_ms: t,
                                        dispatch_ts_ms: now_ms,
                                        lag_ms: now_ms - t,
                                    });
                                }
                                t = next_interval_fire(start, every, t);
                                if t == fire_ms {
                                    // safety: shouldn't happen with every_ms > 0
                                    break;
                                }
                            }
                            // Schedule next fire after now_ms
                            let next = next_interval_fire_at_or_after(start, every, now_ms + 1);
                            if end.is_none_or(|e| next <= e) {
                                let gen = self.timers.get(&key).unwrap().generation;
                                self.heap.push(Reverse((next, key, gen)));
                            } else {
                                self.timers.remove(&key);
                            }
                        }
                        TimerCoalescePolicy::FireOnce => {
                            // Find the latest overdue occurrence
                            let latest =
                                latest_interval_fire_at_or_before(start, every, now_ms);
                            let latest = latest.max(fire_ms);
                            if end.is_none_or(|e| latest <= e) {
                                fires.push(TimerFire {
                                    timer_key: key.clone(),
                                    scheduled_ts_ms: latest,
                                    dispatch_ts_ms: now_ms,
                                    lag_ms: now_ms - latest,
                                });
                            }
                            let next = next_interval_fire(start, every, latest);
                            if end.is_none_or(|e| next <= e) {
                                let gen = self.timers.get(&key).unwrap().generation;
                                self.heap.push(Reverse((next, key, gen)));
                            } else {
                                self.timers.remove(&key);
                            }
                        }
                        TimerCoalescePolicy::SkipMissed => {
                            // Emit on-time fires, skip overdue ones
                            if fire_ms == now_ms && end.is_none_or(|e| fire_ms <= e) {
                                fires.push(TimerFire {
                                    timer_key: key.clone(),
                                    scheduled_ts_ms: fire_ms,
                                    dispatch_ts_ms: now_ms,
                                    lag_ms: 0,
                                });
                            }
                            // Advance schedule past now_ms
                            let next = next_interval_fire_at_or_after(start, every, now_ms + 1);
                            if end.is_none_or(|e| next <= e) {
                                let gen = self.timers.get(&key).unwrap().generation;
                                self.heap.push(Reverse((next, key, gen)));
                            } else {
                                self.timers.remove(&key);
                            }
                        }
                    }
                }
                InternalSchedule::Cron { schedule, end_ms } => {
                    let cron_schedule = schedule.clone();
                    let end = *end_ms;

                    match coalesce {
                        TimerCoalescePolicy::FireAll => {
                            // Emit all missed cron occurrences (capped to prevent
                            // unbounded backlog replay per design spec).
                            let mut t_ms = fire_ms;
                            let mut count = 0usize;
                            loop {
                                if t_ms > now_ms || count >= MAX_CRON_FIRES_PER_DRAIN {
                                    break;
                                }
                                if end.is_none_or(|e| t_ms <= e) {
                                    fires.push(TimerFire {
                                        timer_key: key.clone(),
                                        scheduled_ts_ms: t_ms,
                                        dispatch_ts_ms: now_ms,
                                        lag_ms: now_ms - t_ms,
                                    });
                                    count += 1;
                                }
                                let dt = Utc.timestamp_millis_opt(t_ms).unwrap();
                                match cron_schedule.after(&dt).next() {
                                    Some(next_dt) => t_ms = next_dt.timestamp_millis(),
                                    None => {
                                        self.timers.remove(&key);
                                        break;
                                    }
                                }
                            }
                            // Schedule next after now_ms
                            if self.timers.contains_key(&key) {
                                let now_dt = Utc.timestamp_millis_opt(now_ms).unwrap();
                                if let Some(next_dt) = cron_schedule.after(&now_dt).next() {
                                    let next_ms = next_dt.timestamp_millis();
                                    if end.is_none_or(|e| next_ms <= e) {
                                        let gen = self.timers.get(&key).unwrap().generation;
                                        self.heap.push(Reverse((next_ms, key, gen)));
                                    } else {
                                        self.timers.remove(&key);
                                    }
                                } else {
                                    self.timers.remove(&key);
                                }
                            }
                        }
                        TimerCoalescePolicy::FireOnce => {
                            // Find last cron occurrence at or before now_ms
                            let mut last_ms = fire_ms;
                            let mut t_ms = fire_ms;
                            loop {
                                let dt = Utc.timestamp_millis_opt(t_ms).unwrap();
                                match cron_schedule.after(&dt).next() {
                                    Some(next_dt) => {
                                        let next_ms = next_dt.timestamp_millis();
                                        if next_ms > now_ms {
                                            break;
                                        }
                                        last_ms = next_ms;
                                        t_ms = next_ms;
                                    }
                                    None => break,
                                }
                            }
                            if end.is_none_or(|e| last_ms <= e) {
                                fires.push(TimerFire {
                                    timer_key: key.clone(),
                                    scheduled_ts_ms: last_ms,
                                    dispatch_ts_ms: now_ms,
                                    lag_ms: now_ms - last_ms,
                                });
                            }
                            // Schedule next
                            let last_dt = Utc.timestamp_millis_opt(last_ms).unwrap();
                            match cron_schedule.after(&last_dt).next() {
                                Some(next_dt) => {
                                    let next_ms = next_dt.timestamp_millis();
                                    // If next is still <= now, advance further
                                    let next_ms = if next_ms <= now_ms {
                                        let now_dt = Utc.timestamp_millis_opt(now_ms).unwrap();
                                        match cron_schedule.after(&now_dt).next() {
                                            Some(dt) => dt.timestamp_millis(),
                                            None => {
                                                self.timers.remove(&key);
                                                continue;
                                            }
                                        }
                                    } else {
                                        next_ms
                                    };
                                    if end.is_none_or(|e| next_ms <= e) {
                                        let gen = self.timers.get(&key).unwrap().generation;
                                        self.heap.push(Reverse((next_ms, key, gen)));
                                    } else {
                                        self.timers.remove(&key);
                                    }
                                }
                                None => {
                                    self.timers.remove(&key);
                                }
                            }
                        }
                        TimerCoalescePolicy::SkipMissed => {
                            // Emit on-time fires, skip overdue ones
                            if fire_ms == now_ms && end.is_none_or(|e| fire_ms <= e) {
                                fires.push(TimerFire {
                                    timer_key: key.clone(),
                                    scheduled_ts_ms: fire_ms,
                                    dispatch_ts_ms: now_ms,
                                    lag_ms: 0,
                                });
                            }
                            // Advance past now_ms
                            let now_dt = Utc.timestamp_millis_opt(now_ms).unwrap();
                            match cron_schedule.after(&now_dt).next() {
                                Some(next_dt) => {
                                    let next_ms = next_dt.timestamp_millis();
                                    if end.is_none_or(|e| next_ms <= e) {
                                        let gen = self.timers.get(&key).unwrap().generation;
                                        self.heap.push(Reverse((next_ms, key, gen)));
                                    } else {
                                        self.timers.remove(&key);
                                    }
                                }
                                None => {
                                    self.timers.remove(&key);
                                }
                            }
                        }
                    }
                }
            }
        }

        // Sort by (scheduled_ts_ms, timer_key) for deterministic output
        fires.sort_by(|a, b| {
            a.scheduled_ts_ms
                .cmp(&b.scheduled_ts_ms)
                .then_with(|| a.timer_key.cmp(&b.timer_key))
        });

        fires
    }

    /// Peek at the next scheduled fire time without advancing.
    pub(crate) fn peek_next_fire_ms(&self) -> Option<i64> {
        // Scan all heap entries to find the earliest valid one.
        let mut best: Option<i64> = None;
        for Reverse((fire_ms, key, gen)) in &self.heap {
            if self.cancelled.contains(key) {
                continue;
            }
            match self.timers.get(key) {
                Some(s) if s.generation == *gen => {
                    if best.is_none_or(|b| *fire_ms < b) {
                        best = Some(*fire_ms);
                    }
                }
                _ => continue,
            }
        }
        best
    }

    /// Returns true if there are no active timers.
    pub(crate) fn is_empty(&self) -> bool {
        self.timers.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Interval arithmetic helpers
// ---------------------------------------------------------------------------

/// Compute the next interval fire strictly after `current_ms`.
fn next_interval_fire(start_ms: i64, every_ms: i64, current_ms: i64) -> i64 {
    if current_ms < start_ms {
        return start_ms;
    }
    let elapsed = current_ms - start_ms;
    let periods = elapsed / every_ms;
    start_ms + (periods + 1) * every_ms
}

/// Compute the next interval fire at or after `target_ms`.
fn next_interval_fire_at_or_after(start_ms: i64, every_ms: i64, target_ms: i64) -> i64 {
    if target_ms <= start_ms {
        return start_ms;
    }
    let elapsed = target_ms - start_ms;
    let periods = elapsed / every_ms;
    let candidate = start_ms + periods * every_ms;
    if candidate >= target_ms {
        candidate
    } else {
        candidate + every_ms
    }
}

/// Compute the latest interval fire at or before `target_ms`.
fn latest_interval_fire_at_or_before(start_ms: i64, every_ms: i64, target_ms: i64) -> i64 {
    if target_ms < start_ms {
        return start_ms; // shouldn't happen in normal flow
    }
    let elapsed = target_ms - start_ms;
    let periods = elapsed / every_ms;
    start_ms + periods * every_ms
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_next_interval_fire() {
        assert_eq!(next_interval_fire(100, 50, 100), 150);
        assert_eq!(next_interval_fire(100, 50, 149), 150);
        assert_eq!(next_interval_fire(100, 50, 150), 200);
        assert_eq!(next_interval_fire(100, 50, 50), 100);
    }

    #[test]
    fn test_next_interval_fire_at_or_after() {
        assert_eq!(next_interval_fire_at_or_after(100, 50, 100), 100);
        assert_eq!(next_interval_fire_at_or_after(100, 50, 101), 150);
        assert_eq!(next_interval_fire_at_or_after(100, 50, 150), 150);
        assert_eq!(next_interval_fire_at_or_after(100, 50, 151), 200);
    }

    #[test]
    fn test_latest_interval_fire_at_or_before() {
        assert_eq!(latest_interval_fire_at_or_before(100, 50, 100), 100);
        assert_eq!(latest_interval_fire_at_or_before(100, 50, 149), 100);
        assert_eq!(latest_interval_fire_at_or_before(100, 50, 150), 150);
        assert_eq!(latest_interval_fire_at_or_before(100, 50, 199), 150);
    }
}
