# Phase 6: Strategy Engine Service

## Goal

Implement `zk-engine-svc` — the Rust strategy engine binary that:
- claims a strategy execution from Pilot on startup and retrieves runtime config there
- resolves OMS endpoint via NATS KV
- runs Rust-native strategy instances via `zk-engine-rs` + `zk-strategy-sdk-rs`
- optionally runs Python strategy scripts via `zk-pyo3-rs` Python worker mode
- finalizes execution with Pilot on shutdown

## Design constraints

This phase should follow these rules:

1. runtime config is retrieved from Pilot
2. `execution_id` identifies one concrete run of a strategy for a period of time
3. only one active execution instance is allowed per `strategy_key`
4. the shared service discovery mechanism remains generic and business-agnostic

Implication:

- strategy singleton policy must be enforced by Pilot control-plane logic
- `execution_id` should be execution metadata, not the discovery key
- the KV registration contract should stay generic; Pilot should provide the logical identity and metadata/profile for the engine registration

## Execution supervision mode

The strategy service should support two runtime ownership/supervision modes:

- `BEST_EFFORT`
- `CONTROLLED`

These are not different discovery protocols. They are different reactions to control-plane/discovery degradation.

### `BEST_EFFORT`

- engine keeps running even if the service discovery / registry client becomes unhealthy
- engine still logs loudly and should expose degraded health
- ownership fencing is still fatal

Suitable for:

- development
- research
- manual operation
- environments where temporary control-plane loss should not stop the strategy immediately

### `CONTROLLED`

- engine is expected to remain under control-plane supervision
- if the service discovery / registry client is unhealthy for longer than a configured grace period, the engine terminates itself
- ownership fencing is fatal immediately

Suitable for:

- production singleton strategies
- strict failover environments
- cases where running without trustworthy control-plane visibility is considered unsafe

Important rule:

- CAS fencing / ownership loss: always terminate in both modes
- discovery-client degradation:
  - `BEST_EFFORT`: continue in degraded mode
  - `CONTROLLED`: terminate after grace period

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
   - `ZK_STRATEGY_KEY`
   - `ZK_NATS_URL`, `ZK_PILOT_API_URL`
   - `ZK_EXT_SCRIPTS_DIR` (for Python strategy scripts)
   - `ZK_GRPC_PORT` (optional, for `EngineService` gRPC)
   - `ZK_EXECUTION_MODE` (`BEST_EFFORT` | `CONTROLLED`)
   - `ZK_SD_FAILURE_GRACE_MS` (used in `CONTROLLED` mode)
   - `ZK_ENABLE_EMBEDDED_RTMD_GW` (optional)
2. init tracing + metrics
3. connect to NATS via `zk-infra-rs`
4. call Pilot control-plane start/claim API:
   - suggested endpoint: `POST /v1/strategy-executions/start`
   - body: `{"strategy_key": <key>, "runtime_info": {...}}`
   - Pilot atomically:
     - validates strategy is enabled
     - enforces singleton policy for `strategy_key`
     - allocates authoritative `execution_id`
     - returns strategy config (`symbols`, `account_ids`, `strategy_script_file_name`, etc.)
     - returns any engine-specific runtime metadata/profile needed for registration
     - returns RTMD role/profile metadata when the runtime should act as an embedded RTMD gateway
   - on error or conflict: log and exit before initializing the runtime
5. initialize `TradingClient` from `zk-trading-sdk-rs` with the returned `account_ids`
6. resolve OMS gRPC endpoint via NATS KV (handled by `TradingClient::from_config`)
7. open gRPC channel to OMS with backoff reconnect (handled by SDK)
8. subscribe to NATS RTMD and account event topics per configured symbols/accounts
9. initialize strategy runtime:
    - Rust strategy: instantiate from `zk-strategy-sdk-rs` registry by `strategy_key`
    - Python strategy: launch Python worker subprocess via `zk-pyo3-rs`; establish IPC channel
10. if Pilot profile enables embedded RTMD gateway mode:
    - initialize shared RTMD runtime components (`RtmdVenueAdapter`, `SubscriptionManager`, `Publisher`)
    - fetch effective desired RTMD subscription set from Pilot
    - prepare an additional `mdgw` registration grant for the same process
11. start `EngineService` gRPC server on `ZK_GRPC_PORT` (optional, for control plane queries)
12. register in NATS KV using a stable logical key granted by Pilot
    - preferred: `svc.engine.<strategy_key>`
    - `execution_id` is included in the registration payload, not used as the KV key
