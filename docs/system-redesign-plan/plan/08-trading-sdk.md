# Phase 7: Trading SDK

## Goal

Implement `zk-trading-sdk-rs` — the client library that replaces `TQClient` and eliminates ODS dependency. Provides:
- NATS KV-based service discovery (account → OMS endpoint, refdata endpoint) from the shared registry bucket
- `RefdataSdk` component: gRPC client, in-memory cache, NATS invalidation subscription
- OMS gRPC channel pool with backoff and failover
- Local Snowflake order-ID generation
- NATS subscriptions for order/balance/position/RTMD updates
- RTMD interest lease helpers + refdata-backed subject mapping
- Python bindings via PyO3 (`zk-pyo3-rs`)

## Prerequisites

- Phase 1 complete (proto, `zk-infra-rs`)
- Phase 3 complete (OMS gRPC service running in docker-compose)
- Phase 5 complete (NATS KV registry populated; `KvDiscoveryClient` available)
- Phase 6 complete (`zk-refdata-svc` running; `svc.refdata.refdata_dev_1` registered in KV; Pilot scaffold at `POST /v1/strategy-executions/start`)
- The Pilot scaffold is the production source of `instance_id` (returned in `enriched_config`); the engine calls it before constructing `TradingClient`. `ZK_CLIENT_INSTANCE_ID` is a dev/test fallback only.

## Deliverables

### 7.1 `zk-trading-sdk-rs` crate

Location: `zkbot/rust/crates/zk-trading-sdk-rs/`

#### Module layout

```
zk-trading-sdk-rs/src/
  lib.rs
  client.rs      -- TradingClient (top-level entry point)
  discovery.rs   -- KvDiscoveryClient wrapper; account→OMS and refdata endpoint resolution
  oms.rs         -- OMS gRPC channel pool + command/query wrappers
  refdata.rs     -- RefdataSdk: endpoint discovery, gRPC client, in-memory cache, invalidation
  rtmd_sub.rs    -- RTMD interest lease helpers + refdata-backed subject construction
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
    pub env: String,                        // 'prod' | 'staging' | 'dev'
    pub account_ids: Vec<i64>,
    pub client_instance_id: u16,            // 0–1023 for Snowflake; production: from Pilot enriched_config
    pub discovery_bucket: String,           // default: "zk-svc-registry-v1" (dashes — NATS KV constraint)
    pub refdata_grpc: Option<String>,       // override refdata gRPC address; otherwise discovered from KV
}

impl TradingClientConfig {
    pub fn from_env() -> Result<Self>;      // reads ZK_NATS_URL, ZK_ENV, ZK_ACCOUNT_IDS, etc.
}
```

Env vars:

| Variable | Required | Purpose |
|---|---|---|
| `ZK_NATS_URL` | yes | NATS bootstrap URL |
| `ZK_ENV` | yes | environment namespace |
| `ZK_ACCOUNT_IDS` | yes | comma-separated account IDs |
| `ZK_CLIENT_INSTANCE_ID` | dev/test only | Snowflake instance id (0–1023); production source is Pilot enriched_config |
| `ZK_DISCOVERY_BUCKET` | no | override KV bucket (default: `zk-svc-registry-v1`) |
| `ZK_REFDATA_GRPC` | no | refdata gRPC address override (skip KV discovery) |

**Note on bucket name:** The implementation uses `zk-svc-registry-v1` (dashes). Architecture docs `api_contracts.md` and `sdk.md` still show `zk.svc.registry.v1` (dots), which is NATS-incompatible — NATS KV bucket names cannot contain dots. This discrepancy is tracked in `TODO.md § KV Bucket Naming`. Code must use dashes.

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

**`instance_id` is Pilot-granted.** The engine calls `POST /v1/strategy-executions/start` before constructing `TradingClient`, and Pilot returns `execution_id` + `enriched_config`. `enriched_config.instance_id` is the authoritative source for `SnowflakeIdGen` — Pilot enforces singleton policy and ensures uniqueness via `cfg.instance_id_lease`. The engine extracts the value and passes it into `TradingClientConfig::client_instance_id`.

