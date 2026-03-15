# ZKBot trading_sdk Design

`trading_sdk` replaces `TQClient` (`libs/zk-client/src/zk_client/tqclient.py`) and eliminates all ODS dependency.

## 1. Problem with TQClient + ODS

Current `TQClient.create_client()` startup:
1. calls `ODS.query_client_config(account_ids)` — single ODS endpoint `tq.ods.rpc`
2. ODS returns per-account OMS NATS RPC subjects and event topics
3. TQClient wires subscriptions and RPC stubs from those results

Problems:
- ODS is a SPOF: if ODS is down, no client can start
- ODS is a global singleton: all clients block on it at startup
- Static NATS subjects hard-coded in ODS MongoDB config
- Refdata and channel info also flow through ODS, increasing blast radius

## 2. Rust crate — `zk-trading-sdk-rs`

### 2.1 Module layout

```
zk-trading-sdk-rs/
  src/
    client.rs      -- TradingClient (top-level entry point)
    discovery.rs   -- NATS KV watcher, endpoint cache, account→oms resolution
    oms.rs         -- OMS gRPC channel pool + command/query wrappers
    stream.rs      -- NATS subscription helpers (order/balance/rtmd)
    rtmd_sub.rs    -- RTMD interest lease helpers + subject mapping
    refdata.rs     -- RefdataSdk client/cache/invalidation logic
    id_gen.rs      -- local Snowflake order-ID generator (replaces ODS gen_order_id)
    model.rs       -- SDK domain types wrapping proto types
    config.rs      -- TradingClientConfig, env-var loader
```

Note: `auth.rs` is intentionally excluded. SDK does not manage Vault or secret retrieval.

### 2.2 API surface

```rust
// Bootstrap
TradingClient::from_env() -> Result<TradingClient>
TradingClient::from_config(TradingClientConfig) -> Result<TradingClient>

// Order commands (routes through OMS gRPC)
client.place_order(account_id: i64, order: TradingOrder) -> Result<CommandAck>
client.cancel_order(account_id: i64, cancel: TradingCancel) -> Result<CommandAck>
client.batch_place_orders(account_id: i64, orders: Vec<TradingOrder>) -> Result<CommandAck>
client.batch_cancel_orders(account_id: i64, cancels: Vec<TradingCancel>) -> Result<CommandAck>

// OMS queries
client.query_open_orders(account_id: i64) -> Result<Vec<OmsOrder>>
client.query_positions(account_id: i64) -> Result<Vec<OmsPosition>>
client.query_balances(account_id: i64) -> Result<Vec<Balance>>

// NATS subscriptions
client.subscribe_order_updates(handler: impl Fn(OrderUpdateEvent) + Send + 'static) -> JoinHandle
client.subscribe_balance_updates(asset: &str, handler: impl Fn(BalanceUpdateEvent) + Send + 'static) -> JoinHandle
client.subscribe_position_updates(instrument: &str, handler: impl Fn(PositionUpdateEvent) + Send + 'static) -> JoinHandle
client.subscribe_ticks(instrument_id: &str, handler: impl Fn(TickData) + Send + 'static) -> JoinHandle
client.subscribe_klines(instrument_id: &str, interval: &str, handler: impl Fn(Kline) + Send + 'static) -> JoinHandle
client.subscribe_funding(instrument_id: &str, handler: impl Fn(FundingRate) + Send + 'static) -> JoinHandle
client.subscribe_orderbook(instrument_id: &str, handler: impl Fn(OrderBook) + Send + 'static) -> JoinHandle

// RefdataSdk access
client.refdata() -> &RefdataSdk

// RefdataSdk
refdata.query_instrument_refdata(instrument_id: &str) -> Result<InstrumentRefdata>
refdata.query_market_status(venue: &str, market: &str) -> Result<MarketStatus>
```

RTMD subscriptions should follow the dedicated RTMD subscription protocol, not the service
discovery protocol. See [rtmd_subscription_protocol.md](rtmd_subscription_protocol.md).

Client-facing RTMD APIs should use ZK symbol conventions (`instrument_id`) as the primary input.
The SDK resolves venue-native transport fields from refdata before constructing NATS subjects.

Refdata lookup should use a dedicated `RefdataSdk` inside the SDK stack rather than ad hoc direct
PostgreSQL or mixed client logic.

SDK note:

- hot-path SDK lookups should use the compact/canonical refdata disclosure level rather than the
  richest expanded view
- refdata should be loaded on demand rather than bulk-preloaded by default

### 2.2A RefdataSdk

`TradingClient` should use a dedicated `RefdataSdk` component directly.

Responsibilities of `RefdataSdk`:

- resolve the shared refdata service endpoint
- issue gRPC refdata queries
- maintain the local in-memory refdata and market-status cache
- subscribe to refdata invalidation/deprecation topics
- perform active reload after invalidation

Why split it out:

- keeps `TradingClient` focused on OMS/RTMD user flows
- keeps refdata cache and invalidation behavior reusable
- allows a future standalone refdata-oriented SDK/client without redesigning the trading client

### 2.3 Startup behavior

Replaces `TQClient.create_client()` multi-step ODS dance:

1. connect to NATS at `ZK_NATS_URL`
2. fetch and cache initial `zk-svc-registry-v1` KV snapshot
3. for each account_id in `ZK_ACCOUNT_IDS`:
   - scan `svc.oms.*` KV entries for entries whose `account_ids` contain the account
   - map `account_id → oms_id → gRPC endpoint`
