# Agent Prompt: Implement `Clock` Abstraction In `zk-core-rs`

Use this prompt with a coding agent.

---

Implement the first version of the ZKBot clock/timer abstraction as a new shared Rust crate named `zk-core-rs`.

Scope and constraints:

1. Create a new crate at `zkbot/rust/crates/zk-core-rs`.
2. Put the clock/timer abstraction in this crate because it is intended to be shared infrastructure.
3. Do not change any other package yet.
4. Do not wire the new crate into `zk-engine-rs`, `zk-backtest-rs`, `zk-strategy-sdk-rs`, or any service crate yet.
5. Focus on clean API design, internal correctness, and strong tests.
6. Add ample unit tests and integration-style tests inside `zk-core-rs`.
7. Use ASCII only unless the target file already uses non-ASCII.

Primary reference doc:

- `zkbot/docs/system-arch/clock_and_timer.md`

Secondary context docs:

- `zkbot/docs/system-arch/services/engine_service.md`
- `zkbot/rust/crates/zk-strategy-sdk-rs/src/timer_manager.rs`
- `zkbot/rust/crates/zk-strategy-sdk-rs/src/runner.rs`
- `zkbot/rust/crates/zk-backtest-rs/src/backtester.rs`
- `zkbot/rust/crates/zk-engine-rs/src/live_engine.rs`

## Goal

Design and implement a reusable clock/timer module that can later be consumed by live engine,
backtester, and tests.

This first step is only the shared crate and its tests.

## Required deliverables

Create `zk-core-rs` with a public API roughly centered on:

- `Clock` trait
- timer request/schedule types
- timer fire/result types
- `SimClock`
- `TestClock`
- any shared scheduler needed internally

You may also add:

- `ClockError`
- `TimerCoalescePolicy`
- helper structs/enums for deterministic ordering

For this first pass, `LiveClock` is optional.

Reason:

- `SimClock` and `TestClock` are enough to validate the abstraction
- `LiveClock` is more useful once a downstream crate adopts the interface

## API guidance

Use the design doc, but prefer a pragmatic minimal API over speculative completeness.

Recommended shape:

```rust
pub trait Clock {
    fn now_ms(&self) -> i64;
    fn set_timer(&mut self, req: TimerRequest) -> Result<(), ClockError>;
    fn cancel_timer(&mut self, timer_key: &str) -> bool;
    fn advance_to(&mut self, now_ms: i64) -> Vec<TimerFire>;
}
```

Suggested public types:

```rust
pub struct TimerRequest {
    pub timer_key: String,
    pub schedule: TimerSchedule,
    pub coalesce: TimerCoalescePolicy,
}

pub enum TimerSchedule {
    OnceAt { fire_at_ms: i64 },
    Interval { every_ms: i64, start_ms: Option<i64>, end_ms: Option<i64> },
    Cron { expr: String, start_ms: Option<i64>, end_ms: Option<i64> },
}

pub enum TimerCoalescePolicy {
    FireAll,
    FireOnce,
    SkipMissed,
}

pub struct TimerFire {
    pub timer_key: String,
    pub scheduled_ts_ms: i64,
    pub dispatch_ts_ms: i64,
    pub lag_ms: i64,
}
```

You may adjust names or signatures if there is a clearly better Rust API, but do not drift away from
the architecture doc without reason.

## Behavioral requirements

Implement at least:

- one-shot timers
- interval timers
- cron timers
- timer cancellation
- replacement behavior when the same `timer_key` is re-used
- deterministic ordering of simultaneous timer fires
- lag metadata (`dispatch_ts_ms - scheduled_ts_ms`)
- coalescing behavior for overdue recurring timers

Required ordering rule:

- order fires by `scheduled_ts_ms`
- break ties by stable `timer_key`

Required advancement rule:

- `advance_to(t)` must never move time backwards
- if asked to advance to an earlier time, return an error or no-op consistently
- pick one behavior and test it clearly

Required scheduling rule:

- recurring timers must compute their next scheduled time from schedule cadence, not from delayed
  dispatch time
- avoid cumulative drift

## Implementation guidance

- Keep the internals small and explicit.
- Prefer a heap-based scheduler or another deterministic structure.
- Avoid premature async abstractions; this crate should be synchronous and mode-agnostic.
- `SimClock` and `TestClock` can share most implementation.
- If useful, structure the crate into:
  - `clock.rs`
  - `types.rs`
  - `scheduler.rs`
  - `sim_clock.rs`
  - `test_clock.rs`
  - `lib.rs`

## Testing requirements

Add substantial tests. Do not stop at a few happy-path cases.

At minimum, cover:

1. one-shot timer fires exactly once
2. one-shot timer overdue lag is reported correctly
3. interval timer fires repeatedly at correct schedule points
4. interval timer does not drift when `advance_to` jumps ahead
5. cron timer computes next fire correctly
6. cron timer respects `end_ms`
7. `cancel_timer` prevents future fires
8. replacing an existing key overwrites the old schedule correctly
9. simultaneous timers fire in deterministic order by key
10. `FireAll` returns all overdue occurrences
11. `FireOnce` coalesces multiple missed occurrences into one fire
12. `SkipMissed` advances schedule without emitting old missed occurrences
13. advancing time in multiple steps vs one large jump yields consistent schedule semantics
14. advancing time backwards is handled exactly as designed
15. invalid cron expressions return errors
16. invalid interval configuration returns errors

Also add at least one higher-level test that simulates a realistic mixed schedule:

- one once-at timer
- one interval timer
- one cron timer
- multiple `advance_to` calls
- assertions on exact emitted sequence

## Package requirements

- add `Cargo.toml` for `zk-core-rs`
- add the crate to the Rust workspace if needed for tests/build
- keep dependencies minimal
- use `chrono` / `cron` only if actually needed

## Out of scope

Do not:

- modify `zk-engine-rs`
- modify `zk-backtest-rs`
- modify `zk-strategy-sdk-rs`
- add runtime integration code
- change strategy APIs
- add NATS/gRPC/runtime-service behavior

## Verification

Run the crate tests and report the result.

Preferred commands:

```bash
cargo test -p zk-core-rs
```

If workspace wiring requires it, also run:

```bash
cargo test --package zk-core-rs
```

## Expected output from the agent

1. concise summary of the crate structure
2. concise summary of the API decisions
3. test results
4. any open design choices intentionally deferred to downstream integration

---

Implementation priority:

1. correctness
2. deterministic behavior
3. API clarity
4. test coverage
5. future integrability
