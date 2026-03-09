# Workstreams

## WS1: Contracts And Compatibility

### Goal
Define stable cross-language boundaries before porting core logic.

### Tasks
- [ ] audit current Python protobuf usage against intended Rust codegen
- [ ] define backward-compatibility rules for proto evolution
- [ ] identify message types that need golden fixtures
- [ ] define strategy runtime event/action contract
- [ ] define worker runtime wire protocol

### Outputs
- protobuf compatibility rules
- strategy runtime contract
- worker protocol draft

### Exit criteria
- Python and Rust can parse and round-trip critical messages consistently

## WS2: OMS State Ownership Redesign

### Goal
Replace implicit GIL-based behavior with explicit Rust-safe ownership.

### Tasks
- [ ] enumerate current shared-memory read/write paths in OMS and related services
- [ ] define single-writer actor boundary for OMS core
- [ ] define immutable snapshot model for readers
- [ ] define query/read model and Redis/materialized-view role
- [ ] identify cut points where legacy Python readers must be adapted

### Outputs
- OMS ownership model
- snapshot/query design
- migration notes for reader paths

### Exit criteria
- no required OMS read path depends on direct mutable shared-memory access

## WS3: Rust Workspace Foundation ✓ COMPLETE

### Goal
Stand up a real Rust workspace with crate boundaries aligned to the target architecture.

### Tasks
- [x] replace placeholder crate setup with real crate layout (7 crates: `zk-proto-rs`, `zk-domain-rs`, `zk-oms-rs`, `zk-engine-rs`, `zk-backtest-rs`, `zk-strategy-sdk-rs`, `zk-infra-rs`)
- [x] wire Rust protobuf generation from `protos/` (`buf.gen.yaml` output path updated to `zk-proto-rs`)
- [x] add build/test commands for Rust workspace (`cargo build`, `cargo test --package zk-oms-rs`)
- [ ] add CI hooks for Rust formatting/lint/test — pending

### Outputs
- working Cargo workspace ✓
- generated protobuf crate (`zk-proto-rs`) ✓
- baseline CI support — pending

### Exit criteria
- Rust workspace builds and generated protobuf types are available ✓

## WS4: Testing And Replay Infrastructure

### Goal
Build the parity and regression harness before large-scale porting.

### Tasks
- [ ] define golden protobuf fixture corpus
- [ ] define domain parity fixture corpus
- [ ] capture representative replay traces
- [ ] define strategy compatibility fixture set
- [ ] define benchmark and soak workloads

### Outputs
- parity harness design
- replay fixture inventory
- benchmark plan

### Exit criteria
- at least one end-to-end replay path exists for both Python baseline and Rust candidate implementation

## WS5: Data Preparation And Access Layer

### Goal
Use ArcticDB as the source store and prepare deterministic local fixtures for replay and tests.

### Tasks
- [ ] define import procedure from `~/Downloads/全套数据TXT` into local ArcticDB test libraries
- [ ] define ArcticDB library/symbol/date selection policy
- [ ] define normalized replay schema
- [ ] define dataset manifest format
- [ ] implement ArcticDB-to-Parquet export flow
- [ ] define local cache/snapshot workflow
- [x] first local fixture: `zkstrategy_research/fixtures/USDJPY_2021Q1_1m.parquet` (USDJPY 1m, 2021-Q1, from `~/Downloads/全套数据TXT`; produced by `tools/prepare_local_fixture.py`)
- [x] `LocalParquetKlineDataSource` — drop-in `KlineDataSource` backed by local Parquet (`zk-data/zk_tsdata/LocalKlineDataSource.py`)
- [x] `ETS_kline_bt_local.py` — ETS backtest using local fixture; no remote deps

### Outputs
- dataset manifest
- local data prep procedure
- normalized replay fixture format
- reference local-data backtest workload definition

### Exit criteria
- local ArcticDB slices can be exported to deterministic fixtures consumable by Python and Rust
- at least one reference ETS minute-kline backtest can run against locally prepared data without depending on remote services

## WS6: OMS Port — CORE DONE, service/shadow pending

### Goal
Port OMS core and service behavior to Rust first.

