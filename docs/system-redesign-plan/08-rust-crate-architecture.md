# ZKBot Rust Crate Architecture

Crate dependency graph, responsibilities, and new components needed.

## 1. Crate Inventory

| Crate | Status | Type |
|---|---|---|
| `zk-proto-rs` | exists | library — generated prost bindings |
| `zk-domain-rs` | exists | library — core domain enums and state types |
| `zk-oms-rs` | complete (Phase 2, 12 parity tests green) | library — OMS domain logic |
| `zk-strategy-sdk-rs` | exists | library — strategy trait and context |
| `zk-engine-rs` | exists | library — live event loop and timer |
| `zk-backtest-rs` | exists | library — backtest engine and matching |
| `zk-pyo3-rs` | exists | cdylib + rlib — PyO3 bindings for Python |
| `zk-infra-rs` | exists (partial) | library — NATS/Redis adapters, dataset readers |
| `zk-trading-sdk-rs` | **new** | library — client SDK replacing TQClient + ODS |
| `zk-oms-svc` | **new** | binary — OMS gRPC service host (3-layer mode) |
| `zk-engine-svc` | **new** | binary — Strategy Engine gRPC service host (3-layer mode) |
| `zk-gw-svc` | **new** | binary (per venue) — Trading Gateway gRPC service host |
| `zk-collocated-svc` | **new** | binary — 2-layer or all-in-one host (engine + OMS ± GW plugin in one process, inter-thread channels) |

## 2. Dependency Graph

```
zk-proto-rs
  └── zk-domain-rs
        ├── zk-oms-rs
        │     └── (used by zk-backtest-rs, zk-oms-svc)
        ├── zk-strategy-sdk-rs
        │     └── (used by zk-backtest-rs, zk-engine-rs)
        ├── zk-engine-rs
        │     └── (used by zk-engine-svc)
        ├── zk-backtest-rs
        │     └── (used by zk-pyo3-rs)
        └── zk-infra-rs
              └── (used by zk-trading-sdk-rs, service binaries)

zk-pyo3-rs ← zk-backtest-rs + zk-strategy-sdk-rs + zk-proto-rs + pyo3
zk-trading-sdk-rs ← zk-proto-rs + zk-infra-rs + tonic + async-nats
zk-oms-svc ← zk-oms-rs + zk-infra-rs + zk-trading-sdk-rs (for GW gRPC client) + tonic
zk-engine-svc ← zk-engine-rs + zk-strategy-sdk-rs + zk-trading-sdk-rs + tonic
```

## 3. Crate Responsibilities

### `zk-proto-rs`
- Generated prost + tonic protobuf bindings for all proto packages
- No business logic; pure generated code
- Module aliases for transition: re-exports `zk::oms::v1` etc. alongside legacy `oms`, `tqrpc_oms`

### `zk-domain-rs`
- Core domain enums (`OrderStatus`, `ExecType`, `InstrumentType`, `BuySellType`, etc.)
- State model structs shared across crates
- No infra dependencies

### `zk-oms-rs`
- `OmsCore::process_message` — state machine: processes orders, cancels, gateway reports, rebalancing
- `OrderManager` — order lifecycle and state transitions
- `BalanceManager` — balance/position tracking
- `ConfdataManager` — instrument/risk config loading and lookup
- `utils` — `gen_timestamp_ms`, `round_to_precision`, `gen_order_id` (Snowflake)
- No infra dependencies; transport-agnostic

Known stubs:
- `OmsCore::calc_balance_changes_for_report` returns `None` (Phase 2b)
- No max-size bound on `pending_order_reports` cache
- LRU cleanup only touches `exch_ref_to_order_id`, not `context_cache`

### `zk-strategy-sdk-rs`
- `StrategyBase` trait: required callbacks (`on_tick`, `on_order_update`, `on_balance_update`, timer callbacks)
- `StrategyContext` — injected dependencies for strategies (order submission, query, subscription)
- chrono + cron scheduler support for timer-driven strategies
- No infra dependencies

### `zk-engine-rs`
- Live event loop: event coalescing, batching, priority routing
- Timer manager: cron and interval timers calling strategy callbacks
- Action dispatcher boundary: takes `Vec<StrategyAction>` → calls OMS via injected dispatcher trait
- tokio runtime boundary (only crate besides infra allowed to use tokio directly)

### `zk-backtest-rs`
- Deterministic backtest engine: replay from event queue, matching engine simulation
- Uses `zk-oms-rs` for order lifecycle, `zk-strategy-sdk-rs` for strategy interface
- No infra dependencies (reads from file-based event sources or in-memory queues)

### `zk-pyo3-rs`
- PyO3 bindings exposing `zk-backtest-rs` to Python strategies
- Also provides Python-compatible `trading_sdk` bindings for live Python strategies
- Crate type: `cdylib` (`.so` / `.pyd`) + `rlib`

### `zk-infra-rs`

Modules to implement:

