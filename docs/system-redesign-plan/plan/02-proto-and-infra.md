# Phase 1: Proto Migration + zk-infra-rs Foundation

## Goal

Migrate proto packages to `zk.*` versioned naming and build the core infrastructure modules in `zk-infra-rs` that all service binaries depend on.

## Prerequisites

- Phase 0 complete (`make dev-up` working)

## Deliverables

### 1.1 Proto package migration

Location: `zkbot/protos/`

Steps:
1. Create new proto files under `zk/` namespace, one directory per package:
   - `protos/zk/common/v1/common.proto` — `CommandAck`, `AuditMeta`, `ErrorStatus`, `ControlCommand`, etc.
   - `protos/zk/discovery/v1/discovery.proto` — `ServiceRegistration`, `TransportEndpoint`
   - `protos/zk/oms/v1/oms.proto` — domain types (migrate from `oms/` + `tqrpc_oms/`)
   - `protos/zk/oms/v1/oms_service.proto` — `OMSService` RPC definitions
   - `protos/zk/gateway/v1/gateway_service.proto` — `GatewayService` RPC definitions
   - `protos/zk/exch_gw/v1/exch_gw.proto` — exchange gateway report types
   - `protos/zk/engine/v1/engine_service.proto` — `EngineService` lifecycle RPCs
   - `protos/zk/strategy/v1/strategy.proto` — lifecycle, log, notification types
   - `protos/zk/rtmd/v1/rtmd.proto` — tick, kline, funding, orderbook
   - `protos/zk/monitor/v1/monitor.proto` — alert, notification types
   - `protos/zk/pilot/v1/pilot_service.proto` — `PilotService` RPC definitions
2. Update `zk-proto-rs` build script to generate from new files
3. Add re-export aliases in `zk-proto-rs/src/lib.rs` for legacy module names (`oms`, `tqrpc_oms`, etc.) so existing code compiles without change during transition

Do NOT delete legacy proto files yet — they are needed for Python service compatibility until migration is complete.

### 1.2 `zk-infra-rs` modules

Add the following modules to `zkbot/rust/crates/zk-infra-rs/src/`:

#### `nats.rs` — NATS connection and pub/sub helpers
- `NatsConfig { url, creds_path, max_reconnects }`
- `connect(config: &NatsConfig) -> Result<async_nats::Client>`
- `subject_builder` module: typed helpers to construct topic strings per [05-api-contracts.md](../05-api-contracts.md)
  - `oms_order_update(oms_id, account_id) -> String`
  - `oms_balance_update(oms_id, asset) -> String`
  - `oms_position_update(oms_id, instrument) -> String`
  - `gw_report(gw_id) -> String`
  - `rtmd_tick(venue, instrument_exch) -> String`
  - `rtmd_kline(venue, instrument_exch, interval) -> String`
  - etc.

#### `nats_kv.rs` — NATS KV registry client
- `KvRegistryClient` — wraps `async_nats::jetstream::kv::Store`
- `register(key, payload: ServiceRegistration, ttl_ms) -> Result<()>`
- `heartbeat_loop(key, payload_fn, interval) -> JoinHandle` — updates key on interval using CAS
- `watch(key_prefix) -> impl Stream<Item = KvUpdate>` — watches for endpoint changes
- `get_all_by_prefix(prefix) -> Result<Vec<(String, ServiceRegistration)>>`

#### `grpc.rs` — tonic channel builder
- `GrpcChannelConfig { address, tls_ca_path, connect_timeout, request_timeout }`
- `build_channel(config: &GrpcChannelConfig) -> Result<tonic::transport::Channel>`
- Channel pool and reconnect logic (backoff + jitter)

#### `pg.rs` — PostgreSQL connection pool
- `PgConfig { url, max_connections }`
- `connect(config: &PgConfig) -> Result<sqlx::PgPool>`
- Re-export `sqlx::PgPool` for use in service binaries

#### `redis.rs` — Redis client wrapper
- `RedisConfig { url }`
- `connect(config: &RedisConfig) -> Result<redis::aio::MultiplexedConnection>`
- Typed key helpers for OMS state keys:
  - `order_key(oms_id, order_id) -> String`
  - `open_orders_key(oms_id, account_id) -> String`
  - `balance_key(oms_id, account_id, asset) -> String`
  - `position_key(oms_id, account_id, instrument, side) -> String`

#### `mongo.rs` — MongoDB event writer
- `MongoConfig { url, database }`
- `connect(config: &MongoConfig) -> Result<mongodb::Client>`
- `EventWriter` — typed append-only writer with envelope wrapping

#### `metrics.rs` — Prometheus metrics registry
- `init_metrics_server(port: u16)` — starts metrics HTTP endpoint
- Common histogram/counter constructors matching the metric names in [09-ops-design.md](../09-ops-design.md)

#### `tracing.rs` — OpenTelemetry init
- `init_tracing(service_name: &str)` — sets up JSON logging + OTel SDK

#### `config.rs` — shared env-var config loader
- `from_env::<T: serde::Deserialize>() -> Result<T>` — reads env vars into typed config struct

## Tests

### Unit tests (no docker required)

- `nats_kv.rs`: mock the KV store; test `heartbeat_loop` fires on interval, CAS conflict triggers re-bootstrap
- `nats.rs`: test `subject_builder` produces correct topic strings for all topic types
- `redis.rs`: test key helper functions produce correct strings
- `grpc.rs`: test `build_channel` returns error on invalid address

### Integration tests (requires docker-compose)

- `test_nats_connect`: connect to `localhost:4222`, verify JetStream enabled
- `test_kv_register_and_watch`: write a `ServiceRegistration` to KV, watch sees the update
- `test_kv_heartbeat_expiry`: write entry with 2s TTL, stop heartbeat, verify entry expires
- `test_pg_connect`: connect to PG, run `SELECT 1`, verify schema exists
- `test_redis_connect`: connect, set/get order key, verify round-trip
- `test_proto_round_trip`: encode `PlaceOrderRequest` with new proto package, decode, verify fields

### Parity tests

- For each migrated proto type, compare serialized bytes of old package vs new package (they should be wire-compatible where field numbers are preserved)

## Exit criteria

- [ ] `cargo build -p zk-proto-rs` succeeds — new `zk::*` modules available
- [ ] `cargo build -p zk-infra-rs` succeeds — all 8 modules compile
- [ ] all legacy `oms`, `tqrpc_oms` etc. re-exports compile without change in existing crates
- [ ] `cargo test -p zk-infra-rs` — unit tests pass
- [ ] `cargo test -p zk-infra-rs -- --ignored` — integration tests pass against dev stack
- [ ] `test_kv_heartbeat_expiry` confirms entry expires within 25s (TTL 20s + 5s buffer)
- [ ] `test_proto_round_trip` confirms wire compatibility for all migrated types
- [ ] no `TODO` or `unimplemented!()` in any delivered module