`ZK_CLIENT_INSTANCE_ID` env var is a **dev/test fallback only** — used when `TradingClient::from_env()` is called directly (e.g., integration tests, local bench runs) without a preceding Pilot start call. It must not be used in production. `from_env()` should fail fast with a clear error if neither enriched_config nor `ZK_CLIENT_INSTANCE_ID` provides a value.

#### `discovery.rs` — registry-backed endpoint resolution

```rust
pub struct OmsDiscovery {
    inner: KvDiscoveryClient,
    watch: JoinHandle<()>,
    // account_id -> current OMS endpoint
    cache: Arc<RwLock<HashMap<i64, OmsEndpoint>>>,
}

impl OmsDiscovery {
    pub async fn start(config: &TradingClientConfig) -> Result<Self>;
    pub async fn resolve_oms(&self, account_id: i64) -> Option<OmsEndpoint>;
    pub async fn snapshot(&self) -> HashMap<i64, OmsEndpoint>;
    // Scans svc.refdata.* KV entries; returns first live grpc_endpoint
    pub async fn resolve_refdata(&self) -> Option<String>;
}
```

OMS resolution rules:

- only `svc.oms.*` entries are considered
- KV presence is the liveness signal; stale Pilot DB rows are irrelevant to the SDK
- the endpoint is derived from the service registration payload, typically `transport.authority` or `transport.address`
- if multiple live OMS registrations claim the same account, treat that as an error and surface `SdkError::DiscoveryConflict`

Refdata resolution rules:

- only `svc.refdata.*` entries are considered
- if `ZK_REFDATA_GRPC` is set, skip KV discovery and use the override
- if no refdata entry is present and no override, **fail startup** — the shared refdata service is expected; cache-only mode is not valid at initial startup (see `refdata_loader_service.md` §Cache Distribution To SDK)

Startup sequence for `OmsDiscovery`:
1. connect to NATS JetStream and open `KvDiscoveryClient` on `zk-svc-registry-v1`
2. call `OmsDiscovery::start()` — **snapshot-ready on return** (accepted architecture rule per `reconcile.md` §4); no separate `spawn_watch_loop()` call or explicit wait required by caller
3. scan all `svc.oms.*` registrations from the initial snapshot; build `account_id -> OmsEndpoint`
4. keep a background task that rebuilds the account map on endpoint churn
5. notify `oms.rs` channel pool when an OMS endpoint for any configured account changes

`KvDiscoveryClient::start()` is responsible for delivering the initial snapshot before returning. The SDK must not encode the old empty-then-wait contract.

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

Discovery interaction:

- `OmsChannelPool` keys channels by logical OMS identity, not raw endpoint
- when discovery reports endpoint churn for an existing `oms_id`, the old channel is drained and replaced
- if discovery temporarily has no endpoint for an account, order/query APIs fail fast with a discovery error rather than hanging

#### `refdata.rs` — `RefdataSdk`

`TradingClient` holds a `RefdataSdk` instance and delegates all instrument/market-status lookups through it.

```rust
pub struct RefdataSdk {
    grpc: RefdataGrpcClient,                      // required; startup fails if no refdata service found
    cache: Arc<RwLock<RefdataCache>>,
    _invalidation_task: JoinHandle<()>,
    _market_status_task: JoinHandle<()>,
}

struct RefdataCache {
    instruments: HashMap<String, CachedEntry<InstrumentRefdata>>,   // instrument_id → entry
    by_venue_symbol: HashMap<(String, String), String>,             // (venue, instrument_exch) → instrument_id
    market_status: HashMap<(String, String), CachedEntry<MarketStatus>>,  // (venue, market) → entry
}

struct CachedEntry<T> {
    data: T,
    valid: bool,       // false after invalidation, until reload completes
    loaded_at_ms: i64,
}

impl RefdataSdk {
    // grpc_endpoint is required; returns Err if not provided or connection fails
    pub async fn start(
        nc: &nats::Client,
        grpc_endpoint: String,
    ) -> Result<Self>;

    // On-demand lookup — checks cache first, fetches from gRPC on miss or invalid entry
    pub async fn query_instrument(
        &self,
        instrument_id: &str,
    ) -> Result<InstrumentRefdata>;

    pub async fn query_by_venue_symbol(
        &self,
        venue: &str,
        instrument_exch: &str,
    ) -> Result<InstrumentRefdata>;

    pub async fn query_market_status(
        &self,
        venue: &str,
        market: &str,
    ) -> Result<MarketStatus>;
}
```

