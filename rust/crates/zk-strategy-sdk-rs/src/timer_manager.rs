use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::str::FromStr;

use chrono::{DateTime, TimeZone, Utc};
use cron::Schedule;

use crate::models::TimerEvent;

// ---------------------------------------------------------------------------
// Internal heap entry — min-heap ordered by (fire_ms, key)
// ---------------------------------------------------------------------------

#[derive(Eq, PartialEq, Ord, PartialOrd, Clone)]
struct HeapEntry {
    fire_ms: i64,
    key: String,
    kind: TimerKind,
}

#[derive(Eq, PartialEq, Ord, PartialOrd, Clone)]
enum TimerKind {
    Clock,
    Cron,
}

struct CronEntry {
    schedule: Schedule,
    end_ms: Option<i64>,
}

// ---------------------------------------------------------------------------
// TimerManager — mirrors Python TimerManager from zk_strategy/timer.py
// ---------------------------------------------------------------------------

pub struct TimerManager {
    crons: HashMap<String, CronEntry>,
    /// min-heap: smallest fire_ms pops first
    heap: BinaryHeap<Reverse<HeapEntry>>,
}

impl TimerManager {
    pub fn new() -> Self {
        Self {
            crons: HashMap::new(),
            heap: BinaryHeap::new(),
        }
    }

    /// Subscribe a recurring cron timer.
    ///
    /// `expr` must be a 6-field cron expression `<sec> <min> <hour> <day> <month> <weekday>`.
    /// `start_ms` is the reference time for computing the first occurrence.
    /// `end_ms` is optional; no events are fired after this timestamp.
    pub fn subscribe_cron(&mut self, key: &str, expr: &str, start_ms: i64, end_ms: Option<i64>) {
        let schedule = Schedule::from_str(expr)
            .unwrap_or_else(|e| panic!("invalid cron expression '{expr}': {e}"));
        let start_dt: DateTime<Utc> = Utc.timestamp_millis_opt(start_ms).unwrap();
        let first = match schedule.after(&start_dt).next() {
            Some(dt) => dt.timestamp_millis(),
            None => return,
        };
        if end_ms.map_or(false, |e| first > e) {
            return;
        }
        self.crons
            .insert(key.to_string(), CronEntry { schedule, end_ms });
        self.heap.push(Reverse(HeapEntry {
            fire_ms: first,
            key: key.to_string(),
            kind: TimerKind::Cron,
        }));
    }

    /// Subscribe a one-shot timer that fires at exactly `fire_ts_ms`.
    pub fn subscribe_once_at(&mut self, key: &str, fire_ts_ms: i64) {
        self.heap.push(Reverse(HeapEntry {
            fire_ms: fire_ts_ms,
            key: key.to_string(),
            kind: TimerKind::Clock,
        }));
    }

    /// Return all `TimerEvent`s whose scheduled time is ≤ `now_ms`.
    /// Cron timers are automatically rescheduled to their next occurrence.
    pub fn drain_due(&mut self, now_ms: i64) -> Vec<TimerEvent> {
        let mut events = Vec::new();
        while let Some(Reverse(entry)) = self.heap.peek() {
            if entry.fire_ms > now_ms {
                break;
            }
            let Reverse(entry) = self.heap.pop().unwrap();
            events.push(TimerEvent {
                timer_key: entry.key.clone(),
                ts_ms: entry.fire_ms,
            });

            if entry.kind == TimerKind::Cron {
                if let Some(cron_entry) = self.crons.get(&entry.key) {
                    let fire_dt: DateTime<Utc> = Utc.timestamp_millis_opt(entry.fire_ms).unwrap();
                    if let Some(next_dt) = cron_entry.schedule.after(&fire_dt).next() {
                        let next_ms = next_dt.timestamp_millis();
                        let still_active = cron_entry.end_ms.map_or(true, |e| next_ms <= e);
                        if still_active {
                            self.heap.push(Reverse(HeapEntry {
                                fire_ms: next_ms,
                                key: entry.key,
                                kind: TimerKind::Cron,
                            }));
                        } else {
                            self.crons.remove(&entry.key);
                        }
                    } else {
                        self.crons.remove(&entry.key);
                    }
                }
            }
        }
        events
    }

    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }
}

impl Default for TimerManager {
    fn default() -> Self {
        Self::new()
    }
}
