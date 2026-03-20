# Clock And Timer Design

This note defines the target `Clock` and timer model for ZKBot runtimes.

It applies to:

- `zk-engine-rs`
- `zk-backtest-rs`
- `zk-strategy-sdk-rs`
- future `zk-collocated-svc`

It is intended to unify live, backtest, and test time semantics without forcing all deployment
modes to share the same transport/runtime composition.

Related docs:

- [Architecture](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/arch.md)
- [Engine Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/engine_service.md)
- [Rust Crates](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/rust_crates.md)

## 1. Goals

- provide one explicit owner of runtime time semantics
- make timer behavior consistent across live engine, backtester, and tests
- separate logical event time from monotonic runtime time
- prevent timer drift from becoming an accidental implementation detail
- support deterministic backtests and stable timer ordering
- make future all-in-one mode comparable to a single-process trading runtime such as NautilusTrader

## 2. Non-Goals

- guaranteeing strict replay determinism in live markets
- exposing wall-clock implementation details directly to strategies
- forcing timers to fire at exact real-world instants under overload
- making timer scheduling depend on transport choice such as gRPC vs NATS

## 3. Design Principles

- time is a first-class runtime abstraction, not an incidental field in `StrategyContext`
- the runtime must distinguish logical time from monotonic waiting/sleep time
- timer scheduling rules must be identical across live, backtest, and tests
- timers are scheduled intents; under delay, behavior must follow explicit coalescing policy
- strategy-visible time must be stable and predictable within one callback boundary

## 4. Time Model

ZKBot should carry three related but distinct notions of time.

### 4.1 Logical time

Logical time is the timestamp visible to strategy callbacks.

Use cases:

- `ctx.now_ms()`
- timer scheduling relative to strategy-observed time
- event metadata
- deterministic replay

Logical time is advanced by the runtime according to the current mode:

- live mode: from event ingress timestamps and controlled clock advancement
- backtest mode: from replay event timestamps
- test mode: from explicit manual stepping

### 4.2 Monotonic time

Monotonic time is the local steady clock used for:

- sleeping
- internal deadlines
- measuring runtime latency
- wake-up scheduling

Monotonic time must not be exposed to strategies as business time.

### 4.3 Wall time

Wall time is used only where calendar interpretation is required:

- cron schedule expansion
- emitted metadata and logs
- operator-facing timestamps

Wall time should not be the direct source of truth for internal wait calculations.

## 5. Core Abstraction

The runtime should introduce a first-class `Clock` abstraction.

Recommended shape:

```rust
pub trait Clock: Send {
    fn now_ms(&self) -> i64;
    fn monotonic_ns(&self) -> u64;

    fn set_timer(&mut self, req: TimerRequest) -> Result<(), ClockError>;
    fn cancel_timer(&mut self, timer_key: &str) -> bool;

    fn advance_to(&mut self, now_ms: i64) -> Vec<TimerFire>;
}
```

Notes:

- `now_ms()` returns the current logical time
- `monotonic_ns()` is for runtime diagnostics and waiting logic only
- `advance_to()` moves logical time forward and returns all timer fires due at or before the target
- the clock owns timer scheduling state; timers are not a separate ad hoc subsystem

## 6. Timer API

Recommended request model:

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

Rules:

- `timer_key` is unique within one strategy execution
- setting an existing key replaces the existing schedule unless explicitly rejected by policy
- `OnceAt` fires at most once
- `Interval` should be defined from schedule cadence, not from actual delayed dispatch time
- `Cron` uses wall-clock calendar interpretation but emits logical timestamps

## 7. Required Implementations

ZKBot should provide three clock implementations.

### 7.1 `LiveClock`

Used by `zk-engine-rs` and future collocated runtime modes.

Responsibilities:

- maintain current logical time
- use monotonic time for wakeups
- expand and schedule timers
- return due timer fires when the engine advances time

### 7.2 `SimClock`

Used by `zk-backtest-rs`.

Responsibilities:

- advance only when replay time advances
- never sleep
- preserve deterministic timer ordering
- make timer behavior independent of host-machine timing

### 7.3 `TestClock`

Used by unit/integration tests.

Responsibilities:

- allow explicit manual stepping
- allow exact assertions on fire order and lag metadata
- support reproducible timing tests without sleeping

## 8. Strategy Runtime Integration

Strategies should not manipulate internal clock state directly.

Recommended runtime contract:

- strategy reads time through `StrategyContext`
- strategy requests timers through returned `SAction::SubscribeTimer`
- runtime applies timer subscriptions into the clock

Recommended `StrategyContext` surface:

```rust
impl StrategyContext {
    pub fn now_ms(&self) -> i64;
}
```

The runtime should remain responsible for:

- applying timer subscriptions
- advancing logical time
- invoking `on_timer`