13. if embedded RTMD gateway mode is enabled and this runtime publishes shared RTMD topics:
    - also register the RTMD role using the Pilot-granted `svc.mdgw.<logical_id>` key
14. start engine supervision task:
    - always monitor registration fencing
    - monitor service discovery / registry client health
    - in `CONTROLLED` mode: terminate if discovery stays unhealthy longer than the configured grace window
    - in `BEST_EFFORT` mode: keep running but mark degraded
    - supervise embedded RTMD registration if present
15. start engine event loop

Architecture reference:

- [Engine Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/engine_service.md)
- [Market Data Gateway Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/market_data_gateway_service.md)

#### Graceful shutdown sequence

On SIGTERM or unrecoverable error:
1. stop accepting new strategy actions and cancel timers
2. publish `StrategyLifecycleNotifyEvent{status: STOPPING}` to `zk.strategy.<strategy_key>.lifecycle`
3. delete the engine KV registration immediately
4. drain subscriptions / gRPC / local runtime tasks
5. call Pilot finalize API:
   - suggested endpoint: `POST /v1/strategy-executions/stop`
   - body: `{"strategy_key": <key>, "execution_id": <id>, "reason": "graceful_shutdown"}`
6. if embedded RTMD gateway mode is active:
   - stop RTMD publisher and subscription manager
   - delete `svc.mdgw.<logical_id>` registration if one was created
7. Pilot marks the execution stopped and releases any strategy/engine-specific control-plane state
8. publish `StrategyLifecycleNotifyEvent{status: STOPPED}` if final stopped notification is engine-owned
9. close NATS and process resources

#### Hard shutdown / crash recovery

Hard shutdown includes:

- `SIGKILL`
- process crash / panic
- node/pod death
- network partition severe enough to stop KV heartbeats

In this path, no explicit Pilot deregistration is assumed.

Recovery rules:

1. engine KV registration disappears by delete/purge/expiry or ownership loss
2. embedded RTMD registration disappears too if the same process owned an `mdgw` role
3. Pilot reconciler observes the loss of the stable engine KV key and any associated RTMD role
4. Pilot marks the old execution `fenced` / `crashed` / `expired`
5. Pilot marks embedded RTMD publisher state stale if applicable
6. Pilot releases engine-specific leases/state if applicable
7. a replacement engine may call `start` for the same `strategy_key`
8. Pilot returns a new `execution_id`

This preserves:

- one active execution per strategy
- fresh `execution_id` per run
- recovery without relying on graceful shutdown callbacks
- bounded cleanup for embedded RTMD publisher roles owned by the same runtime

Mode interaction:

- in `BEST_EFFORT` mode, temporary discovery-client failure does not by itself imply hard shutdown
- in `CONTROLLED` mode, prolonged discovery-client failure is treated as a fatal supervision failure even if the process itself is otherwise healthy

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

#### `supervisor.rs` — ownership and discovery health supervisor

```rust
pub async fn run_supervisor(
    execution_mode: ExecutionMode,
    sd_failure_grace_ms: u64,
    registration: &mut ServiceRegistration,
    trading_client: Arc<TradingClient>,
    shutdown: CancellationToken,
) -> Result<()>;
```

Supervisor responsibilities:

- wait on `registration.wait_fenced()` and terminate immediately on ownership loss
- monitor SDK discovery health / registry-watch health
- if embedded RTMD mode is enabled, monitor the RTMD registration fence signal too
- in `CONTROLLED` mode:
  - start a timer when discovery becomes unhealthy
  - terminate if unhealthy duration exceeds `sd_failure_grace_ms`
- in `BEST_EFFORT` mode:
  - record degraded state and continue running

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

- `test_engine_startup_claims_strategy_and_receives_execution_id`: mock Pilot start API → assert engine does not self-generate authoritative execution ownership
- `test_engine_startup_conflict_aborts_before_runtime_init`: Pilot returns singleton conflict for `strategy_key` → engine exits before strategy runtime starts
- `test_engine_graceful_shutdown_finalizes_execution`: trigger shutdown → assert Pilot stop/finalize API called with `strategy_key` + `execution_id`
- `test_engine_shutdown_publishes_stopping_then_stopped`: trigger graceful shutdown → assert lifecycle sequencing is `STOPPING` before final stop
- `test_supervisor_best_effort_tolerates_sd_failure`: discovery health becomes unhealthy → engine remains running in degraded state
- `test_supervisor_controlled_exits_after_sd_grace`: discovery health remains unhealthy past grace window → engine terminates
- `test_supervisor_always_exits_on_fencing`: CAS fencing triggers termination in both modes
- `test_engine_embedded_rtmd_reuses_shared_runtime`: engine in `engine+mdgw` mode starts shared RTMD runtime components from the gateway layer
- `test_engine_embedded_rtmd_registration_optional`: local-only embedded RTMD config does not create a separate `svc.mdgw.*` entry
- `test_engine_on_tick_dispatches_to_strategy`: send synthetic tick → assert `strategy.on_tick` called with correct payload
- `test_engine_place_order_routes_to_oms`: strategy calls `place_order` → assert OMS gRPC called (mock OMS)
- `test_engine_hard_shutdown_recovery_relies_on_kv_loss`: unit/integration boundary test for reconciler-driven recovery path

