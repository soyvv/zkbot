# Rust Migration Analysis and Plan for zkbot

## Goal

Migrate the performance-critical parts of `zkbot` from Python to Rust while keeping:

- existing infrastructure unchanged: `NATS`, `protobuf`, `Redis`
- existing Python strategies usable
- new Rust-native strategies supported
- Python retained as the orchestration layer for backtesting, analysis, and report generation
- research and production workflows aligned instead of forking into two incompatible systems

This document is based on the current code in:

- `libs/zk-core/src/zk_oms/`
- `libs/zk-core/src/zk_strategy/`
- `services/zk-service/src/tq_service_oms/`
- `services/zk-service/src/tq_service_strategy/`
- `protos/`
- `rust/crates/`

## Updated Position

The migration should explicitly support **both Option A and Option B** for Python strategies.

### Option A: Embedded Python via PyO3

Use this for:
- backtesting compatibility
- local/dev parity testing
- less performance-critical strategies
- transitional live deployment when isolation is not the top priority

### Option B: Python strategy worker

Use this for:
- performance-critical Python strategies that should not run inside the Rust engine process
- strategies that need isolation, independent scaling, or safer failure boundaries
- a stepping stone before full Rust porting

### Rust-native strategies

Use this for:
- latency-sensitive and throughput-sensitive strategies
- strategies that benefit from tighter integration with the Rust engine and OMS

So the intended policy is:

1. Low/medium criticality Python strategy: embedded mode is acceptable.
2. Higher criticality Python strategy: use Python worker mode.
3. Highest criticality strategy: port to Rust.

That is the right tradeoff. It avoids forcing everything through PyO3 while still preserving Python compatibility.

## Current State

### Python components currently on the hot path

1. OMS core
- `libs/zk-core/src/zk_oms/core/oms_core.py`
- Stateful order lifecycle, balance tracking, routing, risk checks, gateway report handling.

2. Live strategy engine
- `services/zk-service/src/tq_service_strategy/engine.py`
- Async event loop, NATS subscriptions, event coalescing, timer scheduling, action execution.

3. Strategy runtime
- `libs/zk-core/src/zk_strategy/strategy_core.py`
- Dynamically imports Python strategy files and dispatches callback-style events.

4. Backtester/simulator
- `libs/zk-core/src/zk_strategy/backtester.py`
- Event reordering, simulator integration, timer scheduling, strategy dispatch.

5. Service wrappers and persistence
- `services/zk-service/src/tq_service_oms/oms_server.py`
- `services/zk-service/src/tq_service_oms/redis_handler.py`
- NATS RPC handlers and Redis persistence.

### Important architectural facts

1. External contracts are already protobuf-based.
- This is the strongest migration advantage. It gives a stable cross-language boundary.

2. Python strategies are loaded dynamically from file paths.
- `load_strategy()` in `strategy_core.py` imports a Python file at runtime and finds a subclass of `StrategyBase`.

3. Strategy logic is callback-based and action-oriented.
- `TokkaQuant` accumulates actions like order, cancel, timer, signal, notification, and RPC request.
- This should become the language-neutral runtime contract.

4. Some OMS behavior currently depends on Python execution semantics.
- The existing OMS code mutates shared in-memory state in `OMSCore`, `OrderManager`, and `BalanceManager`.
- Python's GIL makes many of these patterns effectively serialized within a process.
- Rust will require explicit ownership, scheduling, and memory visibility guarantees.

5. Existing Rust support is only scaffolding.
- `rust/crates/zk-core-rs/src/lib.rs` is a placeholder.
- `rust/crates/zk-datamodel-rs/src/lib.rs` is also placeholder/scaffold level.

## What Should Move to Rust

### 1. OMS core

Why:
- high event volume
- correctness-sensitive state machine
- retry/reconciliation logic
- memory and concurrency benefit from Rust

Target:
- Rust becomes the source of truth for order state, balance state, and routing decisions.

### 2. Backtester event engine and simulator core

Why:
- large event streams
- deterministic replay matters
- event queue/reorder/scheduling is compute-heavy
- useful for parameter sweeps and future parallel simulation