| Module | Responsibility |
|---|---|
| `nats` | async-nats pub/sub helpers, subject builders |
| `nats_kv` | NATS KV registry client: write/heartbeat/watch/evict |
| `grpc` | tonic channel builder with TLS and backoff |
| `redis` | redis-rs wrapper for OMS warm-start cache and read replicas |
| `pg` | sqlx PostgreSQL connection pool and query helpers |
| `mongo` | MongoDB event writer (append-only) |
| `vault` | Vault client: auth, secret fetch, lease refresh |
| `metrics` | Prometheus metrics registry and exporters |
| `tracing` | OpenTelemetry setup and context propagation |
| `dataset` | File-based dataset readers (existing, for backtest) |

No domain logic in `zk-infra-rs`.

### `zk-trading-sdk-rs` (new)

See [07-sdk-design.md](07-sdk-design.md) for full API design.

Modules:
- `client` — `TradingClient` entry point
- `discovery` — NATS KV watcher, account→oms_id→endpoint cache
- `oms` — OMS gRPC channel pool, command/query wrappers
- `stream` — NATS subscription helpers
- `id_gen` — Snowflake order-ID generator
- `model` — SDK domain types
- `config` — env-var config loader

## 4. Service Binary Design

### `zk-oms-svc`

Thin binary wrapping `zk-oms-rs`:
- bootstrap: load config from PostgreSQL via `zk-infra-rs::pg`
- build `OmsCore` instance
- establish gateway gRPC clients; warm-load state from Redis; then query open orders and balances/positions from each gateway for reconciliation
- start tonic gRPC server exposing `zk.oms.v1.OMSService`
- wire NATS subscriber (gateway reports → `OmsCore::process_message`)
- wire NATS publisher (OMS events from action handler)
- register `svc.oms.<oms_id>` in NATS KV via `zk-infra-rs::nats_kv`
- run periodic tasks (order/balance resync every 60s, cleanup every 10 min)

### `zk-engine-svc`

Thin binary wrapping `zk-engine-rs`:
- load `cfg.strategy_instance` from PostgreSQL
- resolve OMS endpoint from NATS KV via `zk-trading-sdk-rs::discovery`
- initialize `TradingClient` from `zk-trading-sdk-rs`
- instantiate strategy (Rust-native or Python subprocess via `zk-pyo3-rs`)
- start engine event loop
- start tonic gRPC server exposing `zk.engine.v1.EngineService`

### `zk-gw-svc` (per venue)

Thin binary per venue:
- fetch account secrets from Vault via `zk-infra-rs::vault`
- initialize venue session (venue-specific connector crate or FFI)
- start tonic gRPC server exposing `zk.gateway.v1.GatewayService`
- publish normalized execution reports and balance updates to NATS
- register `svc.gw.<gw_id>` in NATS KV with heartbeat

## 5. Inter-Component Transport by Deployment Mode

The domain crates (`zk-engine-rs`, `zk-oms-rs`) are transport-agnostic. The service binary wires the transport layer.

| Mode | Engine → OMS | OMS → GW |
|---|---|---|
| 3-layer | gRPC (`tonic` client in `zk-engine-svc`) | gRPC (`tonic` client in `zk-oms-svc`) |
| 2-layer | `tokio::sync::mpsc` channel (in-process, `zk-collocated-svc`) | `GatewayPlugin` trait (in-process call) |
| All-in-one | direct function call / `mpsc` (same thread group) | `GatewayPlugin` trait (direct call) |

**`GatewayPlugin` trait** (to be defined in `zk-infra-rs` or a new `zk-gw-core-rs`):
```rust
#[async_trait]
pub trait GatewayPlugin: Send + Sync {
    async fn place_order(&self, req: GwPlaceOrderRequest) -> Result<GwCommandAck>;
    async fn cancel_order(&self, req: GwCancelOrderRequest) -> Result<GwCommandAck>;
    async fn query_balance(&self, req: GwQueryBalanceRequest) -> Result<GwQueryBalanceResponse>;
    async fn query_order(&self, req: GwQueryOrderRequest) -> Result<GwQueryOrderResponse>;
    // ...
}
```

The same `GatewayPlugin` trait is implemented by:
- the gRPC gateway client (3-layer: network call to `zk-gw-svc`)
- the in-process venue plugin (2-layer/all-in-one: direct function call, zero copy)

This allows `zk-oms-rs` to call gateways without caring about the transport layer.

NATS is still used in 2-layer and all-in-one modes for **external fan-out** (Recorder, Monitor, strategy events) — not for the critical order path.

## 6. OMS Reader/Writer Separation

### Problem

In the Python OMS server, gRPC query handlers (e.g. `QueryOpenOrders`) read directly from the shared `OmsCore` / `OrderManager` state. In a true async multi-threaded runtime this requires locking `OmsCore` on every read — contending with the writer — or routing queries through the command channel, serialising all reads behind writes. Both are unacceptable for latency.

### Design

`OmsCore` is single-writer. After every `process_message` call the writer task produces an immutable `OmsSnapshot` and atomically publishes it to a shared read replica. Query handlers read from the replica without ever touching `OmsCore`.

