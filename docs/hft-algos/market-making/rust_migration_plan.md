# MM Strategy Rust Migration Plan

## Scope

This document defines the migration plan for the current Python market-making and hedging strategies
under:

- `zkstrategy_research/zk-strategylib/zk_strategylib/clob_mm/obmm`
- `zkstrategy_research/zk-strategylib/zk_strategylib/clob_mm/hedge`

The target is a Rust-native implementation that runs on the existing Rust strategy/runtime stack:

- `zk-strategy-sdk-rs`
- `zk-engine-rs`
- `zk-backtest-rs`

This plan does not require Python-vs-Rust parity tests. Instead, the Rust strategy should gain a
clean test suite from first principles, plus backtester-backed test utilities and scenario tests.

## Goals

- move the MM strategy and hedging strategy to Rust-native crates
- keep strategy logic separated from engine/runtime infrastructure
- replace Python dynamic class loading with typed Rust config and explicit factories
- make live and backtest execution use the same strategy code path
- build a durable test harness on top of `zk-backtest-rs`
- improve configurability, observability, and operational safety during the migration

## Non-Goals

- preserving Python dynamic loading semantics exactly
- maintaining a Python-compatible plugin interface inside the Rust strategy crates
- building exact event-by-event parity assertions against the current Python strategies

## Target Package Structure

Recommended new crates in `zkbot/rust/crates/`:

### `zk-mm-core-rs`

Pure strategy-domain logic with no live transport or service dependencies.

Modules:

- `config`
- `model`
- `pricer`
- `vol`
- `quote_reconciler`
- `inventory`
- `mm_state`
- `hedge_policy`
- `risk`

Responsibilities:

- pricer trait and concrete pricer implementations
- typed pricer config enum and factory
- sigma/volatility estimator implementations
- target ladder generation
- current-vs-target order reconciliation
- inventory and exposure calculations
- hedge decision policy and sizing
- pure state transition logic where practical

Design rules:

- no direct dependency on `zk-engine-rs`
- avoid direct dependency on protobuf-generated market data types in the pure math layer
- define small input/output structs owned by this crate and convert at the strategy boundary

### `zk-mm-strategy-rs`

Rust-native market-making strategy crate implementing `zk_strategy_sdk_rs::strategy::Strategy`.

Modules:

- `config`
- `mm_strategy`
- `hedge_strategy`
- `adapters`
- `factory`
- `init`
- `metrics`

Responsibilities:

- deserialize strategy config
- instantiate pricer and vol estimator via typed factories
- translate runtime events into `zk-mm-core-rs` inputs
- manage timers, strategy-local caches, and runtime lifecycle
- emit `SAction`s for order placement, cancels, logs, and timers
- optionally host both MM and hedger as separate concrete strategy types

### `zk-mm-test-utils-rs`

Optional helper crate if reusable scenario builders become large enough. Otherwise place these under
`zk-backtest-rs` test helpers or `zk-mm-strategy-rs/tests/common`.

Responsibilities:

- orderbook event builders
- balance and position snapshot builders
- scenario DSL for backtest-driven tests
- assertion helpers for quotes, cancels, and hedge actions

### `zk-mm-bot` or `zk-engine-svc` integration

The runnable bot should be a binary composition concern, not part of the strategy crate.

Responsibilities:

- load effective runtime config
- instantiate the selected strategy
- fetch warmup/init data
- construct dispatcher and market-data/event streams
- run `LiveEngine`

If `zk-engine-svc` becomes the canonical host, no dedicated bot crate is needed beyond strategy
registration/factory wiring.

## Config Migration Plan

## Current Python Pattern

The current Python strategy config can choose:

- pricer implementation by class path
- pricer config class by class path
- vol calculator implementation by class path

This is operationally flexible but shifts validation to runtime and relies on Python import
mechanics.

## Rust Target Pattern

Use typed tagged enums plus explicit factories.

Example shape:

- `StrategyConfig`
  - `maker: MakerConfig`
  - `hedger: Option<HedgerConfig>`
- `MakerConfig`
  - `pricer: PricerConfig`
  - `vol: VolConfig`
- `PricerConfig`
  - `AvellanedaStoikov { ... }`
  - `StaticSpread { ... }`
- `VolConfig`
  - `SimpleSigma { ... }`
  - future variants as needed

Operational benefits:

- invalid config fails at startup
- supported variants are explicit and grep-able
- config changes are type-checked and versionable
- no runtime code import path failures

Migration rule:

- Python `pricer_cls` becomes Rust `pricer.kind`
- Python `pricer_config_cls` is removed and implied by `pricer.kind`
- Python `pricer_config_data` becomes typed fields under the selected variant
- do the same for volatility calculator selection