Target:
- Rust owns event scheduling, order matching simulation, performance-critical bookkeeping, and deterministic replay.

### 3. Live strategy engine shell

Why:
- high fan-in of market/order/balance/timer events
- queueing, batching, coalescing, dispatch, and outbound IO are runtime-critical

Target:
- Rust owns the event loop and orchestration.
- Strategy execution can be Rust-native, Python-embedded, or delegated to Python workers.

### 4. Shared strategy state model

Why:
- orders, balances, pending actions, timers, and event snapshots are central shared runtime state
- this should not diverge across Python live engine, Rust live engine, and backtester

Target:
- Rust owns the canonical runtime state model.

## What Should Stay in Python

These should remain Python-first unless later profiling says otherwise.

1. Backtest orchestration shell.
2. Result analysis and report generation.
3. Research notebooks and exploratory workflows.
4. Monitoring/notification/reporting tools.
5. Non-hot-path exchange/refdata utilities.
6. Python strategy implementations.

## Backtester Direction

The backtester should move to a **hybrid model**.

### Rust owns
- event reordering core
- simulated OMS/matching core
- deterministic execution engine
- state bookkeeping and high-volume event processing

### Python owns
- experiment orchestration
- parameter sweeps
- strategy selection/configuration
- result post-processing
- HTML/chart/report generation
- interactive research workflows in `zkstrategy_research`

### Recommended interface

Expose the Rust backtester to Python as a binding layer, likely via `PyO3` + `maturin`.

Recommended Python-facing shape:
- `BacktestRunner`
- `DatasetAccessor`
- `BacktestResult`
- `ReportAdapter`

That preserves the current researcher workflow while removing the heavy execution path from Python.

## Strategy Compatibility Model

The strategy runtime should be explicitly pluggable.

### Supported runtimes

1. `RustStrategyRuntime`
- native Rust strategies

2. `PythonEmbeddedRuntime`
- Rust engine hosts CPython via PyO3
- for backtesting, parity tests, and less critical live strategies

3. `PythonWorkerRuntime`
- Python strategy runs in a separate process
- Rust engine sends events and receives actions
- for higher criticality Python strategies

### Runtime policy

- Backtesting: primarily `PythonEmbeddedRuntime` and `RustStrategyRuntime`
- Live, less critical strategy: `PythonEmbeddedRuntime` is allowed
- Live, higher criticality strategy: `PythonWorkerRuntime`
- Live, highest criticality strategy: `RustStrategyRuntime`

## Concurrency and Memory Model Risks in OMS

This needs explicit treatment up front.

### Current implicit behavior

In Python, some service behavior relies on:
- shared mutable objects living in one process
- mutation ordering created by the event loop and GIL
- direct readers observing state written by core worker code without explicit synchronization primitives

This is fragile but often works in Python because mutation is effectively serialized.

### Rust implication

In Rust, if you simply port the same shape into a truly concurrent runtime, you risk:
- readers observing partial state transitions
- lock contention or deadlocks
- non-deterministic visibility of updates
- hidden coupling between OMS core and auxiliary readers

### Recommendation: move to explicit ownership and snapshots

Do **not** replicate GIL-era shared mutable access patterns.

Use one of these patterns for OMS state ownership:

1. **Single-writer actor model** for core mutable state
- OMS core owns the authoritative mutable state.
- All state transitions happen on one task/thread/actor.
- Readers do not mutate core state.

2. **Snapshot publication for readers**
- Auxiliary readers get immutable snapshots or versioned views.
- Use `Arc<Snapshot>` or versioned copy-on-write structures.
- Readers consume published snapshots, not raw mutable core memory.

3. **Explicit command/query separation**
- writes go through the OMS actor
- reads go through a query layer backed by snapshots, read models, or separate materialized views

### Recommended choice

For `zkbot`, the safest model is:

- **single-writer OMS core actor**
- **versioned immutable snapshots for readers**
- optional Redis/materialized query state for remote readers

