# Phase 5: Trading SDK

## Goal

Implement `zk-trading-sdk-rs` — the client library that replaces `TQClient` and eliminates ODS dependency. Provides:
- NATS KV-based service discovery (account → OMS endpoint)
- OMS gRPC channel pool with backoff and failover
- Local Snowflake order-ID generation
- NATS subscriptions for order/balance/position/RTMD updates
- Python bindings via PyO3 (`zk-pyo3-rs`)

## Prerequisites

- Phase 1 complete (proto, `zk-infra-rs`)
- Phase 2 complete (OMS gRPC service running in docker-compose)
- Phase 4 complete (NATS KV registry populated; `KvDiscoveryClient` available)

## Deliverables

### 5.1 `zk-trading-sdk-rs` crate

Location: `zkbot/rust/crates/zk-trading-sdk-rs/`

#### Module layout

```
zk-trading-sdk-rs/src/
  lib.rs
  client.rs      -- TradingClient (top-level entry point)
  discovery.rs   -- wraps KvDiscoveryClient; account→OMS resolution + watch
  oms.rs         -- OMS gRPC channel pool + command/query wrappers
  stream.rs      -- NATS subscription helpers (order/balance/position/rtmd)
  id_gen.rs      -- Snowflake order-ID generator
  model.rs       -- SDK domain types (TradingOrder, TradingCancel, OmsOrder, Balance, etc.)
  config.rs      -- TradingClientConfig + from_env()
  error.rs       -- SdkError enum
```

#### `config.rs`

```rust
pub struct TradingClientConfig {
    pub nats_url: String,
    pub env: String,                       // 'prod' | 'staging' | 'dev'
    pub account_ids: Vec<i64>,
    pub client_instance_id: u16,           // 0–1023 for Snowflake
    pub discovery_bucket: String,          // default: "zk.svc.registry.v1"
    pub pilot_grpc: Option<String>,        // for refdata queries
}

impl TradingClientConfig {
    pub fn from_env() -> Result<Self>;     // reads ZK_NATS_URL, ZK_ENV, ZK_ACCOUNT_IDS, etc.
}
```

Env vars:

| Variable | Required | Purpose |
|---|---|---|
| `ZK_NATS_URL` | yes | NATS bootstrap URL |
| `ZK_ENV` | yes | environment namespace |
| `ZK_ACCOUNT_IDS` | yes | comma-separated account IDs |
| `ZK_CLIENT_INSTANCE_ID` | yes | unique Snowflake instance id (0–1023) |
| `ZK_DISCOVERY_BUCKET` | no | override KV bucket (default: `zk.svc.registry.v1`) |
| `ZK_PILOT_GRPC` | no | Pilot gRPC address for refdata queries |

#### `id_gen.rs` — Snowflake generator

```rust
pub struct SnowflakeIdGen {
    instance_id: u16,     // 0–1023 (10 bits)
    sequence: AtomicU16,  // 0–4095 (12 bits), resets per ms
    last_ms: AtomicI64,
}

impl SnowflakeIdGen {
    pub fn new(instance_id: u16) -> Result<Self>;   // returns Err if instance_id > 1023
    pub fn next_id(&self) -> i64;
    // format: timestamp_ms (41 bits) | instance_id (10 bits) | sequence (12 bits)
}
```

#### `discovery.rs` — account → OMS resolution

```rust
pub struct OmsDiscovery {
    inner: KvDiscoveryClient,
    // account_id → (oms_id, grpc_endpoint)
    cache: Arc<RwLock<HashMap<i64, OmsEndpoint>>>,
}

impl OmsDiscovery {
    pub async fn start(config: &TradingClientConfig) -> Result<Self>;
    pub fn resolve_oms(&self, account_id: i64) -> Option<OmsEndpoint>;
    // Called by oms.rs when KV update changes endpoint
    pub fn on_endpoint_change(&self, callback: impl Fn(i64, OmsEndpoint) + Send + 'static);
}
```

Startup sequence:
1. connect to NATS KV `zk.svc.registry.v1`
2. fetch initial snapshot of all `svc.oms.*` entries
3. for each account_id in config: scan entries for `account_ids` membership → cache `account_id → OmsEndpoint`
4. start background watch: on KV update to any `svc.oms.*` key, re-resolve and update cache
5. notify `oms.rs` channel pool of endpoint changes

#### `oms.rs` — gRPC channel pool

```rust
pub struct OmsChannelPool {
    channels: Arc<RwLock<HashMap<String, OmsGrpcClient>>>,  // oms_id → client
}

impl OmsChannelPool {
    pub async fn get_or_connect(&self, endpoint: &OmsEndpoint) -> Result<OmsGrpcClient>;
    pub async fn drain_and_reconnect(&self, oms_id: &str, new_endpoint: OmsEndpoint);
}
```

Retry policy (per channel):
- exponential backoff: base 100ms, max 10s, jitter ±20%
- circuit breaker: open after 5 consecutive failures, half-open probe after 30s

#### `client.rs` — `TradingClient`

Public API:

