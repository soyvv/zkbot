# Phase 6: Strategy Engine Service

## Goal

Implement `zk-engine-svc` — the Rust strategy engine binary that:
- registers with Pilot via REST on startup
- resolves OMS endpoint via NATS KV
- runs Rust-native strategy instances via `zk-engine-rs` + `zk-strategy-sdk-rs`
- optionally runs Python strategy scripts via `zk-pyo3-rs` Python worker mode
- deregisters from Pilot on shutdown

## Prerequisites

- Phase 1 complete (proto, `zk-infra-rs`)
- Phase 2 complete (OMS gRPC service)
- Phase 5 complete (`zk-trading-sdk-rs` with discovery and gRPC pool)
- Phase 4 complete (Pilot bootstrap service with `instance_id` lease)
- docker-compose stack running (NATS, PG, OMS, mock-gw)

## Deliverables

### 6.1 `zk-engine-svc` binary

Location: `zkbot/rust/crates/zk-engine-svc/`

#### `main.rs` — bootstrap

Mirrors `strat_service.py` startup sequence:

1. load `EngineSvcConfig` from env:
   - `ZK_STRATEGY_KEY`, `ZK_INSTANCE_ID` (u16, 0–1023)
   - `ZK_NATS_URL`, `ZK_PILOT_API_URL`
   - `ZK_EXT_SCRIPTS_DIR` (for Python strategy scripts)
   - `ZK_GRPC_PORT` (optional, for `EngineService` gRPC)
2. generate `execution_id` locally: `Uuid::new_v4().to_string()`
3. init tracing + metrics
4. connect to NATS via `zk-infra-rs`
5. POST to Pilot REST `POST /v1/strat/strategy-inst`:
   - body: `{"strategy_key": <key>, "execution_id": <uuid>, "instance_id": <u16>}`
   - response: strategy config (`symbols`, `account_ids`, `tq_config`, `strategy_script_file_name`, etc.)
   - on error or 409 conflict: log and exit (do not start with unknown config)
6. initialize `TradingClient` from `zk-trading-sdk-rs` with resolved `account_ids` and `instance_id`
7. resolve OMS gRPC endpoint via NATS KV (handled by `TradingClient::from_config`)
8. open gRPC channel to OMS with backoff reconnect (handled by SDK)
9. subscribe to NATS RTMD and account event topics per configured symbols/accounts
10. initialize strategy runtime:
    - Rust strategy: instantiate from `zk-strategy-sdk-rs` registry by `strategy_key`
    - Python strategy: launch Python worker subprocess via `zk-pyo3-rs`; establish IPC channel
11. start `EngineService` gRPC server on `ZK_GRPC_PORT` (optional, for control plane queries)
12. register `svc.engine.<execution_id>` in NATS KV via `KvRegistryClient`
13. start engine event loop

#### Shutdown sequence (mirrors `strat_service.py`):

On SIGTERM or unrecoverable error:
1. publish `StrategyLifecycleNotifyEvent{status: STOPPED}` to `zk.strategy.<strategy_id>.lifecycle`
2. DELETE `{ZK_PILOT_API_URL}/v1/strat/strategy-inst?strategy_key=<key>&execution_id=<uuid>`
3. cancel `KvRegistryClient` handle (entry expires by TTL)
4. graceful shutdown of gRPC server and NATS connection

#### `engine_loop.rs` — event loop

```rust
pub async fn run_engine_loop(
    strategy: Box<dyn Strategy>,
    trading_client: Arc<TradingClient>,
    nats_subscriptions: Vec<NatsSubscription>,
    shutdown: CancellationToken,
) -> Result<()>;
```

Event dispatch:
- RTMD tick → `strategy.on_tick(tick)`
- RTMD kline → `strategy.on_kline(kline)`
- OMS `OrderUpdateEvent` → `strategy.on_order_update(event)`
- OMS `BalanceUpdateEvent` → `strategy.on_balance_update(event)`
- timer events → `strategy.on_timer(timer_id)`

Strategy actions:
- `place_order(account_id, order)` → `trading_client.place_order(...)`
- `cancel_order(account_id, cancel)` → `trading_client.cancel_order(...)`
- publish log/signal → NATS `zk.strategy.<id>.log` / `zk.strategy.<id>.signal`

#### `grpc_handler.rs` — `EngineService` (optional)