### Tasks
- [x] port order state transitions (`order_mgr.rs`: linkage, state, trade, exec, fee reports; inferred trades; LRU eviction)
- [x] port routing and validation (`oms_core.rs`: process_message dispatch, panic mode, risk check, context resolution)
- [x] port balance and position updates (`balance_mgr.rs`: OMS-managed + exchange-reported dual tracking, sign-flip, merge)
- [x] parity test suite (`tests/oms_parity.rs`: 12 tests, all green — placement, rejection, linkage, partial fill, cancel, inferred trade, balance update)
- [ ] port reconciliation/recovery logic — `calc_balance_changes_for_report` stubbed; full spot/margin bookkeeping deferred to Phase 5
- [ ] implement Rust OMS service on current NATS subjects (in `zk-infra-rs`) — **deferred to Phase 5** (after backtester and live engine)
- [ ] implement shadow-mode compare flow — **deferred to Phase 5**

### Implementation notes
- `OmsMessage` / `OmsAction` typed enums replace Python's `OMSAction(action_type, dict, Any)`
- `handle_external_orders` / `external_order_dict` (renamed from `non_tq` — proto-level `OrderSourceTq/NonTq` tracked in WS10)
- Error path fix: `create_order` receives `None` context when `ctx.has_error()` to avoid panic on missing `symbol_ref`

### Outputs
- Rust OMS core ✓
- Rust OMS service — pending
- shadow-mode validation path — pending

### Exit criteria
- Rust OMS passes parity suite ✓
- can run in shadow mode safely — pending

## WS7: Backtester Port

### Goal
Move the hot execution path of backtesting to Rust while retaining Python orchestration.

### Tasks
- [x] port replay queue and event ordering (`event_queue.rs`: BtEventKind, EventQueue with type-priority tie-breaking)
- [x] port simulator core and matching policies (`simulator.rs`: SimulatorCore, order/tick/cancel; `match_policy.rs`: ImmediateMatchPolicy, FirstComeFirstServedMatchPolicy)
- [x] port strategy SDK shared kernel: `StrategyRunner`, `StrategyContext` (orders/balances), `TimerManager` (cron + one-shot), `Strategy` trait, `SAction` enum
- [x] port `__tq_init__` lifecycle hook: `init_data_fetcher` on `BacktestConfig`, `on_init` in `Strategy` trait + `StrategyRunner`; 11 parity tests green
- [x] define `zk-pyo3-rs` crate and add `maturin` / `pyo3` build setup
- [x] implement `ZkQuantAdapter` pyclass (`adapter.rs`): wraps `StrategyContext` snapshot + `Vec<SAction>` accumulator; maps legacy `tq.*` methods to `SAction`s (`buy`/`sell` → `PlaceOrder`, `set_timer_clock` → `SubscribeTimer::OnceAt`, `log` → `Log`, `get_account_balance` → reads ctx balances, `get_custom_init_data` → `ctx.get_init_data()`)
- [x] implement `PyStrategyAdapter` (Rust `Strategy` impl, `py_strategy.rs`): holds `Py<PyAny>` Python strategy; each callback acquires GIL, constructs `ZkQuantAdapter`, calls Python method, drains accumulated actions; tries legacy names first (`on_orderupdate`, `on_scheduled_time`)
- [x] implement `RustBacktester` pyclass (`runner.rs`): `push_bar(ts, sym, o, h, l, c, v)` loader + `run(strategy, config, fetcher)` entry point; `BacktestResult` → Python dicts
- [x] expose `zk_backtest.sim_core.ImmediateMatchPolicy` and `FirstComeFirstServedMatchPolicy` as Python-importable marker types (`sim_core.rs`)
- [x] `InitDataFetcher` made `Send`; `Backtester::set_init_data_fetcher()` API added; `StrategyContext::get_balances_map()` / `account_ids()` added
- [ ] **[NEXT]** run ETS end-to-end via `RustBacktester`; assert fill count, log shape, PnL match `ETS_kline_bt_local.py` Python-only baseline
- [ ] adapt Python orchestration (`StrategyBacktestor`) to optionally delegate the inner backtest loop to Rust — keep `get_trades()` / `BacktestReport` flows intact
- [ ] extend `BacktestResult` bridge to include fills/trades in a DataFrame-compatible shape
- [ ] (after WS5) run ETS on locally prepared ArcticDB kline slice as regression fixture

### PyO3 callback API mapping

