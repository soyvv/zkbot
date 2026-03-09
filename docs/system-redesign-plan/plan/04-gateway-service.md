# Phase 4: Real Trading Gateway Service

## Goal

Implement `zk-gw-svc` — a production-ready gRPC service binary that:
- connects to a single venue/account pair
- serves `zk.gateway.v1.GatewayService`
- publishes normalized execution reports and balance updates to NATS
- self-registers in NATS KV (`svc.gw.<gw_id>`) with heartbeat

The first concrete target is an **OKX gateway** (spot + perp). `zk-mock-gw` (Phase 2)
implements the same interface and serves as the integration test harness for all other phases;
this phase delivers the real venue-connected implementation.

## Prerequisites

- Phase 1 complete (`zk-infra-rs` modules available)
- Phase 2 complete (`zk-mock-gw` validates the `GatewayService` contract before this is built)
- docker-compose stack running (NATS, Vault)
- Vault seeded with test API keys at `kv/trading/gw/<account_id>/api_key` etc.

## Deliverables

### 3.1 `zk-gw-svc` binary

Location: `zkbot/rust/crates/zk-gw-svc/`

#### `main.rs` — bootstrap

1. load `GwSvcConfig` from env:
   - `ZK_NATS_URL`, `ZK_VAULT_ADDR`, `ZK_VAULT_TOKEN` (or `ZK_VAULT_ROLE`)
   - `ZK_GW_ID`, `ZK_ACCOUNT_ID`, `ZK_VENUE`, `ZK_GRPC_PORT`
2. fetch exchange credentials from Vault:
   - `kv/trading/gw/<account_id>/api_key`
   - `kv/trading/gw/<account_id>/api_secret`
   - `kv/trading/gw/<account_id>/private_key` (if applicable)
3. init tracing + metrics
4. connect to NATS via `zk-infra-rs`
5. initialize venue session (REST + WebSocket):
   - validate connectivity via `GET /account` or equivalent
   - subscribe to execution report WebSocket feed
6. start tonic gRPC server on `ZK_GRPC_PORT`
7. register `svc.gw.<gw_id>` in NATS KV via `KvRegistryClient`:
   - `service_type: "gw"`, `service_id: gw_id`
   - `account_ids: [account_id]`
   - `venue`, `capabilities`, `transport.address`
8. start heartbeat loop (5s interval, 20s TTL)
9. start NATS publisher for execution reports (from WebSocket → NATS bridge)

#### `grpc_handler.rs` — `GatewayService` implementation

Implement all RPCs from [05-api-contracts.md](../05-api-contracts.md) §2.2:

**Order execution:**

`PlaceOrder(GwPlaceOrderRequest) -> GwCommandAck`:
1. map proto request to venue REST/WS order request
2. sign with fetched credentials
3. call venue REST API
4. return `GwCommandAck{success, exch_order_ref}` (or error)

`BatchPlaceOrders`: iterate and call `PlaceOrder` per order; collect results.

`CancelOrder(GwCancelOrderRequest) -> GwCommandAck`:
1. map to venue cancel request
2. call venue REST cancel API
3. return `GwCommandAck{success}`

`BatchCancelOrders`: iterate and call `CancelOrder` per cancel.

**Queries:**

`QueryBalance(GwQueryBalanceRequest) -> GwQueryBalanceResponse`:
- call venue REST balance endpoint
- normalize to `BalanceUpdate` per asset

`QueryOrder(GwQueryOrderRequest) -> GwQueryOrderResponse`:
- call venue REST order query
- normalize to `OrderReport`

`QueryTrades(GwQueryTradesRequest) -> GwQueryTradesResponse`:
- call venue REST trade history
- normalize to list of `OrderReport` (fill records)

#### `nats_publisher.rs` — execution report bridge

Bridge from venue WebSocket feed to NATS topics:

`on_execution_report(raw_event)`:
1. normalize venue execution report → `exch_gw.OrderReport`
2. publish to `zk.gw.<gw_id>.report`

`on_balance_update(raw_event)`:
1. normalize → `exch_gw.BalanceUpdate`
2. publish to `zk.gw.<gw_id>.balance`

`on_position_update(raw_event)`:
1. normalize → `exch_gw.PositionUpdate`
2. publish to `zk.gw.<gw_id>.position`

`on_session_event(event)`:
1. on connect/reconnect: build `GwSystemEvent{event_type: GW_EVENT_STARTED, gw_id}`
2. publish to `zk.gw.<gw_id>.system`

#### `venue/` — venue-specific adapter

```
zk-gw-svc/src/venue/
  mod.rs          -- VenueAdapter trait
  okx/
    mod.rs
    rest.rs       -- OKX REST client (sign, retry, rate-limit)
    ws.rs         -- OKX WebSocket client (reconnect loop)
    normalize.rs  -- OKX raw types → zk proto types
```

`VenueAdapter` trait:
```rust
#[async_trait]
pub trait VenueAdapter: Send + Sync {
    async fn place_order(&self, req: GwPlaceOrderRequest) -> Result<GwCommandAck>;
    async fn cancel_order(&self, req: GwCancelOrderRequest) -> Result<GwCommandAck>;
    async fn query_balance(&self, req: GwQueryBalanceRequest) -> Result<GwQueryBalanceResponse>;
    async fn query_order(&self, req: GwQueryOrderRequest) -> Result<GwQueryOrderResponse>;
    async fn query_trades(&self, req: GwQueryTradesRequest) -> Result<GwQueryTradesResponse>;
}
```

## Tests

### Unit tests

- `test_normalize_okx_order_report`: inject raw OKX JSON WebSocket event → assert `OrderReport` fields (status, qty, price, exch_order_ref)
- `test_normalize_okx_balance_update`: inject raw OKX balance push → assert `BalanceUpdate` per asset
- `test_sign_okx_request`: assert HMAC-SHA256 signature matches expected for known timestamp+body
- `test_venue_adapter_place_order_error_mapping`: map OKX error codes to `GwCommandAck{success:false, error_code}`

### Integration tests (docker-compose)

Integration tests for this phase run against a real OKX sandbox account (or use recorded HTTP
fixtures via `wiremock` for CI without credentials). The mock-gw is already validated in Phase 2.

- `test_gw_vault_credential_fetch`:
  1. Vault seeded with test API key at expected path
  2. gw startup reads credentials successfully (no panic, no empty strings)
- `test_gw_kv_registration`:
  1. start `zk-gw-svc` with sandbox credentials
  2. watch NATS KV → confirm `svc.gw.<gw_id>` appears within 5s
  3. stop process → confirm entry expires within 25s
- `test_gw_system_event_on_startup`:
  1. subscribe to `zk.gw.<gw_id>.system`
  2. start `zk-gw-svc` → assert `GwSystemEvent { GW_EVENT_STARTED }` within 5s
- `test_gw_query_balance`:
  1. call `QueryBalance` via gRPC
  2. assert response contains at least one asset with non-negative balance

## Exit criteria

- [ ] `cargo build -p zk-gw-svc` succeeds
- [ ] unit tests pass: `cargo test -p zk-gw-svc`
- [ ] `test_gw_vault_credential_fetch` passes with dev Vault
- [ ] `test_gw_kv_registration` passes: entry appears and expires correctly
- [ ] `test_gw_system_event_on_startup` passes within 5s
- [ ] OKX venue adapter compiles; sandbox `QueryBalance` returns data (real credentials optional in CI via fixture)
- [ ] No `unimplemented!()` in delivered code; OKX adapter stubs must be marked with `// TODO(phase-4)` and tracked