Implement `zk.engine.v1.EngineService` for control plane use:
- `StopStrategy`: sets `shutdown` cancellation token
- `PauseStrategy` / `ResumeStrategy`: pause/resume event processing
- `QueryStrategyState`: return current strategy internal state snapshot
- `QueryRuntimeMetrics`: return Prometheus-style metrics snapshot

### 6.2 Python worker mode (`zk-pyo3-rs`)

For Python strategy scripts in `ZK_EXT_SCRIPTS_DIR`:

1. engine spawns a Python subprocess with `zk_pyo3` loaded
2. subprocess imports strategy script, instantiates `StrategyBase` subclass
3. engine ↔ Python worker communicate via `tokio::sync::mpsc` channel (encoded as msgpack)
4. Python worker calls `trading_client.place_order(...)` which routes through the in-process Rust SDK

Python strategy interface (mirrors existing `StrategyBase`):
```python
class MyStrategy(StrategyBase):
    def on_tick(self, tick: TickData): ...
    def on_order_update(self, event: OrderUpdateEvent): ...
    def on_timer(self, timer_id: str): ...
```

### 6.3 `zk-engine-rs` domain crate

Location: `zkbot/rust/crates/zk-engine-rs/`

Domain logic only — no infra imports:
- `Strategy` trait definition
- timer management
- in-memory state (positions, PnL tracking per strategy)
- signal generation utilities

`zk-strategy-sdk-rs`:
- strategy registry (name → factory)
- helper utilities for order sizing, risk checks
- common strategy patterns (market-making scaffold, momentum scaffold)

## Tests

### Unit tests

- `test_engine_startup_registers_execution_id`: mock Pilot REST → assert POST sent with `execution_id` UUID and `strategy_key`
- `test_engine_shutdown_deregisters`: trigger shutdown → assert DELETE sent to Pilot
- `test_engine_shutdown_publishes_lifecycle_event`: trigger shutdown → assert NATS `StrategyLifecycleNotifyEvent{STOPPED}` published
- `test_engine_on_tick_dispatches_to_strategy`: send synthetic tick → assert `strategy.on_tick` called with correct payload
- `test_engine_place_order_routes_to_oms`: strategy calls `place_order` → assert OMS gRPC called (mock OMS)
- `test_engine_pilot_registration_failure_aborts_startup`: Pilot returns 409 → engine exits with non-zero code

### Integration tests (docker-compose)

- `test_engine_startup_full_sequence`:
  1. start OMS + mock-gw + Pilot bootstrap service
  2. start engine with `ZK_STRATEGY_KEY=strat_test`
  3. assert Pilot receives `POST /v1/strat/strategy-inst`
  4. assert `svc.engine.<execution_id>` appears in NATS KV within 10s
  5. assert NATS `OrderUpdateEvent` subscriptions active for account 9001
- `test_engine_strategy_place_order_roundtrip`:
  1. inject synthetic tick on `zk.rtmd.tick.OKX.BTC-USDT`
  2. strategy processes tick and calls `place_order`
  3. assert OMS receives PlaceOrder gRPC request
  4. mock-gw fills → assert `OrderUpdateEvent(FILLED)` delivered to strategy
- `test_engine_shutdown_on_sigterm`:
  1. start engine
  2. send SIGTERM
  3. assert `StrategyLifecycleNotifyEvent{STOPPED}` published within 2s
  4. assert Pilot receives DELETE request
  5. assert KV entry expires within 25s
- `test_python_worker_place_order` (requires Python strategy fixture):
  1. load `tests/fixtures/dummy_strategy.py` (places one order on first tick)
  2. inject tick → assert order placed via OMS

## Exit criteria

- [ ] `cargo build -p zk-engine-svc` succeeds
- [ ] unit tests pass: `cargo test -p zk-engine-svc`
- [ ] `test_engine_startup_full_sequence` passes against docker-compose stack
- [ ] `test_engine_strategy_place_order_roundtrip` completes within 1s end-to-end
- [ ] `test_engine_shutdown_on_sigterm` passes: lifecycle event + Pilot deregister + KV expiry
- [ ] Pilot bootstrap `instance_id` conflict rejected when same `instance_id` used by two running engines
- [ ] Python strategy `dummy_strategy.py` successfully calls `place_order` via worker IPC
- [ ] No `unimplemented!()` or `todo!()` in `zk-engine-svc` or `zk-engine-rs`