4. establish gRPC channels to all resolved OMS endpoints (TLS)
5. subscribe to `zk.oms.<oms_id>.order_update.<account_id>` and `zk.oms.<oms_id>.balance_update.*` per account
6. start KV watch loop: on endpoint change, transparently swap gRPC channel

No HTTP or NATS RPC call to ODS at any point.

For RTMD:

1. accept a client RTMD interest in terms of `instrument_id`
2. resolve `instrument_id -> (venue, instrument_exch)` from refdata
3. build the deterministic RTMD subject from the resolved transport fields
4. publish or refresh an RTMD interest lease in `zk.rtmd.subs.v1`
5. subscribe directly to the deterministic `zk.rtmd.*` subject

No Pilot lookup or service-discovery lookup is needed for subject construction.

For refdata via `RefdataSdk`:

1. resolve the refdata service endpoint from `svc.refdata.*` if a shared service is deployed
2. load refdata through the refdata gRPC service into an in-memory cache
3. subscribe to `zk.control.refdata.updated`
4. mark affected cached entries invalid or deprecated by `instrument_id`, venue, or watermark
5. actively reload the affected canonical entries from the refdata gRPC service

The SDK should maintain a local refdata cache because subject mapping and symbol resolution are
runtime-hot operations.

`TradingClient` should call into `RefdataSdk` rather than owning separate refdata query/cache logic.

### 2.4 Order ID generation and instance_id registration

Order IDs are generated by the **client** (strategy via trading_sdk, UI, or direct API). OMS validates uniqueness but does not generate IDs.

`id_gen.rs` provides a Snowflake generator:
```
order_id = timestamp_ms (41 bits) | instance_id (10 bits) | sequence (12 bits)
```
- `instance_id`: a unique integer (0–1023) assigned per running strategy/client instance
- `timestamp_ms`: millisecond epoch
- `sequence`: per-ms counter, resets each ms

**instance_id assignment and registration:**

`instance_id` is passed at process startup via `ZK_CLIENT_INSTANCE_ID` env var or CLI arg. The operator is responsible for assigning unique values per environment. To prevent collisions:

1. Pilot maintains a simple `cfg.instance_id_lease` table keyed by `(env, instance_id)` with `logical_id` and `leased_until` timestamp
2. On engine startup (the `POST /v1/strat/strategy-inst` call), Pilot also validates and records the claimed `instance_id` — rejecting if another active instance holds it
3. On engine shutdown (the `DELETE /v1/strat/strategy-inst` call), Pilot releases the lease
4. Leases auto-expire after a configurable TTL (e.g. 5 min) if the engine crashes without deregistering

This gives uniqueness guarantees without requiring a separate allocation RPC.

```sql
-- In cfg schema
create table cfg.instance_id_lease (
  env          text not null,
  instance_id  int not null,             -- 0–1023 Snowflake worker id
  logical_id   text not null,
  leased_until timestamptz not null,
  primary key (env, instance_id)
);
```

### 2.5 Minimal config (env vars)

| Variable | Required | Purpose |
|---|---|---|
| `ZK_NATS_URL` | yes | NATS bootstrap URL |
| `ZK_ENV` | yes | environment namespace (`prod`, `staging`, `dev`) |
| `ZK_ACCOUNT_IDS` | yes | comma-separated account IDs |
| `ZK_CLIENT_INSTANCE_ID` | yes | unique Snowflake instance id for order-id generation |
| `ZK_DISCOVERY_BUCKET` | no | override KV bucket name (default: `zk-svc-registry-v1`) |
| `ZK_REFDATA_GRPC` | no | Refdata gRPC address override |

### 2.6 Retry and failover

- gRPC channels use exponential backoff (base 100ms, max 10s, jitter)
- on NATS KV endpoint update: drain in-flight requests on old channel, connect to new endpoint
- circuit breaker per OMS gRPC channel: open after N consecutive failures, half-open probe after timeout
- NATS subscriptions: at-least-once with client-side idempotency on `correlation_id`

Refdata cache behavior:

- reads should use the local SDK cache when valid
- cache misses and invalidated entries should be loaded from the refdata gRPC service
- `zk.control.refdata.updated` should trigger targeted invalidation or deprecation, followed by SDK
  initiated refresh
- if the refdata service is temporarily unavailable, existing cache entries may continue to serve
  reads until TTL or explicit invalidation policy forces refresh
- if data is stale, the caller decides how to react; the SDK should not delete a mapping only
  because freshness degraded

Market-status cache behavior:

- TradFi market open/close status should also be loaded from the refdata gRPC service
- `zk.control.market_status.updated` should trigger targeted invalidation or refresh for affected
  venue/market session state
- strategy or OMS session-gating logic should not independently invent market calendar truth when the
  refdata service is available

## 3. Python SDK

Two options:

**Option A (preferred) — PyO3 binding** (`zk-pyo3-rs` extended):
- expose `TradingClient` to Python via PyO3
- provides same async API surface as Rust
- used by Python strategies running in `zk-engine-rs` Python worker mode
- zero-copy for protobuf payloads via shared memory where possible

**Option B — thin Python gRPC wrapper**:
- generate Python stubs from `zk.oms.v1` protos
- Python wrapper handles KV discovery via `nats.py` and `grpcio`
- simpler to maintain but higher overhead than PyO3

Migration path from `TQClient`:
- `TQClient.create_client()` → `TradingClient.from_env()`
- `TQClient.place_order(order)` → `client.place_order(account_id, order)`
- `TQClient.cancel_order(cancel)` → `client.cancel_order(account_id, cancel)`
- Order/balance update callbacks: same shape, wired via `subscribe_*` instead of channel setup in `TQClient`