This preserves determinism and greatly reduces race conditions.

### Migration note

Any current worker/reader path that depends on direct in-memory object sharing should be treated as a required redesign item, not a detail to “port later”.

## Recommended Target Architecture

### Core principle

Separate the system into:

1. **Domain core**
- pure Rust
- deterministic
- no NATS/Redis/Python dependencies

2. **Infra adapters**
- NATS adapter
- Redis adapter
- protobuf translation
- Python adapter
- file/dataset adapter
- ArcticDB adapter

3. **Strategy runtimes**
- Rust strategy runtime
- Python embedded runtime
- Python worker runtime

### Proposed Rust crates

Under `zkbot/rust/crates/`:

1. `zk-proto-rs`
- generated protobuf types and helpers
- replaces the current placeholder `zk-datamodel-rs`

2. `zk-domain-rs`
- core enums, state models, action/event traits
- no network or database dependencies

3. `zk-oms-rs`
- OMS state machine
- order routing and validation
- balance and position updates
- reconciliation and recovery logic

4. `zk-engine-rs`
- live event loop
- event batching/coalescing
- timer manager
- strategy dispatch orchestration

5. `zk-backtest-rs`
- reorder queue
- simulator core
- matching policies
- deterministic replay

6. `zk-strategy-sdk-rs`
- Rust strategy trait and helper API
- equivalent to `StrategyBase` and `TokkaQuant`

7. `zk-strategy-py`
- PyO3 adapter crate
- loads Python strategy files
- marshals events in and actions out

8. `zk-strategy-worker-proto-rs`
- strategy worker protocol types/helpers if split from existing strategy proto

9. `zk-infra-rs`
- NATS adapter
- Redis adapter
- service bootstrap
- dataset readers

## Strategy API Direction

Do not preserve the current Python API only as an implementation detail. Preserve it as a contract.

### Language-neutral strategy contract

Inputs:
- tick events
- bar events
- signal events
- order updates
- position updates
- timer events
- strategy commands
- portfolio/account snapshots when needed

Outputs:
- place order
- cancel order
- amend/replace order
- subscribe timer
- publish signal
- log
- notify
- generic RPC request
- optional strategy state export/checkpoint

### Rust-native strategy interface

Use a trait-based model like:

```rust
trait Strategy {
    fn on_create(&mut self, ctx: &mut StrategyContext) -> Vec<Action>;
    fn on_tick(&mut self, event: TickData, ctx: &mut StrategyContext) -> Vec<Action>;
    fn on_bar(&mut self, event: Kline, ctx: &mut StrategyContext) -> Vec<Action>;
    fn on_signal(&mut self, event: RealtimeSignal, ctx: &mut StrategyContext) -> Vec<Action>;
    fn on_order_update(&mut self, event: OrderUpdateEvent, ctx: &mut StrategyContext) -> Vec<Action>;
    fn on_position_update(&mut self, event: PositionUpdateEvent, ctx: &mut StrategyContext) -> Vec<Action>;
    fn on_timer(&mut self, event: TimerEvent, ctx: &mut StrategyContext) -> Vec<Action>;
    fn on_cmd(&mut self, cmd: StrategyCommand, ctx: &mut StrategyContext) -> Vec<Action>;
}
```

### Python compatibility direction

Keep `StrategyBase` mostly stable, but make the engine backend replaceable.

A Python strategy should not need to know whether it is being run by:
- the old Python engine
- the Rust embedded runtime
- the Rust worker runtime

## Protobuf / Protocol Layer Analysis and Proposed Updates

The current protobuf layer is workable for the current system, but it is too narrow for a broader multi-strategy, multi-asset target.

### Current strengths

1. `common.proto` already includes several instrument types:
- `SPOT`, `PERP`, `FUTURE`, `CFD`, `OPTION`, `ETF`, `STOCK`

2. `strategy.proto` already has basic lifecycle and command concepts.

3. `oms.proto` has enough order/position primitives for current usage.

### Current limitations

1. `strategy.proto` is too execution-style specific.
- It assumes one rough class of strategy config and lifecycle.
- It does not distinguish runtime type or strategy category.