| Python (legacy TokkaQuant) | Rust (Strategy trait) | Notes |
|---|---|---|
| `on_create(tq)` | `on_create(ctx)` | config not yet passed; tq adapter provides log only |
| `on_reinit(config, tq)` | `on_reinit(ctx)` | config dict injected via `init_data_fetcher` + `ctx.get_init_data()` |
| `on_tick(tick, tq)` | `on_tick(tick, ctx)` | direct mapping |
| `on_bar(bar, tq)` | `on_bar(bar, ctx)` | direct mapping |
| `on_orderupdate(oue, tq)` | `on_order_update(oue, ctx)` | rename |
| `on_scheduled_time(te, tq)` | `on_timer(te, ctx)` | rename |
| `tq.buy(**kwargs)` | `SAction::PlaceOrder(...)` | adapter translates kwargs → `StrategyOrder` |
| `tq.sell(**kwargs)` | `SAction::PlaceOrder(...)` | same, opposite side |
| `tq.set_timer_clock(name, dt)` | `SAction::SubscribeTimer(OnceAt(ts))` | dt → unix ms |
| `tq.get_account_balance(id)` | `ctx.account_state(id).balances()` | adapter wraps in dict-like object |
| `tq.get_custom_init_data()` | `ctx.get_init_data::<T>()` | kline history injected by fetcher |
| `tq.log(msg)` | `SAction::Log(...)` | direct |
| `tq.send_notification(...)` | no-op in backtest | elided |

### Outputs
- Rust backtest core ✓
- PyO3 extension crate `zk_backtest` ✓ (`zk-pyo3-rs`)
- `ZkQuantAdapter` + `PyStrategyAdapter` ✓
- `RustBacktester` pyclass ✓
- Python `BacktestConfig` / `BacktestResult` bridge — partial (dict output done; DataFrame / `get_trades()` pending)
- migrated orchestration path — pending

### Exit criteria
- Python can run `ETS_kline_bt.py` using the Rust backtester core
- ETS in-process fixture run is in the regression suite
- the ETS local-data reference workload works through the local-data test path (after WS5)

## WS8: Live Engine Port — CORE DONE

### Goal
Port live event orchestration to Rust.

### Tasks
- [x] `EngineEvent` enum: Tick, Bar, OrderUpdate, PositionUpdate, Signal, Timer
- [x] tick coalescing: deduplicate same-symbol ticks per batch before dispatch
- [x] `ActionDispatcher` trait + `NoopDispatcher` + `RecordingDispatcher`
- [x] `LiveEngine<S, D>`: async tokio mpsc event loop; startup lifecycle; SAction dispatch routing
- [x] `run_timer_clock`: 1 Hz async task — mirrors Python `run_clock()`
- [x] 6 parity tests green (`tests/engine_parity.rs`)
- [x] support Rust-native strategy runtime (via `Strategy` trait)
- [x] support embedded Python runtime (via `PyStrategyAdapter` in `zk-pyo3-rs`)
- [ ] NATS event source: ticks/bars/order-updates/position-updates → channel (Phase 5)
- [ ] NATS action dispatcher: orders/cancels → OMS subjects; logs → publish (Phase 5)

### Outputs
- Rust live engine (`zk-engine-rs`) ✓
- Rust strategy SDK (`zk-strategy-sdk-rs`) ✓ (shared with backtester)
- embedded Python runtime adapter (`zk-pyo3-rs::PyStrategyAdapter`) ✓

### Exit criteria
- one Rust engine instance can run a Rust strategy and a Python-embedded strategy ✓ (Rust-native tested; Python-embedded structurally wired)

## WS9: Python Worker Runtime

### Goal
Support isolated Python strategy execution for higher criticality strategies.

### Tasks
- [ ] define worker lifecycle and heartbeat
- [ ] implement event/action transport
- [ ] implement timeout/backpressure behavior
- [ ] implement worker supervision and restart policy
- [ ] add parity tests against embedded runtime

### Outputs
- Python worker runtime
- supervision model
- compatibility tests

### Exit criteria
- a Python strategy can run out-of-process with acceptable latency and recovery behavior

## WS10: Protocol Expansion

### Goal
Extend protobuf contracts for broader strategy categories and instrument classes.

### Tasks
- [ ] add strategy runtime typing
- [ ] add strategy category typing
- [ ] add richer execution intent/order metadata
- [ ] add richer instrument metadata for ETF/stock/crypto/cfd/options/futures support
- [ ] add versioning/deprecation policy

### Outputs
- proto change set
- compatibility notes
- migration policy

### Exit criteria
- protocol layer can describe target strategy and instrument breadth without ad hoc side channels

## WS11: Production Hardening And Cutover

### Goal
Validate Rust services under production-like conditions and provide a safe cutover path.