Startup sequence:
1. connect the gRPC channel to `grpc_endpoint`; return `Err` if connection fails — refdata is a required shared service
2. spawn `_invalidation_task`: subscribe to `zk.control.refdata.updated`; on receipt, mark the named entry `valid=false` and trigger async reload
3. spawn `_market_status_task`: subscribe to `zk.control.market_status.updated`; on receipt, update `market_status` cache inline (market status events carry the new state directly)
4. do NOT bulk-preload — populate cache on demand

Cache behavior:

- `query_instrument` checks `instruments` cache; if entry missing or `valid=false`, calls `gRPC.QueryInstrumentById` and upserts into cache
- **temporary outage after warmup** (gRPC unreachable, stale entry exists): return the stale entry with `SdkError::RefdataStale`; caller decides how to react — this is the only case where stale data is served
- **no cached entry and gRPC unavailable**: return `SdkError::RefdataNotAvailable` — do not start with an empty cache and pretend the service is optional
- `by_venue_symbol` is populated as a secondary index on each successful gRPC fetch; `query_by_venue_symbol` resolves through this index

Invalidation flow (`zk.control.refdata.updated` payload):

```json
{
  "event_type": "refdata_updated",
  "instrument_id": "BTCUSDT_MOCK",
  "venue": "MOCK",
  "change_class": "invalidate",   // "invalidate" | "deprecate"
  "watermark": 12345,
  "updated_at_ms": 1712000000000
}
```

- `invalidate`: set `valid=false`; spawn reload task to fetch fresh entry from gRPC
- `deprecate`: set `valid=false`; do not auto-reload; return `SdkError::RefdataDeprecated` on next lookup

Market-status flow (`zk.control.market_status.updated` payload):

```json
{
  "event_type": "market_status_updated",
  "venue": "MOCK",
  "market": "MOCK",
  "session_state": "open",
  "effective_at_ms": 1712000000000,
  "updated_at_ms": 1712000000000
}
```

The `_market_status_task` updates the `market_status` cache inline without an additional gRPC call.

#### `rtmd_sub.rs` — RTMD interest lease helpers

RTMD subject construction requires venue-native fields resolved from refdata. `rtmd_sub.rs` wraps the lookup and subject formatting so `client.rs` stays clean.

```rust
pub struct RtmdInterestManager {
    nc: nats::Client,
    refdata: Arc<RefdataSdk>,
}

impl RtmdInterestManager {
    // Resolve instrument_id → (venue, instrument_exch) from RefdataSdk, then:
    // 1. publish RTMD interest lease to zk.rtmd.subs.v1
    // 2. subscribe to the deterministic NATS subject
    pub async fn subscribe_ticks(
        &self,
        instrument_id: &str,
        handler: impl Fn(TickData) + Send + 'static,
    ) -> Result<JoinHandle<()>>;

    pub async fn subscribe_klines(
        &self,
        instrument_id: &str,
        interval: &str,
        handler: impl Fn(Kline) + Send + 'static,
    ) -> Result<JoinHandle<()>>;

    pub async fn subscribe_funding(
        &self,
        instrument_id: &str,
        handler: impl Fn(FundingRate) + Send + 'static,
    ) -> Result<JoinHandle<()>>;

    pub async fn subscribe_orderbook(
        &self,
        instrument_id: &str,
        handler: impl Fn(OrderBook) + Send + 'static,
    ) -> Result<JoinHandle<()>>;
}

// Deterministic subject construction (no service-discovery lookup required after refdata resolved)
pub fn tick_topic(venue: &str, instrument_exch: &str) -> String {
    format!("zk.rtmd.tick.{}.{}", venue, instrument_exch)
}
pub fn kline_topic(venue: &str, instrument_exch: &str, interval: &str) -> String {
    format!("zk.rtmd.kline.{}.{}.{}", venue, instrument_exch, interval)
}
pub fn funding_topic(venue: &str, instrument_exch: &str) -> String {
    format!("zk.rtmd.funding.{}.{}", venue, instrument_exch)
}
pub fn orderbook_topic(venue: &str, instrument_exch: &str) -> String {
    format!("zk.rtmd.orderbook.{}.{}", venue, instrument_exch)
}
```