2. `oms.proto` order model is too narrow for some asset classes and strategy styles.
- no explicit basket order / portfolio rebalance concept
- no explicit order replace/amend concept in the main model
- limited algorithmic execution metadata
- insufficient support for option/volatility-specific fields

3. `common.proto` instrument metadata is not yet rich enough for all target assets.
- options need strike, option right, expiry semantics
- ETFs/stocks may need lot, venue, session/calendar, borrow/shortability metadata
- futures/perps/CFDs need margin, multiplier, settlement, funding/carry metadata

4. There is no formal strategy-runtime wire contract yet.
- embedded mode does not need one internally, but worker mode does
- backtester orchestration also benefits from a stable event/action schema

### Recommended compatible protobuf evolution

Keep current messages backward-compatible where possible and add new fields/messages rather than replacing aggressively.

### A. Add strategy runtime typing

In `strategy.proto`, add fields such as:
- `strategy_runtime_type`: `PY_EMBEDDED`, `PY_WORKER`, `RUST_NATIVE`
- `strategy_category`: `CTA`, `MARKET_MAKING`, `ARBITRAGE`, `PORTFOLIO_REBALANCE`, `VOL_TRADING`, `CUSTOM`
- `market_data_requirements`
- `execution_profile`
- `instrument_universe`

Purpose:
- scheduler, engine, and deployment tooling can reason about runtime and resource model explicitly

### B. Add a formal strategy worker protocol

Add new messages such as:
- `StrategyEventEnvelope`
- `StrategyActionEnvelope`
- `StrategySnapshotRequest`
- `StrategySnapshotResponse`
- `StrategyHeartbeat`
- `StrategyRuntimeError`

This protocol should support:
- Python worker runtime
- future remote Rust strategy workers if needed
- testing and replay harnesses

### C. Extend order intent model in `oms.proto`

Add compatible support for:
- amend/replace requests
- basket/multi-leg order intent
- execution instructions for TWAP/VWAP/POV/iceberg style execution
- routing hints / venue preferences
- strategy metadata and portfolio tags
- parent-child order linkage

This matters for:
- market making
- arbitrage
- ETF rebalance
- vol trading
- execution algos

### D. Extend instrument metadata in `common.proto`

Add fields for:
- option right (`CALL`/`PUT`)
- strike price
- multiplier / deliverable definition
- quote currency vs settlement currency distinction where needed
- borrowability / shortability / locate requirements
- trading sessions / calendar id / timezone
- venue-specific lot rules
- underlying instrument id
- portfolio grouping / sector / benchmark info for ETF/stock workflows

### E. Add canonical market data schema notes

For strategy portability, standardize how these are represented:
- L1 quote
- tick trade
- order book snapshot / incremental depth
- kline/bar at multiple resolutions
- synthetic spreads / derived instruments

### F. Add explicit versioning and deprecation policy

Before migration accelerates, define:
- protobuf version compatibility policy
- required/optional field policy
- how old Python services and new Rust services coexist during rollout

## Testing Strategy

A strong testing plan is required **before** large-scale porting starts.

### Testing goals

1. Behavioral parity between Python and Rust.
2. Deterministic replay for backtests and service event traces.
3. Cross-language serialization compatibility.
4. Concurrency correctness under Rust runtime.
5. Operational confidence for staged rollout.

### Test layers

#### 1. Golden protobuf compatibility tests

For each critical message type:
- generate Python payload
- parse in Rust
- serialize again
- compare semantics and golden fixtures

Cover at least:
- order requests
- order updates
- positions
- strategy events/actions
- refdata entries

#### 2. Domain parity tests

Build a corpus of Python OMS and backtester scenarios and replay them into Rust.

Compare:
- order state transitions
- fill accumulation
- balance updates
- position snapshots
- generated actions
- timer behavior

#### 3. Concurrency and state visibility tests

Specifically test the redesign areas that used to rely on GIL side effects.