### Tasks
- [ ] define canary rollout plan and rollback triggers
- [ ] implement shadow traffic mode: Python and Rust process same events, compare outputs
- [ ] implement dual-run comparison mode for OMS and engine
- [ ] define SLOs for OMS (order state transition latency, balance update latency, reconciliation correctness)
- [ ] define SLOs for live strategy engine (event dispatch latency, timer precision, action execution latency)
- [ ] run performance and soak tests against production-representative workloads
- [ ] run failure injection: NATS disconnects, Redis stalls, malformed gateway reports, duplicate messages, worker hangs
- [ ] document observability requirements: metrics, traces, structured logs for Rust services
- [ ] document replay and recovery procedures
- [ ] define rollback procedure if Rust service must be reverted to Python

### Outputs
- canary and dual-run validation infrastructure
- SLO definitions for OMS and engine
- failure injection test results
- observability runbook
- rollback playbook

### Exit criteria
- Rust services can replace Python services in production without losing operational capabilities
- all failure injection scenarios have documented behaviors and pass acceptance thresholds
- shadow comparison shows no regressions against Python baseline on agreed parity suite

## WS12: gRPC Transport + NATS KV Discovery Migration

### Goal
Move Rust live services to `gRPC + NATS pub/sub` with dynamic discovery through `NATS KV`.

### Migration order
1. simulator service
2. OMS service
3. strategy engine service
4. gateways stay unchanged in this wave

### Tasks
- [ ] define `NATS KV` registry schema:
  - [ ] key patterns for gateway/oms/engine/sim registrations
  - [ ] endpoint payload fields (address, protocol, capabilities, account metadata, TTL)
  - [ ] heartbeat/lease semantics and stale-entry cleanup behavior
- [ ] implement gateway registration publisher:
  - [ ] register gateway transport info at startup
  - [ ] register account ownership/coverage metadata
  - [ ] refresh lease until shutdown
- [ ] implement OMS discovery + registration flow:
  - [ ] load account config from DB
  - [ ] resolve required gateways from KV by account metadata
  - [ ] connect using discovered gateway transport info
  - [ ] register OMS gRPC endpoint keyed by OMS id
- [ ] implement simulator gRPC service endpoint and KV registration
- [ ] implement engine OMS resolution flow:
  - [ ] load configured OMS id
  - [ ] resolve OMS endpoint from KV
  - [ ] connect via gRPC and recover on endpoint changes
- [ ] integrate failover behavior:
  - [ ] KV watch-based endpoint refresh
  - [ ] reconnect backoff/jitter
  - [ ] stale endpoint eviction based on lease timeout
- [ ] document bootstrap and local-dev workflow

### Outputs
- `gRPC` service endpoints for simulator, OMS, and engine
- `NATS KV` registry contract + lease model
- startup/discovery/reconnect runbook

### Exit criteria
- simulator, OMS, and engine communicate via gRPC in integration tests
- OMS can discover gateway transport/account registrations from KV and route correctly
- engine can resolve OMS by configured OMS id and reconnect after OMS restart

### Implementation notes (added after initial port)
- `zk-strategy-sdk-rs`: `Strategy` trait with no-op defaults; `SAction` enum (PlaceOrder, Cancel, Log, SubscribeTimer); all callbacks return `Vec<SAction>` (pure functional, no accumulated side-state)
- `zk-backtest-rs`: `BacktestOms` wraps Rust `OmsCore` + `SimulatorCore`; `Backtester` owns the `EventQueue` (min-heap by ts + type-priority) and drives the event loop; `ImmediateMatchPolicy` and `FirstComeFirstServedMatchPolicy` both ported
- 5 parity tests in `tests/backtest_parity.rs`: immediate fill, FCFS fill-on-tick, cancel-before-fill, price-miss no-fill, sell-against-bid; all green
- **WS7 extension (strategy SDK shared kernel)**:
  - `context.rs` — `StrategyContext` + `AccountState` + `OrderView<'_>`: tracks pending/open/terminal orders and balances per account; mirrors Python `StrategyStateProxy`
  - `timer_manager.rs` — `TimerManager` with cron (`cron` crate, 6-field) + one-shot timers, min-heap based `drain_due(now_ms)`; mirrors Python `TimerManager`
  - `runner.rs` — `StrategyRunner` owns `ctx + timer`; shared kernel for both `Backtester` and future `LiveEngine`; mirrors Python `StrategyTemplate`; all callbacks update ctx before dispatching to strategy
  - `Strategy` trait: all callbacks now receive `ctx: &StrategyContext` for read-only state queries
  - `Backtester` refactored to delegate all dispatch to `StrategyRunner`; `SubscribeTimer` actions routed to `runner.timer`
  - 9 parity tests total (added: once-timer fires once, cron fires 5×, open-orders visible in ctx, position visible in ctx); all green
- Python binding (PyO3) and orchestration migration are deferred (see remaining tasks above)