Interest lease format (published to `zk.rtmd.subs.v1`):
```json
{
  "instrument_id": "BTCUSDT_MOCK",
  "venue": "MOCK",
  "instrument_exch": "BTC-USDT",
  "data_types": ["tick"],
  "client_id": "engine_dev_1",
  "lease_until_ms": 1712000060000
}
```

The RTMD gateway watches `zk.rtmd.subs.v1` and activates/deactivates venue feed subscriptions based on live interest leases. See `rtmd_subscription_protocol.md` for the full contract.

#### `stream.rs` — OMS topic construction

Topic names for OMS event streams (no refdata lookup needed):

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
```

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

// NATS subscriptions — OMS (non-blocking, return JoinHandle)
client.subscribe_order_updates(account_id: i64, handler: impl Fn(OrderUpdateEvent) + Send + 'static) -> JoinHandle
client.subscribe_balance_updates(account_id: i64, asset: &str, handler: impl Fn(BalanceUpdateEvent) + Send + 'static) -> JoinHandle
client.subscribe_position_updates(account_id: i64, instrument: &str, handler: impl Fn(PositionUpdateEvent) + Send + 'static) -> JoinHandle

// RTMD subscriptions — resolved via refdata then RTMD interest lease
// instrument_id is the ZK canonical ID; SDK resolves venue-native fields from RefdataSdk
client.subscribe_ticks(instrument_id: &str, handler: impl Fn(TickData) + Send + 'static) -> Result<JoinHandle>
client.subscribe_klines(instrument_id: &str, interval: &str, handler: impl Fn(Kline) + Send + 'static) -> Result<JoinHandle>
client.subscribe_funding(instrument_id: &str, handler: impl Fn(FundingRate) + Send + 'static) -> Result<JoinHandle>
client.subscribe_orderbook(instrument_id: &str, handler: impl Fn(OrderBook) + Send + 'static) -> Result<JoinHandle>

// RefdataSdk access
client.refdata() -> &RefdataSdk
```

`place_order` flow:
1. resolve OMS endpoint for `account_id` via `OmsDiscovery`
2. attach `order_id = client.next_order_id()` if not already set in `TradingOrder`
3. call `OmsGrpcClient::place_order(PlaceOrderRequest{...})`
4. return `CommandAck`

`subscribe_ticks` flow:
1. call `refdata.query_instrument(instrument_id)` to get `(venue, instrument_exch)`
2. delegate to `RtmdInterestManager::subscribe_ticks(instrument_id, handler)` which publishes lease and subscribes

`from_config()` startup sequence:

1. connect to NATS
2. call `OmsDiscovery::start()` — returns snapshot-ready; no separate wait step needed
3. resolve OMS endpoints for configured accounts from snapshot; fail fast if any unresolved
4. construct `OmsChannelPool`
5. resolve refdata endpoint from KV (`svc.refdata.*`) or `ZK_REFDATA_GRPC` override; fail fast if none found
6. call `RefdataSdk::start(grpc_endpoint)` — connects gRPC; subscribes to invalidation/market-status; does NOT bulk-preload
7. construct `RtmdInterestManager`
8. initialize `SnowflakeIdGen` from `config.client_instance_id`
   — production: value from `enriched_config.instance_id` returned by Pilot `POST /v1/strategy-executions/start`
   — dev/test: from `ZK_CLIENT_INSTANCE_ID` env var (fallback only)
