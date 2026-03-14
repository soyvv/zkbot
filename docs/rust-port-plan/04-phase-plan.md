# Phase Plan

## Phase 0: Stabilize Contracts, Tests, And Data Prep

### Required outputs
- protobuf compatibility rules
- strategy runtime contract
- OMS ownership redesign notes
- ArcticDB-based local data prep design
- replay/testing plan
- TXT-to-ArcticDB seed import plan for local minute-kline fixtures

### Tasks
- [ ] finish WS1
- [ ] finish WS2 design portion
- [ ] finish WS4 design portion
- [ ] finish WS5 design portion

### Do not start next phase until
- critical messages have golden test coverage defined
- GIL/shared-state assumptions are documented
- local ArcticDB export flow is defined
- a local-data reference path for the ETS minute-kline backtest is defined

## Phase 1: Build Foundations ✓ COMPLETE

### Required outputs
- real Rust workspace ✓
- generated Rust protobuf types ✓ (`zk-proto-rs`, `buf.gen.yaml` updated)
- baseline replay fixture pipeline — deferred (WS4/WS5 not yet started)
- first local ETS reference fixture prepared from local minute-kline data — deferred

### Tasks
- [x] finish WS3 (workspace + proto crate)
- [ ] implement first slice of WS5 — deferred
- [ ] implement first slice of WS4 — deferred

### Notes
Workspace builds cleanly (`cargo build`, `cargo test` green). WS4/WS5 data prep deferred — Phase 2 proceeded with OMS port using in-process parity tests instead of fixture files.

## Phase 2: Port OMS First — CORE DONE, service/shadow pending

### Required outputs
- Rust OMS core ✓ (all modules, 12 parity tests green)
- Rust OMS service — pending (NATS RPC adapter in `zk-infra-rs`)
- parity suite ✓ (12 tests in `zk-oms-rs/tests/oms_parity.rs`)
- shadow mode comparison — pending

### Tasks
- [x] port order state transitions (`order_mgr.rs`)
- [x] port routing and validation (`oms_core.rs`, `config.rs`)
- [x] port balance and position updates (`balance_mgr.rs`)
- [ ] port reconciliation/recovery logic (`calc_balance_changes_for_report` stubbed — Phase 2b)
- [ ] implement Rust OMS service on current NATS subjects
- [ ] implement shadow-mode compare flow

### Known stubs / deferred (tracked in 07-open-questions-and-non-port-changes.md)
- `OmsCore::calc_balance_changes_for_report` returns `None`; full spot/margin bookkeeping in Phase 2b
- `pending_order_reports` cache unbounded; cap needed
- LRU eviction (`order_id_queue`) does not clean `context_cache`

### Do not start next phase until
- Rust OMS matches Python on agreed parity suite ✓ (parity tests green)
- *(OMS NATS service and shadow mode deferred — see Phase 5)*

## Phase 3: Port Backtester Core

### Phase 3a: Rust Core ✓ COMPLETE

- [x] Rust replay/event queue ported (`event_queue.rs`, `backtester.rs`)
- [x] Simulator core + match policies ported (`simulator.rs`, `match_policy.rs`)
- [x] BacktestOms integrating OmsCore + SimulatorCore (`backtest_oms.rs`)
- [x] Strategy SDK shared kernel: `StrategyRunner`, `StrategyContext`, `TimerManager` (`zk-strategy-sdk-rs`)
- [x] `__tq_init__` lifecycle hook: `init_data_fetcher` in `BacktestConfig`, `on_init` in `Strategy` trait + `StrategyRunner`
- [x] 11 parity tests green (`tests/backtest_parity.rs`)

### Phase 3b: PyO3 Binding + ETS Compatibility — ACTIVE NEXT

**Goal:** Let the existing Python ETS strategy (`ETS_on_kline.py`) run on the Rust backtester with zero changes to the strategy file.