Examples:
- one writer + many readers snapshot consistency
- no torn reads across order and balance state
- reader snapshot version monotonicity
- NATS input bursts and backpressure handling

#### 4. End-to-end replay tests

Capture real production-style event traces:
- market data
- order requests
- gateway reports
- balance updates

Replay them locally through:
- Python baseline
- Rust OMS
- Rust engine
- backtest core where applicable

#### 5. Strategy compatibility tests

Take representative strategies from different classes and ensure they run under:
- Python legacy runtime
- Rust embedded runtime
- Rust worker runtime if supported for that test case

#### 6. Performance and soak tests

Measure:
- event throughput
- tail latency
- memory growth
- snapshot publication cost
- worker round-trip overhead

## Local Test Data Plan

You asked for a plan to bring remote market data files local for easy testing. That should be formalized now.

### Objectives

1. Enable deterministic local testing without external service dependency.
2. Use the same data format for backtest, replay, and regression tests where practical.
3. Make it easy to subset and version test datasets.

### Proposed dataset tiers

1. **Golden tiny datasets**
- a few KB/MB
- checked into repo or fetched predictably
- used in CI

2. **Regression datasets**
- MB to low-GB
- stored locally or in a managed artifact bucket
- used for local/dev testing and scheduled regression jobs

3. **Benchmark datasets**
- larger production-like slices
- used for performance and soak testing

### Data store direction

Use **ArcticDB** as the primary research and local testing market data store.

Rationale:
- it already fits the existing Python research workflow better than introducing a second primary store
- local ArcticDB snapshots can be used to build deterministic replay and backtest fixtures
- it can remain the source of truth for research-side historical data while Rust consumes normalized extracts for high-performance replay

### Proposed canonical formats

Use a two-layer approach.

Primary store:
- **ArcticDB** for research datasets, local testing datasets, and developer workflows

Execution/replay interchange format:
- **Parquet** for exported deterministic replay slices, bars, trades, quotes, signals, and snapshots

Optional additions:
- Arrow IPC / Feather for faster interactive Python exchange if useful
- compressed NDJSON only for ad hoc logs or debug traces, not primary backtest data

### Proposed accessor layer

Create a language-neutral dataset accessor abstraction layered on top of ArcticDB and Parquet exports.

Concepts:
- `DatasetManifest`
- `InstrumentDatasetRef`
- `MarketDataAccessor`
- `BacktestSliceRequest`
- `ReplayEventSource`

Responsibilities:
- resolve local ArcticDB vs exported Parquet source
- read symbol/date partitions
- normalize schemas
- export deterministic local replay slices from ArcticDB
- expose iterator/stream interface to Rust core
- expose DataFrame-friendly interface to Python orchestration layer

### Suggested partitioning

For Parquet datasets, partition by:
- dataset type (`ticks`, `quotes`, `trades`, `bars_1m`, `bars_5m`, `signals`)
- venue/exchange
- asset class
- symbol or instrument id
- date

### Suggested schema notes

For tick/trade/quote files, include:
- event timestamp
- receive timestamp if available
- instrument id
- exchange / venue
- source sequence / event id if available
- side / price / qty as needed
- raw source metadata fields in an extensible map or companion columns

### Suggested local tooling

Add scripts to:
- sync or snapshot remote ArcticDB data to local ArcticDB for testing
- export symbol/date slices from ArcticDB to deterministic Parquet replay fixtures
- validate schema
- build small deterministic test fixtures from larger raw data
- register manifests for replay tests

### Data preparation step

Add an explicit data preparation stage before parity and replay testing.

Inputs:
- selected local ArcticDB libraries and symbols
- date ranges for golden, regression, and benchmark scenarios

Outputs:
- normalized local replay datasets
- deterministic Parquet fixtures for CI and regression
- manifests describing dataset provenance, schema version, symbol universe, and date coverage

Recommended flow:
1. Select representative instruments and strategy scenarios from ArcticDB.
2. Materialize local slices from ArcticDB into a normalized schema.
3. Export deterministic Parquet fixtures for Rust replay and Python orchestration.
4. Register fixtures in a dataset manifest used by parity and performance tests.
5. Keep a small golden fixture set under version control or reproducibly generated, with larger sets stored outside the repo.