9. return a ready `TradingClient`

If discovery cannot resolve any OMS for a required account during bootstrap, return a descriptive startup error.

### 7.2 Python bindings — `zk-pyo3-rs`

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

# Refdata access
rd = client.refdata().query_instrument("BTCUSDT_MOCK")
print(rd["price_tick_size"])
```

Python callback subscriptions:
```python
client.subscribe_order_updates(account_id=9001, handler=lambda ev: print(ev))
client.subscribe_ticks("BTCUSDT_MOCK", handler=lambda tick: print(tick))
```

PyO3 wrapper uses `pyo3-asyncio` (tokio runtime) for async bridges.

## Tests

### Unit tests — Snowflake ID

- `test_snowflake_id_uniqueness`: generate 10_000 IDs in parallel across 4 threads → assert all unique
- `test_snowflake_id_monotonic`: IDs should be monotonically increasing within single thread
- `test_snowflake_instance_id_bounds`: `SnowflakeIdGen::new(1024)` → returns `Err`

### Unit tests — OMS discovery

- `test_discovery_resolve_by_account`: populate discovery snapshot with two OMS entries; resolve `account_id=9001` → correct `oms_id`
- `test_discovery_ignores_non_oms_entries`: registry contains `svc.gw.*`, `svc.refdata.*`, `svc.engine.*` → ignored by OMS resolution path
- `test_discovery_conflict_on_duplicate_account_owner`: two live OMS entries advertise the same account → returns `SdkError::DiscoveryConflict`
- `test_discovery_endpoint_change_rebuilds_account_cache`: changed OMS transport updates resolved endpoint
- `test_from_config_fails_when_required_account_unresolved`: configured account has no live OMS registration → startup error

### Unit tests — topic construction

- `test_order_update_topic_format`: assert `order_update_topic("oms_dev_1", 9001) == "zk.oms.oms_dev_1.order_update.9001"`
- `test_balance_update_topic_format`: assert format is by asset, not instrument
- `test_tick_topic_format`: assert `tick_topic("MOCK", "BTC-USDT") == "zk.rtmd.tick.MOCK.BTC-USDT"`
- `test_kline_topic_format`: assert `kline_topic("MOCK", "BTC-USDT", "1m") == "zk.rtmd.kline.MOCK.BTC-USDT.1m"`

### Unit tests — RefdataSdk

- `test_refdata_cache_miss_fetches_from_grpc`: cache empty; `query_instrument("BTCUSDT_MOCK")` triggers gRPC call and populates cache
- `test_refdata_cache_hit_skips_grpc`: warm cache; `query_instrument` returns from cache without gRPC call
- `test_refdata_invalidation_marks_entry_stale`: receive `zk.control.refdata.updated` for `BTCUSDT_MOCK` with `change_class=invalidate` → entry marked `valid=false`; next query triggers reload
- `test_refdata_deprecation_no_auto_reload`: receive `change_class=deprecate` → entry marked `valid=false`; next query returns `SdkError::RefdataDeprecated`, no gRPC call
- `test_refdata_stale_entry_returned_when_grpc_unavailable`: entry is invalid but gRPC unreachable → returns stale data with `SdkError::RefdataStale` rather than a hard failure
- `test_refdata_by_venue_symbol_uses_secondary_index`: query by `(venue, instrument_exch)` resolves through secondary index built on first lookup
- `test_market_status_updated_event_updates_cache_inline`: receive `zk.control.market_status.updated` → cache entry updated without gRPC call
- `test_refdata_discovery_scans_svc_refdata_prefix`: discovery snapshot with `svc.refdata.refdata_dev_1` → `resolve_refdata()` returns correct `grpc_endpoint`
- `test_refdata_discovery_skipped_when_grpc_override_set`: `ZK_REFDATA_GRPC` set → KV scan not performed

### Integration tests (docker-compose)

- `test_trading_client_from_env_connects`:
  1. start docker-compose stack with OMS, mock-gw, and refdata-svc
  2. set env vars, call `TradingClient::from_env()`
  3. assert no error; assert OMS endpoint resolved for account 9001; assert `RefdataSdk` started
- `test_trading_client_waits_for_registry_snapshot`:
  1. start SDK before OMS registration is present
  2. register OMS into KV shortly after
  3. assert SDK becomes ready only after the OMS entry appears
- `test_refdata_query_via_sdk`:
  1. call `client.refdata().query_instrument("BTCUSDT_MOCK")`
  2. assert `price_tick_size == 0.01`; assert `venue == "MOCK"`
  3. call again → assert second call served from cache (no gRPC call)
- `test_refdata_market_status_stub`:
  1. call `client.refdata().query_market_status("MOCK", "MOCK")`
  2. assert `session_state == "closed"` (Phase 6 stub)
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
- `test_python_binding_refdata_query` (pytest):
  1. `rd = client.refdata().query_instrument("BTCUSDT_MOCK")`
  2. assert `rd["venue"] == "MOCK"` and `rd["price_tick_size"] == 0.01`

### Parity tests

- `test_sdk_parity_place_order`: submit same order via `zk-trading-sdk-rs` and Python TQClient; assert `CommandAck` shape equivalent
- `test_sdk_parity_order_id_uniqueness`: generate 1000 IDs in Python (legacy) and Rust; assert no overlap when same instance_id used

## Exit criteria

- [ ] `cargo build -p zk-trading-sdk-rs` succeeds
- [ ] `cargo build -p zk-pyo3-rs` with trading_sdk bindings succeeds
- [ ] unit tests pass: `cargo test -p zk-trading-sdk-rs`
- [ ] `test_refdata_cache_miss_fetches_from_grpc` and `test_refdata_invalidation_marks_entry_stale` pass
- [ ] `test_refdata_stale_entry_returned_when_grpc_unavailable` demonstrates graceful degradation
- [ ] integration test `test_refdata_query_via_sdk` passes against docker-compose stack (refdata-svc running)
- [ ] integration test `test_place_order_roundtrip` passes against docker-compose stack
- [ ] `test_oms_endpoint_failover` demonstrates transparent channel swap
- [ ] `TradingClient::from_config()` fails clearly when a configured account has no live OMS registration
- [ ] RTMD subscribe APIs resolve instrument→venue mapping from `RefdataSdk` before constructing subjects
- [ ] Python binding `test_python_binding_place_order` and `test_python_binding_refdata_query` pass
- [ ] `TradingClient::from_env()` with missing required env var returns descriptive error (not panic)
- [ ] No ODS call in any path — confirmed by absence of `tq.ods.rpc` in all test traces
- [ ] Snowflake ID generation: 10_000 IDs across 4 threads, zero duplicates
- [ ] `ZK_CLIENT_INSTANCE_ID` absent AND no `enriched_config.instance_id` → descriptive startup error
- [ ] `TradingClient::from_config()` with pilot-granted `instance_id` (from enriched_config) passes integration test alongside dev-mode env var path

## Deferred (not in this phase)

- **Full Pilot token bootstrap** (`topology_registration.md` §3): production path will use `zk.bootstrap.register` NATS request/reply + signed token + `registration_grant`. Phase 7 Pilot scaffold uses a simplified REST call; Phase 10 replaces with the full token flow. `instance_id` sourcing from enriched_config is already the design.
- RTMD interest lease renewal — current phase publishes lease once; production needs periodic refresh
- RTMD gateway registration watching for session/lease expiry callbacks
- PyO3 zero-copy optimization for protobuf payloads via shared memory
- gRPC TLS channel support (current plan assumes insecure for dev stack)
- Circuit breaker per-channel telemetry and alerting hooks