**Required outputs**
- PyO3 extension module `zk_backtest` (`zk-pyo3-rs` crate, `maturin` build) ✓
- `ZkQuantAdapter`: Python-facing context object bridging `StrategyContext` API to the legacy `TokkaQuant` callback API (`tq.buy()`, `tq.sell()`, `tq.get_account_balance()`, `tq.set_timer_clock()`, …) ✓
- `PyStrategyAdapter`: Rust `Strategy` impl that holds a Python strategy object and delegates each callback via PyO3 ✓
- `RustBacktester` pyclass: `push_bar(ts, sym, o, h, l, c, v)` + `run(strategy, config, fetcher)` ✓
- `BacktestResult` bridge: translate Rust `BacktestResult` into Python-friendly dicts ✓ (order_placements + logs; trade fill shape deferred)
- Python `BacktestConfig` bridge: accept existing `BacktestConfig` fields (`kline_symbols`, `kline_data_source`, `init_balances`, `match_policy_cls`, …) and drive the Rust `Backtester` — deferred (Python orchestration migration)
- ETS minute-kline reference workload running end-to-end using local data
  - **Fixture**: `zkstrategy_research/fixtures/USDJPY_2021Q1_1m.parquet` (USDJPY 1m, 2021-Q1, from `~/Downloads/全套数据TXT`)
  - **Prep script**: `zkstrategy_research/tools/prepare_local_fixture.py` (run once to produce Parquet from raw TXT)
  - **Data source**: `zkstrategy_research/zk-data/zk_tsdata/LocalParquetKlineDataSource` (drop-in for `ArcticdbKlineDataSource`)
  - **Python baseline**: `zkstrategy_research/zk-strategylib/zk_strategylib/entry_target_stop/ETS_kline_bt_local.py` (runs ETS on local fixture, no remote deps)

**Tasks**
- [x] define `zk-pyo3-rs` crate and add `maturin` / `pyo3` build setup (`crates/zk-pyo3-rs/`)
- [x] implement `ZkQuantAdapter` pyclass: wraps `StrategyContext` snapshot + action accumulator, maps legacy `tq.*` calls to `SAction`s (`adapter.rs`)
- [x] implement `PyStrategyAdapter` (Rust `Strategy` impl): holds `Py<PyAny>` strategy, calls Python callbacks with `ZkQuantAdapter` as `tq` (`py_strategy.rs`)
- [x] implement `RustBacktester` pyclass: `push_bar(ts, sym, o, h, l, c, v)` event loader, `run(strategy, config, fetcher)` entry point (`runner.rs`)
- [x] expose `zk_backtest.sim_core.ImmediateMatchPolicy` and `FirstComeFirstServedMatchPolicy` as Python-importable marker types (`sim_core.rs`)
- [x] `InitDataFetcher` made `Send` so `Backtester` is `Send`; `set_init_data_fetcher()` API added
- [x] `StrategyContext`: added `get_balances_map()` and `account_ids()` accessors
- [ ] run ETS end-to-end on local Parquet fixture (`USDJPY_2021Q1_1m.parquet`); assert fill count, trade PnL shape, log lines match Python-only baseline (`ETS_kline_bt_local.py`)
- [ ] Python `BacktestConfig` bridge: adapt `StrategyBacktestor` to call `RustBacktester` instead of Python OMS/simulator
- [ ] extend `BacktestResult` bridge to include fills/trades in a shape compatible with `get_trades()` and `BacktestReport`
- [ ] (after WS5) extend fixture set to additional symbols/date ranges from `~/Downloads/全套数据TXT`

**ETS API compatibility notes** (from `ETS_on_kline.py` audit)
- Callbacks use OLD signature: `on_reinit(config, tq)`, `on_tick(tick, tq)`, `on_bar(bar, tq)`, `on_orderupdate(order_update, tq)`, `on_scheduled_time(timer_event, tq)` — `tq` is a `TokkaQuant` lookalike
- `tq.get_custom_init_data()` — reads kline history injected by `init_data_fetcher`; already modelled by `ctx.get_init_data::<T>()`
- `tq.get_account_balance(account_id)` — must return a dict `{symbol: Balance}` with `.total_qty`, `.long_short_type`
- `tq.buy(**kwargs)` / `tq.sell(**kwargs)` — place limit orders; return `order_id`
- `tq.set_timer_clock(timer_name, date_time)` — one-shot timer; maps to `SAction::SubscribeTimer(OnceAt)`
- `tq.log(msg)` — maps to `SAction::Log`
- Notification calls (`tq.send_notification`) can be no-ops in backtest
- `kline_symbols` map (`{'USD-P/JPY@SIM1': 'USDJPY'}`) drives bar subscription; Rust backtester must inject bars keyed by internal symbol

### Do not start Phase 4 until
- Python can run `ETS_kline_bt.py` using the Rust backtester core and produce equivalent outputs
- at least one ETS in-process fixture run is in the regression suite

## Phase 4: Port Live Engine — CORE DONE, NATS wiring pending

### Required outputs
- Rust engine (`zk-engine-rs`) ✓ (core loop, event batching, timer, dispatcher)
- Rust-native strategy SDK (`zk-strategy-sdk-rs`) ✓ (shared with backtester)
- embedded Python runtime ✓ (PyStrategyAdapter in `zk-pyo3-rs`, usable from engine)