## Migration Sequencing

### Phase 0: Contract and test stabilization

Deliverables:
- freeze protobuf compatibility rules
- define canonical event/action schema for strategy runtime
- document Redis keyspace and state ownership
- define testing pyramid and required parity datasets
- define local ArcticDB snapshot/cache, export pipeline, and dataset accessor design

Tasks:
- audit current `betterproto` Python messages against intended Rust `prost` generation
- remove ambiguity around naming drift (`tq_*` vs `zk_*`)
- document all shared-memory or GIL-dependent OMS assumptions
- define required representative strategy set for compatibility tests
- define golden/local/regression dataset policy

Exit criteria:
- Python and Rust can serialize/deserialize the same protobuf payloads reliably
- migration-critical behaviors depending on GIL/shared memory are documented
- initial replay datasets are available locally

### Phase 1: Build real Rust foundations

Deliverables:
- replace placeholder Rust crates with real crate layout
- protobuf generation for Rust integrated into repo build
- core domain models and event/action traits in Rust
- dataset accessor skeleton in Rust and Python

Tasks:
- introduce Cargo workspace structure for domain/infra separation
- add codegen pipeline from `protos/`
- add golden protobuf tests
- define snapshot model for OMS reader access

Exit criteria:
- Rust workspace builds cleanly and can parse/publish current message types
- a local replay dataset prepared from ArcticDB can be read from both Python and Rust paths

### Phase 2: Port OMS to Rust first

Deliverables:
- Rust OMS core with parity tests against Python OMS behavior
- Rust OMS service speaking the same NATS RPC subjects
- Redis persistence compatible with existing consumers or versioned for dual-read
- explicit single-writer + snapshot reader model

Tasks:
- port order state machine, routing, balance handling, reconciliation
- mirror existing test coverage from `libs/zk-core/src/zk_oms/tests/`
- replay captured event traces against Python and Rust OMS
- replace implicit shared-memory assumptions with snapshot/query interfaces

Why first:
- highest business value
- most bounded state machine
- independent of final strategy runtime choice

Exit criteria:
- Rust OMS passes parity suite and can run in shadow/canary mode

### Phase 3: Port backtester core to Rust, keep Python orchestration

Deliverables:
- Rust reorder queue, timer handling, simulator, and event dispatch core
- Python bindings for orchestration, analysis, and report generation
- local ArcticDB-backed dataset accessor, export pipeline, and replay harness

Tasks:
- port `ReorderQueue`, event ordering logic, simulator core, matching policies
- keep Python strategy callback compatibility through embedded adapter first
- expose Rust backtest core to Python via bindings
- integrate with existing reporting flows in `zkstrategy_research`

Exit criteria:
- researchers can run existing Python strategies on Rust backtest core from Python
- report generation remains Python-native

### Phase 4: Introduce pluggable Rust live strategy engine

Deliverables:
- Rust live event engine
- Rust strategy SDK
- Python embedded runtime

Tasks:
- port event subscription, queueing, timer scheduling, action execution
- support embedded Python strategy execution through PyO3 adapter crate
- support Rust-native strategies through trait API

Exit criteria:
- one Rust engine can run both a Rust strategy and a Python-embedded strategy

### Phase 5: Add Python worker runtime

Deliverables:
- out-of-process Python strategy runtime
- formal strategy worker protocol over protobuf/NATS or narrower IPC
- worker lifecycle, heartbeat, restart, and timeout policy

Tasks:
- define worker event/action protocol
- add supervisor and runtime registration
- add backpressure, timeout, retry, and fault isolation behavior

Exit criteria:
- higher-criticality Python strategies can run out-of-process with acceptable latency and isolation

### Phase 6: Protocol expansion for broader strategy and asset support

Deliverables:
- compatible protobuf evolution plan implemented incrementally
- richer strategy category and runtime descriptors
- richer instrument metadata and execution intent model