### Integration tests (docker-compose)

- `test_engine_startup_full_sequence`:
  1. start OMS + mock-gw + Pilot bootstrap service
  2. start engine with `ZK_STRATEGY_KEY=strat_test`
  3. assert Pilot receives strategy start/claim request
  4. assert a stable engine KV key for the strategy appears within 10s
  5. assert returned registration payload contains `execution_id`
  6. assert no second engine for the same `strategy_key` can start while the first remains live
  7. assert NATS `OrderUpdateEvent` subscriptions active for account 9001
- `test_engine_hard_shutdown_allows_replacement_after_kv_loss`:
  1. start engine for `strategy_key=strat_test`
  2. kill it without graceful finalize
  3. wait for KV loss / Pilot reconciliation
  4. start replacement engine for the same `strategy_key`
  5. assert replacement succeeds with a new `execution_id`
- `test_engine_singleton_rejected_while_original_still_live`:
  1. start engine for `strategy_key=strat_test`
  2. start second engine with same `strategy_key`
  3. assert Pilot rejects second start attempt
- `test_engine_controlled_mode_exits_when_sd_unhealthy_too_long`:
  1. start engine in `CONTROLLED` mode
  2. break SDK discovery / registry watch health long enough
  3. assert engine exits
- `test_engine_best_effort_mode_survives_sd_unhealthy_window`:
  1. start engine in `BEST_EFFORT` mode
  2. break SDK discovery / registry watch health
  3. assert engine stays alive but reports degraded state
- `test_engine_embedded_rtmd_gateway_registration`:
  1. start engine in `engine+mdgw` mode
  2. assert `svc.engine.<strategy_key>` and `svc.mdgw.<logical_id>` both appear when shared publishing is enabled
  3. assert RTMD topics are published from the embedded runtime
- `test_engine_strategy_place_order_roundtrip`:
  1. inject synthetic tick on `zk.rtmd.tick.OKX.BTC-USDT`
  2. strategy processes tick and calls `place_order`
  3. assert OMS receives PlaceOrder gRPC request
  4. mock-gw fills → assert `OrderUpdateEvent(FILLED)` delivered to strategy
- `test_engine_shutdown_on_sigterm`:
  1. start engine
  2. send SIGTERM
  3. assert `StrategyLifecycleNotifyEvent{STOPPING}` published
  4. assert engine KV entry is removed promptly
  5. assert Pilot receives finalize/stop request
  6. assert final lifecycle state is recorded as stopped
- `test_python_worker_place_order` (requires Python strategy fixture):
  1. load `tests/fixtures/dummy_strategy.py` (places one order on first tick)
  2. inject tick → assert order placed via OMS

## Exit criteria

- [ ] `cargo build -p zk-engine-svc` succeeds
- [ ] unit tests pass: `cargo test -p zk-engine-svc`
- [ ] `test_engine_startup_full_sequence` passes against docker-compose stack
- [ ] only one engine for a given `strategy_key` can be active at a time
- [ ] hard shutdown of an engine allows replacement startup after KV loss is reconciled
- [ ] replacement engine receives a fresh `execution_id`
- [ ] `CONTROLLED` mode exits after prolonged discovery failure
- [ ] `BEST_EFFORT` mode survives discovery failure but exposes degraded status
- [ ] embedded RTMD gateway mode reuses the shared RTMD runtime and registers `mdgw` only when publishing a shared feed
- [ ] `test_engine_strategy_place_order_roundtrip` completes within 1s end-to-end
- [ ] `test_engine_shutdown_on_sigterm` passes: STOPPING + KV delete + Pilot finalize
- [ ] Pilot bootstrap `instance_id` conflict rejected when same `instance_id` used by two running engines
- [ ] Python strategy `dummy_strategy.py` successfully calls `place_order` via worker IPC
- [ ] No `unimplemented!()` or `todo!()` in `zk-engine-svc` or `zk-engine-rs`