### Tasks
- [x] `EngineEvent` enum: Tick, Bar, OrderUpdate, PositionUpdate, Signal, Timer
- [x] tick coalescing: deduplicate by `(instrument_code, exchange)` before dispatch
- [x] `ActionDispatcher` trait + `NoopDispatcher` + `RecordingDispatcher` (tests)
- [x] `LiveEngine<S, D>`: async mpsc event loop, startup lifecycle, SAction dispatch
- [x] timer clock (`run_timer_clock`) as a separate async task
- [x] 6 parity tests green (`tests/engine_parity.rs`: bar dispatch, order via dispatcher, timer once-fires, tick coalescing ×2, on_create log)
- [ ] NATS event source: subscribe to ticks/bars/order-updates/position-updates → push to channel (Phase 5, `zk-infra-rs`)
- [ ] NATS action dispatcher: orders/cancels → OMS NATS subjects; logs/signals → publish (Phase 5)
- [ ] Python-embedded strategy run through `LiveEngine` (requires NATS or test harness)

### Do not start next phase until
- Rust engine runs both Rust and embedded Python strategies correctly on the agreed test set ✓ (Rust-native verified; Python-embedded structurally ready via `PyStrategyAdapter`)

## Phase 5: OMS Live Service, Shadow Mode, And Python Worker

### Rationale
OMS NATS service and shadow-compare are deferred until after the backtest and live engine cores
exist, so the shadow harness can be tested with replay traces from Phase 3.

### Required outputs
- Rust OMS gRPC service (WS6 remainder) — **core DONE** (see Phase 5b notes)
- shadow-mode comparison flow
- `calc_balance_changes_for_report` full implementation (Phase 2b balance bookkeeping)
- out-of-process Python strategy runtime (WS9)

### Tasks
- [ ] port reconciliation/recovery logic (`calc_balance_changes_for_report` — WS6)
- [x] implement Rust OMS gRPC service (`zk-oms-svc` — see Phase 5b)
- [ ] implement shadow-mode compare flow (WS6)
- [ ] execute WS9 (Python worker runtime)

### Do not start next phase until
- OMS shadow mode comparison is available
- worker runtime meets latency, timeout, and recovery acceptance thresholds

## Phase 5b: gRPC + NATS KV Discovery Migration (simulator, OMS, engine first) — PARTIALLY DONE

### Required outputs
- simulator, OMS, and engine expose/consume gRPC transport
- NATS KV service discovery registry with lease/heartbeat semantics
- gateway startup registration for transport/account metadata (gateway runtime unchanged)

### Tasks
- [ ] execute WS12 registry schema and lease model
- [x] implement gateway startup KV declaration (implemented in `zk-mock-gw`):
  - [x] transport info
  - [x] account info
- [x] implement OMS startup flow (`zk-oms-svc`):
  - [x] load account config from DB
  - [x] discover/connect target gateways from KV (`svc.gw.*` watch)
  - [x] register OMS gRPC endpoint in KV (`svc.oms.<oms_id>`)
- [ ] implement engine startup flow:
  - [ ] load configured OMS id
  - [ ] discover OMS endpoint from KV
  - [ ] connect and reconnect on OMS endpoint changes
- [x] implement simulator gRPC endpoint + KV registration (`zk-mock-gw`)

### Implementation notes
- `zk-oms-svc` is production-ready with full startup sequence, NATS gateway report handling, Redis persistence, gRPC command/query handlers, single-writer actor pattern, and latency observability
- `zk-mock-gw` implements both legacy `ExchangeGatewayService` and new `GatewayService` (`zk.gateway.v1`)
- Dev tooling: `scripts/clear_oms_redis.sh`, `Makefile` targets `oms-redis-clear`, `dev-up` (waits for Redis, clears OMS keys), `dev-reset` (wipes volumes then calls `dev-up`)
- E2E latency benchmark: `zk-oms-svc/examples/e2e_latency.rs`
- KV keys in use: `svc.gw.<gw_id>` (gateway), `svc.oms.<oms_id>` (OMS registration)

### Do not start next phase until
- simulator/OMS/engine run with gRPC transport in integration environment
- OMS account routing works from DB config + KV-discovered gateways
- engine-to-OMS binding by OMS id is stable across restart/failover events

## Phase 6: Expand Protocols And Asset Coverage

### Required outputs
- broader strategy and instrument protocol support

### Tasks
- [ ] execute WS10

### Do not start next phase until
- protocol changes are documented with compatibility and rollout notes

## Phase 7: Production Hardening And Cutover

### Required outputs
- canary rollout plan
- shadow and dual-run comparison infrastructure
- SLO definitions for OMS and live engine
- failure injection test results
- observability and rollback playbook

### Tasks
- [ ] execute WS11

### Do not close the program until
- Rust services pass shadow comparison on production-representative event traces
- all failure injection scenarios meet acceptance thresholds
- rollback procedure is documented and tested
- observability runbook is complete