Tasks:
- add fields/messages required for CTA, MM, arb, rebalance, vol trading
- extend instrument metadata for ETF/stock/crypto spot/perp/CFD and options/futures expansion
- add parent-child and basket order support where needed

Exit criteria:
- new runtime can support broader strategy types without ad hoc side channels

### Phase 7: Production hardening and cutover

Deliverables:
- canary rollout plan
- observability, replay, recovery, and rollback procedures
- SLOs for OMS and strategy engine

Tasks:
- shadow traffic
- dual-run or compare mode where needed
- perf and soak tests
- failure injection on NATS disconnects, Redis stalls, malformed reports, duplicate messages, worker hangs

Exit criteria:
- Rust services can replace Python services without losing operational capabilities

## Other Architectural Changes Worth Capturing Now

These are not strictly “the Rust port”, but they should be noted because migration will expose them.

### 1. Separate command, state, and analytics concerns more clearly

Current code mixes:
- hot-path state mutation
- query/read concerns
- logging/recording concerns

Recommendation:
- formal command path
- formal query/read model
- formal analytics/export path

### 2. Unify naming and packaging

There is ongoing `tq_*` and `zk_*` drift.

Recommendation:
- freeze a naming policy before Rust crates and new protocols expand the surface area further.

### 3. Introduce explicit runtime/config descriptors

A strategy should declare:
- runtime type
- strategy category
- market data requirements
- instrument universe type
- latency sensitivity
- state persistence needs

This helps scheduling, deployment, and testing.

### 4. Build replayability into services by design

Every critical runtime should support:
- event capture
- deterministic local replay
- parity comparison mode

This is not optional for a cross-language migration.

### 5. Consider a dedicated read model for OMS queries

If query traffic grows, consider:
- a materialized read model
- or Redis-backed projection generated from OMS snapshots/events

This avoids coupling fast reads to the OMS mutation loop.

## Recommended First 90 Days

### Month 1

1. Freeze protocol compatibility rules and testing plan.
2. Document all GIL/shared-memory assumptions in OMS and engine.
3. Stand up real Rust workspace and protobuf generation.
4. Define ArcticDB-based dataset manifest, local snapshot/cache, and export/accessor plan.

### Month 2

1. Port OMS core state machine.
2. Build Rust OMS shadow service on existing NATS subjects.
3. Add snapshot/query model for readers.
4. Start replaying captured OMS traces locally.

### Month 3

1. Port backtest queue/simulator core.
2. Expose Rust backtester to Python.
3. Prove one existing Python strategy can run unchanged on Rust backtest core.
4. Define Python worker runtime protocol and minimal prototype.

## Concrete Recommendation

If the goal is maximum payoff with manageable risk, the order should be:

1. **Freeze contracts, datasets, and test strategy first**
2. **Document and redesign GIL-dependent shared-state assumptions**
3. **OMS to Rust first**
4. **Backtester core to Rust second, with Python still orchestrating/reporting**
5. **Live strategy engine orchestration to Rust third**
6. **Python compatibility via both embedded PyO3 and worker runtime**
7. **Rust-native strategy SDK in parallel once the event/action contract is stable**
8. **Compatible protobuf expansion for broader strategy/instrument support**

## Final Position

Yes, Rust is a good fit for the performance-critical parts of `zkbot`.

Yes, `PyO3` should be part of the plan.

Yes, a Python worker mode should also be part of the plan.

But they should serve different roles:

- `PyO3`: compatibility bridge, backtesting bridge, and support for less critical strategies
- Python worker: isolation path for performance-critical Python strategies that are not yet ported
- Rust-native strategy SDK: end state for highest criticality strategies

The right end state is:

- Rust owns performance-critical domain logic and service orchestration
- protobuf remains the stable wire contract
- Python remains the main orchestration/reporting environment for research and backtesting
- Python strategies continue to work through embedded and worker runtime adapters
- Rust strategies become first-class through a native SDK
- GIL-era implicit shared-memory assumptions are replaced by explicit state ownership and snapshot/query patterns
- research and production converge on the same Rust core instead of splitting permanently