This keeps strategy code deterministic and avoids exposing mutable runtime scheduling internals.

## 9. Engine Integration

The current implementation uses:

- `StrategyContext.current_ts_ms`
- `TimerManager`
- synthetic timer pulses in the live engine

The target design should evolve toward:

1. engine receives an external event or explicit clock tick
2. engine advances the clock to the selected logical time
3. clock returns due `TimerFire`s
4. engine dispatches timer callbacks through the same serial event loop
5. engine then dispatches external events according to the selected ordering rule

This keeps timer behavior inside the same single-writer event loop as strategy callbacks.

## 10. Backtester Integration

`zk-backtest-rs` should use `SimClock` as the sole source of logical time.

For each replay timestamp `T`:

1. advance `SimClock` to `T`
2. drain all due timers
3. dispatch those timer callbacks in deterministic order
4. process the replay event at `T`
5. process any resulting synthetic follow-up events according to backtester rules

This preserves deterministic replay semantics while keeping timer behavior identical in shape to the
live runtime.

## 11. Ordering Semantics

Timer ordering must be explicit and shared across modes.

Baseline ordering rule:

- timers are ordered by `scheduled_ts_ms`
- ties are broken by stable `timer_key`

Recommended per-timestamp processing order in replay/test mode:

1. timer fires due at `T`
2. internal synthetic follow-up events produced by the runtime for `T`
3. external market/account/order/signal events at `T`

Live mode note:

- exact same-timestamp ordering from external systems may still be arrival-dependent
- timer ordering should remain deterministic relative to other timers
- live mode should not promise more determinism than the environment can support

## 12. Drift And Overload Policy

Timers are scheduled intents, not a promise of exact dispatch instant.

Rules:

- represent each timer by its scheduled next due time
- when a timer is delayed, compute future schedule points from the schedule definition, not from the
  delayed dispatch time
- do not replay an unbounded backlog of missed firings by default

Recommended default coalescing:

- `OnceAt`: fire once if overdue
- `Interval`: default to `FireOnce` when severely delayed
- `Cron`: advance directly to the next valid schedule point after current time

Timer metadata should include:

- scheduled time
- actual dispatch time
- lag

The runtime should expose timer lag and dropped/coalesced counts through metrics and query APIs.

## 13. Pause And Resume Semantics

Timer behavior during pause must be policy-driven and explicit.

Recommended default:

- paused strategies do not receive timer callbacks
- timers continue to exist in the schedule model
- on resume, overdue periodic timers coalesce to at most one immediate callback per timer key
- no unbounded backlog replay

This matches the engine-service design direction and avoids surprise bursts on resume.

## 14. Suggested Crate Placement

Recommended layering:

- `zk-strategy-sdk-rs`
  - timer request/fire types
  - strategy-visible clock snapshot methods
- `zk-engine-rs`
  - `LiveClock`
  - runtime integration for live engine
- `zk-backtest-rs`
  - `SimClock`
  - replay-time advancement logic
- shared internal scheduler
  - existing `TimerManager` can evolve into the reusable scheduling core

`TimerManager` should become an implementation detail of the clock abstraction rather than the
primary public runtime concept.

## 15. Migration Plan From Current State

Current state:

- `StrategyContext` stores `current_ts_ms`
- `TimerManager` directly owns scheduling heap state
- `StrategyRunner::advance_time()` drains timers
- live engine emits synthetic timer pulses

Recommended migration:

1. add `Clock`, `TimerRequest`, `TimerFire`, and coalescing policy types
2. adapt `TimerManager` into a scheduler used internally by the clock
3. replace direct `current_ts_ms` ownership with clock-owned logical time
4. update `StrategyContext` to expose read-only time from the clock snapshot
5. update live engine and backtester to advance the clock explicitly
6. keep `SAction::SubscribeTimer` as the strategy-facing timer request path

This preserves the current strategy API shape while making time semantics first-class.

## 16. Open Questions

- whether interval timers need a distinct public schedule variant immediately, or can remain cron +
  one-shot first
- whether timer cancellation should be surfaced as a dedicated `SAction`
- whether the engine should process timers before or after external events at the same timestamp in
  live mode
- whether query APIs should expose the full timer schedule set or only summary lag/state
- whether OMS-side delayed actions and simulator delayed fills should reuse the same clock contract

## 17. Bottom Line

ZKBot should adopt a first-class clock abstraction with shared live/backtest/test semantics.

The design target is:

- one owner of logical time
- one timer scheduling contract
- explicit drift/coalescing semantics
- deterministic replay behavior
- live behavior that is semantically aligned even when the external world is not deterministic

This moves time from an implementation detail to a real runtime contract, which is necessary for a
credible all-in-one execution mode and for long-term parity between `zk-engine-rs` and
`zk-backtest-rs`.
