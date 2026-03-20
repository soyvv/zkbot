use zk_core_rs::{
    Clock, ClockError, SimClock, TestClock, TimerCoalescePolicy, TimerFire, TimerRequest,
    TimerSchedule,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn once_at(key: &str, fire_at_ms: i64) -> TimerRequest {
    TimerRequest {
        timer_key: key.to_string(),
        schedule: TimerSchedule::OnceAt { fire_at_ms },
        coalesce: TimerCoalescePolicy::FireAll,
    }
}

fn interval(
    key: &str,
    every_ms: i64,
    start_ms: Option<i64>,
    end_ms: Option<i64>,
    coalesce: TimerCoalescePolicy,
) -> TimerRequest {
    TimerRequest {
        timer_key: key.to_string(),
        schedule: TimerSchedule::Interval {
            every_ms,
            start_ms,
            end_ms,
        },
        coalesce,
    }
}

fn cron(
    key: &str,
    expr: &str,
    start_ms: Option<i64>,
    end_ms: Option<i64>,
    coalesce: TimerCoalescePolicy,
) -> TimerRequest {
    TimerRequest {
        timer_key: key.to_string(),
        schedule: TimerSchedule::Cron {
            expr: expr.to_string(),
            start_ms,
            end_ms,
        },
        coalesce,
    }
}

fn keys(fires: &[TimerFire]) -> Vec<&str> {
    fires.iter().map(|f| f.timer_key.as_str()).collect()
}

fn scheduled_times(fires: &[TimerFire]) -> Vec<i64> {
    fires.iter().map(|f| f.scheduled_ts_ms).collect()
}

// ---------------------------------------------------------------------------
// Test 1: one-shot timer fires exactly once
// ---------------------------------------------------------------------------

#[test]
fn test_oneshot_fires_exactly_once() {
    let mut clock = TestClock::new(0);
    clock.set_timer(once_at("t1", 100)).unwrap();

    let fires = clock.advance_to(100).unwrap();
    assert_eq!(fires.len(), 1);
    assert_eq!(fires[0].timer_key, "t1");
    assert_eq!(fires[0].scheduled_ts_ms, 100);

    // Advancing further should not fire again
    let fires = clock.advance_to(200).unwrap();
    assert!(fires.is_empty());
}

// ---------------------------------------------------------------------------
// Test 2: one-shot overdue lag reported correctly
// ---------------------------------------------------------------------------

#[test]
fn test_oneshot_overdue_lag() {
    let mut clock = TestClock::new(0);
    clock.set_timer(once_at("t1", 100)).unwrap();

    // Advance past the scheduled time
    let fires = clock.advance_to(150).unwrap();
    assert_eq!(fires.len(), 1);
    assert_eq!(fires[0].scheduled_ts_ms, 100);
    assert_eq!(fires[0].dispatch_ts_ms, 150);
    assert_eq!(fires[0].lag_ms, 50);
}

// ---------------------------------------------------------------------------
// Test 3: interval timer fires repeatedly at correct schedule points
// ---------------------------------------------------------------------------

#[test]
fn test_interval_fires_repeatedly() {
    let mut clock = TestClock::new(0);
    clock
        .set_timer(interval("iv", 100, Some(100), None, TimerCoalescePolicy::FireAll))
        .unwrap();

    let fires = clock.advance_to(100).unwrap();
    assert_eq!(scheduled_times(&fires), vec![100]);

    let fires = clock.advance_to(200).unwrap();
    assert_eq!(scheduled_times(&fires), vec![200]);

    let fires = clock.advance_to(300).unwrap();
    assert_eq!(scheduled_times(&fires), vec![300]);
}

// ---------------------------------------------------------------------------
// Test 4: interval timer does not drift when advance_to jumps ahead
// ---------------------------------------------------------------------------

#[test]
fn test_interval_no_drift_on_jump() {
    let mut clock = TestClock::new(0);
    clock
        .set_timer(interval("iv", 100, Some(100), None, TimerCoalescePolicy::FireAll))
        .unwrap();

    // Jump ahead by 350ms -- should fire at 100, 200, 300 (not 350)
    let fires = clock.advance_to(350).unwrap();
    assert_eq!(scheduled_times(&fires), vec![100, 200, 300]);

    // Next fire should be at 400, not 450
    let fires = clock.advance_to(400).unwrap();
    assert_eq!(scheduled_times(&fires), vec![400]);
}

// ---------------------------------------------------------------------------
// Test 5: cron timer computes next fire correctly
// ---------------------------------------------------------------------------

#[test]
fn test_cron_computes_next_fire() {
    let mut clock = TestClock::new(0);
    // Every second: "* * * * * *"
    // Start from timestamp 0 (1970-01-01 00:00:00 UTC)
    clock
        .set_timer(cron(
            "cron1",
            "* * * * * *",
            Some(0),
            None,
            TimerCoalescePolicy::FireOnce,
        ))
        .unwrap();

    // First fire should be at 1000ms (next second after epoch)
    let fires = clock.advance_to(1000).unwrap();
    assert_eq!(fires.len(), 1);
    assert_eq!(fires[0].timer_key, "cron1");
    assert_eq!(fires[0].scheduled_ts_ms, 1000);
}

// ---------------------------------------------------------------------------
// Test 6: cron timer respects end_ms
// ---------------------------------------------------------------------------

#[test]
fn test_cron_respects_end_ms() {
    let mut clock = TestClock::new(0);
    // Every second, but end at 3000ms
    clock
        .set_timer(cron(
            "cron1",
            "* * * * * *",
            Some(0),
            Some(3000),
            TimerCoalescePolicy::FireOnce,
        ))
        .unwrap();

    // Advance to 2000 -- should fire
    let fires = clock.advance_to(2000).unwrap();
    assert_eq!(fires.len(), 1);

    // Advance to 3000 -- should fire (at boundary)
    let fires = clock.advance_to(3000).unwrap();
    assert_eq!(fires.len(), 1);

    // Advance to 4000 -- should NOT fire (past end)
    let fires = clock.advance_to(4000).unwrap();
    assert!(fires.is_empty());
}

// ---------------------------------------------------------------------------
// Test 7: cancel_timer prevents future fires
// ---------------------------------------------------------------------------

#[test]
fn test_cancel_timer() {
    let mut clock = TestClock::new(0);
    clock
        .set_timer(interval("iv", 100, Some(100), None, TimerCoalescePolicy::FireAll))
        .unwrap();

    let fires = clock.advance_to(100).unwrap();
    assert_eq!(fires.len(), 1);

    assert!(clock.cancel_timer("iv"));
    // Should not cancel again
    assert!(!clock.cancel_timer("iv"));

    let fires = clock.advance_to(200).unwrap();
    assert!(fires.is_empty());
}

// ---------------------------------------------------------------------------
// Test 8: replacing an existing key overwrites the old schedule
// ---------------------------------------------------------------------------

#[test]
fn test_replace_existing_key() {
    let mut clock = TestClock::new(0);
    clock.set_timer(once_at("t1", 100)).unwrap();

    // Replace with a later time
    clock.set_timer(once_at("t1", 200)).unwrap();

    let fires = clock.advance_to(150).unwrap();
    assert!(fires.is_empty(), "old timer at 100 should be replaced");

    let fires = clock.advance_to(200).unwrap();
    assert_eq!(fires.len(), 1);
    assert_eq!(fires[0].scheduled_ts_ms, 200);
}

// ---------------------------------------------------------------------------
// Test 9: simultaneous timers fire in deterministic order by key
// ---------------------------------------------------------------------------

#[test]
fn test_deterministic_order_by_key() {
    let mut clock = TestClock::new(0);
    // All fire at the same time, but keys should sort alphabetically
    clock.set_timer(once_at("charlie", 100)).unwrap();
    clock.set_timer(once_at("alpha", 100)).unwrap();
    clock.set_timer(once_at("bravo", 100)).unwrap();

    let fires = clock.advance_to(100).unwrap();
    assert_eq!(keys(&fires), vec!["alpha", "bravo", "charlie"]);
}

// ---------------------------------------------------------------------------
// Test 10: FireAll returns all overdue occurrences
// ---------------------------------------------------------------------------

#[test]
fn test_fire_all_overdue() {
    let mut clock = TestClock::new(0);
    clock
        .set_timer(interval("iv", 100, Some(100), None, TimerCoalescePolicy::FireAll))
        .unwrap();

    // Jump ahead -- should get all missed fires
    let fires = clock.advance_to(500).unwrap();
    assert_eq!(scheduled_times(&fires), vec![100, 200, 300, 400, 500]);
}

// ---------------------------------------------------------------------------
// Test 11: FireOnce coalesces multiple missed occurrences into one fire
// ---------------------------------------------------------------------------

#[test]
fn test_fire_once_coalesces() {
    let mut clock = TestClock::new(0);
    clock
        .set_timer(interval(
            "iv",
            100,
            Some(100),
            None,
            TimerCoalescePolicy::FireOnce,
        ))
        .unwrap();

    // Jump ahead -- should get only one fire at the latest overdue time
    let fires = clock.advance_to(500).unwrap();
    assert_eq!(fires.len(), 1);
    assert_eq!(fires[0].scheduled_ts_ms, 500);
    assert_eq!(fires[0].lag_ms, 0);

    // Next fire should be at 600
    let fires = clock.advance_to(600).unwrap();
    assert_eq!(fires.len(), 1);
    assert_eq!(fires[0].scheduled_ts_ms, 600);
}

// ---------------------------------------------------------------------------
// Test 12: SkipMissed advances schedule without emitting old fires
// ---------------------------------------------------------------------------

#[test]
fn test_skip_missed() {
    let mut clock = TestClock::new(0);
    clock
        .set_timer(interval(
            "iv",
            100,
            Some(100),
            None,
            TimerCoalescePolicy::SkipMissed,
        ))
        .unwrap();

    // Jump ahead -- should emit nothing (all are overdue/missed)
    let fires = clock.advance_to(350).unwrap();
    assert!(fires.is_empty());

    // Next fire at 400 should work normally
    let fires = clock.advance_to(400).unwrap();
    assert_eq!(fires.len(), 1);
    assert_eq!(fires[0].scheduled_ts_ms, 400);
    assert_eq!(fires[0].lag_ms, 0);
}

// ---------------------------------------------------------------------------
// Test 13: multiple small steps vs one large jump yields consistent semantics
// ---------------------------------------------------------------------------

#[test]
fn test_small_steps_vs_large_jump() {
    // Large jump
    let mut clock1 = TestClock::new(0);
    clock1
        .set_timer(interval(
            "iv",
            100,
            Some(100),
            None,
            TimerCoalescePolicy::FireAll,
        ))
        .unwrap();
    let fires_jump = clock1.advance_to(300).unwrap();

    // Small steps
    let mut clock2 = TestClock::new(0);
    clock2
        .set_timer(interval(
            "iv",
            100,
            Some(100),
            None,
            TimerCoalescePolicy::FireAll,
        ))
        .unwrap();
    let mut fires_steps = Vec::new();
    for t in (100..=300).step_by(100) {
        fires_steps.extend(clock2.advance_to(t).unwrap());
    }

    // Both should produce the same scheduled_ts_ms sequence
    assert_eq!(scheduled_times(&fires_jump), scheduled_times(&fires_steps));
    assert_eq!(scheduled_times(&fires_jump), vec![100, 200, 300]);
}

// ---------------------------------------------------------------------------
// Test 14: advancing time backwards returns error
// ---------------------------------------------------------------------------

#[test]
fn test_advance_backward_error() {
    let mut clock = TestClock::new(100);
    let result = clock.advance_to(50);
    assert!(result.is_err());
    match result.unwrap_err() {
        ClockError::TimeMovedBackward {
            current_ms,
            requested_ms,
        } => {
            assert_eq!(current_ms, 100);
            assert_eq!(requested_ms, 50);
        }
        other => panic!("expected TimeMovedBackward, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Test 15: invalid cron expressions return errors
// ---------------------------------------------------------------------------

#[test]
fn test_invalid_cron_expression() {
    let mut clock = TestClock::new(0);
    let result = clock.set_timer(cron(
        "bad",
        "not a cron",
        None,
        None,
        TimerCoalescePolicy::FireAll,
    ));
    assert!(matches!(result, Err(ClockError::InvalidCronExpression(_))));
}

// ---------------------------------------------------------------------------
// Test 16: invalid interval configuration returns errors
// ---------------------------------------------------------------------------

#[test]
fn test_invalid_interval() {
    let mut clock = TestClock::new(0);

    let result = clock.set_timer(interval("bad", 0, None, None, TimerCoalescePolicy::FireAll));
    assert!(matches!(result, Err(ClockError::InvalidInterval(_))));

    let result = clock.set_timer(interval("bad", -100, None, None, TimerCoalescePolicy::FireAll));
    assert!(matches!(result, Err(ClockError::InvalidInterval(_))));
}

// ---------------------------------------------------------------------------
// Test 17: mixed schedule integration test
// ---------------------------------------------------------------------------

#[test]
fn test_mixed_schedule_integration() {
    let mut clock = TestClock::new(0);

    // One once-at timer at 150ms
    clock.set_timer(once_at("once", 150)).unwrap();

    // One interval timer every 100ms starting at 100ms
    clock
        .set_timer(interval(
            "interval",
            100,
            Some(100),
            Some(400),
            TimerCoalescePolicy::FireAll,
        ))
        .unwrap();

    // One cron timer: every second (fires at 1000, 2000, ...)
    clock
        .set_timer(cron(
            "cron",
            "* * * * * *",
            Some(0),
            None,
            TimerCoalescePolicy::FireOnce,
        ))
        .unwrap();

    // Step 1: advance to 100
    let fires = clock.advance_to(100).unwrap();
    assert_eq!(keys(&fires), vec!["interval"]);
    assert_eq!(scheduled_times(&fires), vec![100]);

    // Step 2: advance to 200 -- interval at 200, once-at at 150
    let fires = clock.advance_to(200).unwrap();
    assert_eq!(keys(&fires), vec!["once", "interval"]);
    assert_eq!(scheduled_times(&fires), vec![150, 200]);

    // Step 3: advance to 400 -- interval at 300, 400
    let fires = clock.advance_to(400).unwrap();
    assert_eq!(keys(&fires), vec!["interval", "interval"]);
    assert_eq!(scheduled_times(&fires), vec![300, 400]);

    // Step 4: advance to 1000 -- cron fires (interval ended at 400)
    let fires = clock.advance_to(1000).unwrap();
    assert_eq!(keys(&fires), vec!["cron"]);
    assert_eq!(fires[0].scheduled_ts_ms, 1000);

    // Step 5: advance to 2000 -- cron fires again
    let fires = clock.advance_to(2000).unwrap();
    assert_eq!(keys(&fires), vec!["cron"]);
    assert_eq!(fires[0].scheduled_ts_ms, 2000);
}

// ---------------------------------------------------------------------------
// Additional: SimClock basic test
// ---------------------------------------------------------------------------

#[test]
fn test_simclock_basic() {
    let mut clock = SimClock::new(0);
    assert_eq!(clock.now_ms(), 0);

    clock.set_timer(once_at("t1", 50)).unwrap();
    let fires = clock.advance_to(50).unwrap();
    assert_eq!(fires.len(), 1);
    assert_eq!(clock.now_ms(), 50);
}

// ---------------------------------------------------------------------------
// Additional: TestClock tick helper
// ---------------------------------------------------------------------------

#[test]
fn test_testclock_tick() {
    let mut clock = TestClock::new(0);
    clock.set_timer(once_at("t1", 100)).unwrap();

    let fires = clock.tick(50).unwrap();
    assert!(fires.is_empty());
    assert_eq!(clock.now_ms(), 50);

    let fires = clock.tick(50).unwrap();
    assert_eq!(fires.len(), 1);
    assert_eq!(clock.now_ms(), 100);
}

// ---------------------------------------------------------------------------
// Additional: TestClock peek_next_fire_ms
// ---------------------------------------------------------------------------

#[test]
fn test_testclock_peek() {
    let mut clock = TestClock::new(0);
    assert_eq!(clock.peek_next_fire_ms(), None);

    clock.set_timer(once_at("t1", 200)).unwrap();
    clock.set_timer(once_at("t2", 100)).unwrap();
    assert_eq!(clock.peek_next_fire_ms(), Some(100));

    clock.advance_to(100).unwrap();
    assert_eq!(clock.peek_next_fire_ms(), Some(200));
}

// ---------------------------------------------------------------------------
// Additional: advance_to same time is a no-op
// ---------------------------------------------------------------------------

#[test]
fn test_advance_to_same_time() {
    let mut clock = TestClock::new(100);
    let fires = clock.advance_to(100).unwrap();
    assert!(fires.is_empty());
    assert_eq!(clock.now_ms(), 100);
}

// ---------------------------------------------------------------------------
// Additional: interval with end_ms stops firing
// ---------------------------------------------------------------------------

#[test]
fn test_interval_end_ms() {
    let mut clock = TestClock::new(0);
    clock
        .set_timer(interval(
            "iv",
            100,
            Some(100),
            Some(200),
            TimerCoalescePolicy::FireAll,
        ))
        .unwrap();

    let fires = clock.advance_to(300).unwrap();
    // Should only fire at 100 and 200
    assert_eq!(scheduled_times(&fires), vec![100, 200]);

    let fires = clock.advance_to(400).unwrap();
    assert!(fires.is_empty());
}

// ---------------------------------------------------------------------------
// Additional: cancel already-fired one-shot returns false
// ---------------------------------------------------------------------------

#[test]
fn test_cancel_already_fired_oneshot() {
    let mut clock = TestClock::new(0);
    clock.set_timer(once_at("t1", 100)).unwrap();

    let fires = clock.advance_to(100).unwrap();
    assert_eq!(fires.len(), 1);

    // Timer was auto-removed after firing, cancel should return false
    assert!(!clock.cancel_timer("t1"));
}

// ---------------------------------------------------------------------------
// Additional: cron FireAll emits all missed occurrences
// ---------------------------------------------------------------------------

#[test]
fn test_cron_fire_all() {
    let mut clock = TestClock::new(0);
    // Every second cron
    clock
        .set_timer(cron(
            "cron1",
            "* * * * * *",
            Some(0),
            None,
            TimerCoalescePolicy::FireAll,
        ))
        .unwrap();

    // Jump to 3000ms -- should fire at 1000, 2000, 3000
    let fires = clock.advance_to(3000).unwrap();
    assert_eq!(fires.len(), 3);
    assert_eq!(scheduled_times(&fires), vec![1000, 2000, 3000]);
}

// ---------------------------------------------------------------------------
// Additional: cron SkipMissed skips overdue, fires on-time
// ---------------------------------------------------------------------------

#[test]
fn test_cron_skip_missed() {
    let mut clock = TestClock::new(0);
    // Every second cron with SkipMissed
    clock
        .set_timer(cron(
            "cron1",
            "* * * * * *",
            Some(0),
            None,
            TimerCoalescePolicy::SkipMissed,
        ))
        .unwrap();

    // Jump past several cron points -- should emit nothing (all overdue)
    let fires = clock.advance_to(2500).unwrap();
    assert!(fires.is_empty());

    // Advance to exact cron point -- should fire
    let fires = clock.advance_to(3000).unwrap();
    assert_eq!(fires.len(), 1);
    assert_eq!(fires[0].scheduled_ts_ms, 3000);
    assert_eq!(fires[0].lag_ms, 0);
}