## Strategy Composition Model

### Market Making Strategy

`MmStrategy` should own:

- immutable config
- pricer instance
- vol estimator instance
- current orderbook snapshot
- inventory snapshot cache
- target quote state
- outstanding order bookkeeping
- market-making state machine
- runtime diagnostics counters

Callbacks:

- `on_init`
  - load warmup data into the vol estimator
- `on_reinit`
  - register timers
  - initialize runtime state
- `on_tick`
  - update market snapshot
  - feed vol estimator
  - compute new target quotes when applicable
  - reconcile target vs current orders
- `on_order_update`
  - update strategy-local order state and error tracking
- `on_position_update`
  - refresh inventory-dependent logic
- `on_timer`
  - periodic vol calc
  - periodic state check
  - periodic hedge trigger if needed

### Hedging Strategy

Two valid end states:

1. separate `HedgeStrategy` implementing `Strategy`
2. hedge module embedded inside `MmStrategy`

Recommended phase order:

- implement hedging as a separate Rust strategy first
- only merge it into the maker strategy later if operations clearly require a single execution unit

Why:

- the current Python hedger is operationally distinct
- hedge retry logic and execution policy are easier to test in isolation
- deployment can remain flexible while the Rust path stabilizes

## Test Strategy

## Principles

- no Python parity harness required
- define expected Rust behavior directly from strategy rules
- cover pure math at unit-test level
- cover runtime behavior with `zk-backtest-rs`
- treat test utilities as first-class reusable infrastructure

## Test Layers

### 1. `zk-mm-core-rs` unit tests

Test:

- pricer math
- ladder slicing
- min-order-size and max-position handling
- vol estimator windowing and resampling behavior
- hedge sizing logic
- reconciliation logic
- state machine transitions

These tests should be deterministic and not depend on backtester/runtime components.

### 2. `zk-mm-strategy-rs` integration tests

Test the concrete `Strategy` implementation using direct event injection through the runner where
possible.

Test:

- timer registration and firing
- target quote refresh on tick flow
- order placement and cancel generation
- order rejection handling
- position update handling
- hedge trigger behavior under inventory changes

### 3. Backtester-backed scenario tests

Build a reusable scenario harness on top of `zk-backtest-rs`.

Recommended helper capabilities:

- start time / end time builder
- orderbook tick stream builder
- synthetic fills and no-fill policies
- balance and position initialization
- account and symbol mapping setup
- helpers to assert quote counts, side mix, prices, cancels, and hedge actions

Scenarios to cover:

- stable market with repeated requoting
- wide spread and thin spread conditions
- inventory drifting long and short
- max inventory boundary reached
- order rejection and retry handling
- cancel lag and stale quote cleanup
- hedge threshold not crossed
- hedge threshold crossed with capped order sizes
- partial fills requiring multiple hedge cycles
- restart/reinit with existing positions

### 4. Optional performance and soak tests

Use criterion benches or dedicated test binaries for:

- quote generation throughput
- reconciliation throughput with many live orders
- timer-heavy workloads
- long-running scenario stability

## Backtester Test Utility Plan

Enhance the backtest path to be strategy-friendly rather than strategy-specific.

Recommended additions:

- event-sequence builder helpers for orderbook updates and timestamps
- snapshot builders for balances and positions
- deterministic matching policies for:
  - no fills
  - touch fills
  - partial fills
  - delayed fills
- helper assertions over emitted `SAction`s or resulting OMS state

Design rule:

- keep utilities generic enough for other strategies
- place MM-specific scenario DSL close to `zk-mm-strategy-rs` if it would distort generic
  backtester helpers

## Migration Phases

### Phase 0: Preparation

- document the current Python strategy behaviors that will be intentionally preserved
- identify the minimum viable MM pricer set to support on day 1
- identify the minimum viable hedge modes to support on day 1
- confirm whether hedger will run as a separate engine execution initially

Deliverables:

- Rust config schema draft
- crate creation in workspace
- strategy responsibility split finalized

### Phase 1: Core Library Extraction

Implement `zk-mm-core-rs`.

Tasks:

- define market/quote/inventory domain structs
- implement pricer trait and current AS pricer
- implement volatility estimator equivalent to the current simple sigma calculator
- implement quote ladder builder
- implement quote reconciliation logic
- implement hedge policy and sizing logic
- implement unit tests

Exit criteria:

- `zk-mm-core-rs` compiles with strong unit test coverage
- no runtime crate dependencies leak into the core crate

### Phase 2: Market-Making Strategy Port