```
┌────────────────────────────────┐        ┌────────────────────────────────────┐
│  gRPC command handlers         │        │  gRPC query handlers               │
│  PlaceOrder, CancelOrder, ...  │        │  QueryOpenOrders, QueryBalances, ...│
└────────────┬───────────────────┘        └──────────────┬─────────────────────┘
             │ mpsc::Sender<OmsCommand>                   │ arc_swap::load()
             ▼                                            ▼
┌────────────────────────────┐        ┌──────────────────────────────────┐
│  OmsCore writer task       │──────▶ │  ArcSwap<OmsSnapshot>            │
│  (single tokio task)       │ store  │  (shared read replica)           │
│  process_message()         │        │                                  │
│  take_snapshot()           │        │  Arc<OmsSnapshot> per load()     │
└────────────────────────────┘        │  — zero locks, zero contention   │
                                      └──────────────────────────────────┘
```

### `OmsSnapshot` — `zk-oms-rs`

Defined in the domain crate as a pure data type (no infra deps). `arc-swap` is used only in the service binary.

```rust
/// Immutable point-in-time snapshot of OmsCore state.
/// Produced by OmsCore::take_snapshot() after each mutation.
/// Clone is cheap because all collections are cloned once per mutation, not per reader.
#[derive(Clone, Debug)]
pub struct OmsSnapshot {
    /// order_id → order
    pub orders: HashMap<i64, OmsOrder>,
    /// account_id → set of open order_ids (non-terminal)
    pub open_order_ids: HashMap<i64, HashSet<i64>>,
    /// balance key (account_id:asset) → balance
    pub balances: HashMap<String, Balance>,
    /// position key (account_id:instrument:side) → position
    pub positions: HashMap<String, OmsPosition>,
    /// accounts currently in panic mode
    pub panic_accounts: HashSet<i64>,
    /// monotonic counter, incremented on every mutation (for debugging/metrics)
    pub seq: u64,
    pub snapshot_ts_ms: i64,
}

impl OmsCore {
    /// Produce a snapshot of current state. Called by the writer task after every process_message().
    pub fn take_snapshot(&self) -> OmsSnapshot { ... }
}
```

### Service wiring — `zk-oms-svc`

```rust
// Shared between writer task and all gRPC handlers
type ReadReplica = Arc<ArcSwap<OmsSnapshot>>;

// Writer task (single tokio task):
async fn oms_writer_loop(
    mut oms_core: OmsCore,
    mut rx: mpsc::Receiver<OmsCommand>,
    replica: ReadReplica,
    nats: NatsPublisher,
    redis: RedisWriter,
    shutdown: CancellationToken,
) {
    loop {
        tokio::select! {
            Some(cmd) = rx.recv() => {
                let actions = oms_core.process_message(cmd.into());
                // handle actions: dispatch to GW, publish NATS, write Redis ...
                // update read replica atomically
                replica.store(Arc::new(oms_core.take_snapshot()));
                cmd.reply(ack);
            }
            _ = shutdown.cancelled() => break,
        }
    }
}

// gRPC query handler (runs on any tokio worker thread):
async fn query_open_orders(&self, req: Request<...>) -> Result<Response<...>> {
    let snap = self.replica.load();  // atomic pointer load — never blocks writer
    let orders = snap.open_order_ids
        .get(&account_id)
        .map(|ids| ids.iter().filter_map(|id| snap.orders.get(id)).cloned().collect())
        .unwrap_or_default();
    Ok(Response::new(QueryOpenOrdersResponse { orders }))
}
```

`arc-swap` crate: `replica.store(Arc::new(...))` is a single atomic pointer swap. `replica.load()` returns an `Arc<OmsSnapshot>` guard — readers hold a ref-counted pointer to the snapshot at that instant; a subsequent writer swap does not affect in-flight reads.

### Collocated mode benefit

In 2-layer / all-in-one mode, the engine task can directly call `replica.load()` to read OMS state (e.g. open positions) without sending a command through the channel. Only mutations go through the channel. This eliminates a round-trip for read-heavy strategy paths (e.g. on each tick checking current positions).

### Replica staleness

The replica reflects state **after the last completed mutation**. Query results may lag by one in-flight command (e.g. a `PlaceOrder` being processed). This is acceptable — same as the Python OMS behaviour where callers read state after awaiting a command ack.

## 7. Design Constraint

- **No infra imports in domain crates.** `zk-oms-rs`, `zk-engine-rs`, `zk-strategy-sdk-rs`, `zk-backtest-rs` must not import `tonic`, `async-nats`, `redis`, `sqlx`, or `vault`. Only `tokio` is permitted in `zk-engine-rs` for the async runtime boundary.
- **Service binaries stay thin.** Business logic belongs in domain crates; binaries handle only bootstrap, wiring, lifecycle, and observability.
- **`zk-trading-sdk-rs` is the only external client dependency.** Strategies and integrations import only this crate; they do not import `zk-oms-rs` or `zk-infra-rs` directly.