```rust
// Bootstrap
TradingClient::from_env() -> Result<TradingClient>
TradingClient::from_config(TradingClientConfig) -> Result<TradingClient>

// Order commands (route through OMS gRPC)
client.place_order(account_id: i64, order: TradingOrder) -> Result<CommandAck>
client.cancel_order(account_id: i64, cancel: TradingCancel) -> Result<CommandAck>
client.batch_place_orders(account_id: i64, orders: Vec<TradingOrder>) -> Result<CommandAck>
client.batch_cancel_orders(account_id: i64, cancels: Vec<TradingCancel>) -> Result<CommandAck>

// Order ID generation
client.next_order_id() -> i64

// OMS queries
client.query_open_orders(account_id: i64) -> Result<Vec<OmsOrder>>
client.query_positions(account_id: i64) -> Result<Vec<OmsPosition>>
client.query_balances(account_id: i64) -> Result<Vec<Balance>>

// NATS subscriptions (non-blocking, return JoinHandle)
client.subscribe_order_updates(account_id: i64, handler: impl Fn(OrderUpdateEvent) + Send + 'static) -> JoinHandle
client.subscribe_balance_updates(account_id: i64, asset: &str, handler: impl Fn(BalanceUpdateEvent) + Send + 'static) -> JoinHandle
client.subscribe_position_updates(account_id: i64, instrument: &str, handler: impl Fn(PositionUpdateEvent) + Send + 'static) -> JoinHandle
client.subscribe_ticks(venue: &str, instrument_exch: &str, handler: impl Fn(TickData) + Send + 'static) -> JoinHandle
client.subscribe_klines(venue: &str, instrument_exch: &str, interval: &str, handler: impl Fn(Kline) + Send + 'static) -> JoinHandle
```

`place_order` flow:
1. resolve OMS endpoint for `account_id` via `OmsDiscovery`
2. attach `order_id = client.next_order_id()` if not already set in `TradingOrder`
3. call `OmsGrpcClient::place_order(PlaceOrderRequest{...})`
4. return `CommandAck`

#### `stream.rs` — NATS topic construction

Topic names are deterministic — constructed locally without any lookup:

```rust
fn order_update_topic(oms_id: &str, account_id: i64) -> String {
    format!("zk.oms.{}.order_update.{}", oms_id, account_id)
}
fn balance_update_topic(oms_id: &str, asset: &str) -> String {
    format!("zk.oms.{}.balance_update.{}", oms_id, asset)
}
fn position_update_topic(oms_id: &str, instrument: &str) -> String {
    format!("zk.oms.{}.position_update.{}", oms_id, instrument)
}
fn tick_topic(venue: &str, instrument_exch: &str) -> String {
    format!("zk.rtmd.tick.{}.{}", venue, instrument_exch)
}
```

### 5.2 Python bindings — `zk-pyo3-rs`

Location: `zkbot/rust/crates/zk-pyo3-rs/src/trading_sdk.rs`

Expose `TradingClient` to Python:

```python
from zk_pyo3 import TradingClient

client = TradingClient.from_env()
ack = client.place_order(account_id=9001, order={
    "instrument": "BTC-USDT-PERP",
    "side": "BUY",
    "qty": "0.01",
    "price": "50000",
    "order_type": "LIMIT",
})
```

Python callback subscriptions:
```python
client.subscribe_order_updates(account_id=9001, handler=lambda ev: print(ev))
```

PyO3 wrapper uses `pyo3-asyncio` (tokio runtime) for async bridges.

## Tests

### Unit tests

- `test_snowflake_id_uniqueness`: generate 10_000 IDs in parallel across 4 threads → assert all unique
- `test_snowflake_id_monotonic`: IDs should be monotonically increasing within single thread
- `test_snowflake_instance_id_bounds`: `SnowflakeIdGen::new(1024)` → returns `Err`
- `test_discovery_resolve_by_account`: populate cache with two OMS entries; resolve `account_id=9001` → correct `oms_id`
- `test_order_update_topic_format`: assert `order_update_topic("oms_dev_1", 9001) == "zk.oms.oms_dev_1.order_update.9001"`
- `test_balance_update_topic_format`: assert format is by asset, not instrument

### Integration tests (docker-compose)

- `test_trading_client_from_env_connects`:
  1. start docker-compose stack with OMS and mock-gw
  2. set env vars, call `TradingClient::from_env()`
  3. assert no error; assert OMS endpoint resolved for account 9001
- `test_place_order_roundtrip`:
  1. subscribe to `zk.oms.oms_dev_1.order_update.9001`
  2. call `client.place_order(9001, TradingOrder{...})`
  3. assert `CommandAck{success:true}` returned
  4. assert `OrderUpdateEvent` received on NATS within 500ms
- `test_oms_endpoint_failover`:
  1. start SDK client; confirm connected to OMS
  2. update OMS KV entry with new port
  3. call `place_order` → assert SDK transparently reconnects and succeeds
- `test_subscribe_order_updates_delivery`:
  1. subscribe with handler that appends to `Vec<OrderUpdateEvent>`
  2. place order via mock-gw → fill → assert handler called with `FILLED` event
- `test_python_binding_place_order` (pytest):
  1. `client = TradingClient.from_env()`
  2. `ack = client.place_order(account_id=9001, order={...})`
  3. assert `ack["success"] == True`

### Parity tests

- `test_sdk_parity_place_order`: submit same order via `zk-trading-sdk-rs` and Python TQClient; assert `CommandAck` shape equivalent
- `test_sdk_parity_order_id_uniqueness`: generate 1000 IDs in Python (legacy) and Rust; assert no overlap when same instance_id used

## Exit criteria

- [ ] `cargo build -p zk-trading-sdk-rs` succeeds
- [ ] `cargo build -p zk-pyo3-rs` with trading_sdk bindings succeeds
- [ ] unit tests pass: `cargo test -p zk-trading-sdk-rs`
- [ ] integration test `test_place_order_roundtrip` passes against docker-compose stack
- [ ] `test_oms_endpoint_failover` demonstrates transparent channel swap
- [ ] Python binding `test_python_binding_place_order` passes
- [ ] `TradingClient::from_env()` with missing required env var returns descriptive error (not panic)
- [ ] No ODS call in any path — confirmed by absence of `tq.ods.rpc` in all test traces
- [ ] Snowflake ID generation: 10_000 IDs across 4 threads, zero duplicates