Implement `MmStrategy` in `zk-mm-strategy-rs`.

Tasks:

- define typed config structs
- build pricer and vol estimator factories
- implement strategy-local state
- map orderbook ticks to core inputs
- generate `SAction::PlaceOrder` and `SAction::Cancel`
- register periodic timers
- add integration tests around runner callbacks

Exit criteria:

- strategy compiles and runs in live engine skeleton
- strategy passes unit/integration/backtest scenario tests

### Phase 3: Hedging Strategy Port

Implement `HedgeStrategy` in `zk-mm-strategy-rs` or a separate crate if separation is still useful.

Tasks:

- port position aggregation rules
- port hedge thresholds and aggressiveness policy
- define order executor abstraction at the Rust strategy boundary
- implement retry, timeout, and pending-order tracking
- add backtest scenario coverage for fills, retries, and threshold crossing

Exit criteria:

- hedger supports at least one production hedge mode with deterministic tests
- runtime interaction points are explicit and observable

### Phase 4: Runtime Wiring

Wire strategy instantiation into the runnable engine path.

Tasks:

- expose strategy factory from `zk-mm-strategy-rs`
- integrate with `zk-engine-svc` or a temporary bot binary
- load warmup/init data before `startup()`
- pass effective config from control-plane/runtime config layer
- connect action dispatcher to OMS/trading client path

Exit criteria:

- the Rust strategy can run end-to-end in a development environment
- startup and shutdown behavior is documented

### Phase 5: Production Hardening

Tasks:

- add structured logs and counters around requotes, stale orders, rejects, and hedge actions
- add configuration validation with explicit startup errors
- add guardrails for invalid symbol/account combinations
- add performance measurement for hot quote paths
- document rollout and rollback procedure

Exit criteria:

- strategy is operationally supportable without Python dynamic loading
- the strategy exposes enough diagnostics for live debugging

## Improvements To Make During Migration

The migration is a good chance to improve the strategy rather than only translate it.

### 1. Separate pure logic from runtime objects

The Python implementation mixes quote logic and runtime orchestration. The Rust version should keep
math, reconciliation, and hedge policy in pure components.

### 2. Replace timer-specific callback names with typed timer intents

Rather than scattering string timer keys through the strategy, define constants or an enum-backed
mapping such as:

- `RequoteCheck`
- `SigmaRefresh`
- `PositionResync`
- `HedgeCheck`

This reduces stringly typed behavior.

### 3. Promote quote reconciliation to an explicit module

The current design implicitly combines target generation and order management. A separate
reconciliation module will make stale order handling and cancel/replace policy easier to reason
about and test.

### 4. Make market session and refdata hooks explicit

As the engine/runtime evolves, strategy-side refdata and market-status checks should become explicit
dependencies instead of ad hoc lookups.

### 5. Add strategy diagnostics as first-class state

Track and expose:

- current strategy mode
- latest sigma
- latest reservation price / spread inputs
- quote generation count
- cancel count
- reject count
- recoverable error count
- hedge pending quantity

This will materially improve live supportability.

### 6. Simplify hedger execution policy boundaries

The current Python hedger mixes hedge decision logic and execution orchestration. In Rust, separate:

- should hedge?
- how much to hedge?
- how aggressive should the hedge order be?
- how are retries and timeouts tracked?

That makes the behavior much easier to change safely.

### 7. Prefer config versioning over implicit semantic drift

If a pricer or hedge policy changes materially, add a new config variant instead of silently
changing an old one.

### 8. Build generic backtester helpers that other strategies can reuse

Do not keep all test scaffolding inside MM-only code if it naturally generalizes.

## Risks And Open Questions

- whether the current strategy needs multiple pricers on day 1 or only the current AS variant
- whether hedging should remain a separate runtime unit for operational isolation
- whether the existing strategy context needs more helper APIs for order lookup and symbol metadata
- whether warmup/init data should stay strategy-specific or move toward a generic dataset loader
- whether the current backtester needs new hooks to simulate hedge execution timing cleanly

## Recommended Immediate Next Steps

1. add `zk-mm-core-rs` and `zk-mm-strategy-rs` to the Rust workspace
2. define the top-level Rust config schema for maker and hedger
3. port the current AS pricer and simple sigma calculator into `zk-mm-core-rs`
4. build the first backtester-backed MM scenario harness
5. implement `MmStrategy` before starting the hedger port

## Related Docs

- [Rust Crate Architecture](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/rust_crates.md)
- [Engine Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/engine_service.md)
- [Bootstrap And Runtime Config](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/bootstrap_and_runtime_config.md)
- [API Contracts](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/api_contracts.md)
