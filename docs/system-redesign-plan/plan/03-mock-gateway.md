# Phase 2: Mock Gateway (Trading Simulator)

## Goal

Implement `zk-mock-gw` — a self-contained trading simulator that implements
`zk.gateway.v1.GatewayService` without any real exchange connectivity. It becomes
the primary integration test harness for every subsequent phase (OMS, trading_sdk,
engine, collocated mode).

Building the simulator before the OMS service means Phase 3 (OMS) integration tests
can run a complete order lifecycle (place → fill → NATS report) against a real process,
not a hand-rolled stub.

## Prerequisites

- Phase 1 complete (`zk-proto-rs` with `zk.gateway.v1` protos; `zk-infra-rs` NATS + KV modules)
- docker-compose stack running (NATS JetStream)

## Deliverables

### 2.1 `zk-mock-gw` binary

Location: `zkbot/rust/crates/zk-mock-gw/`

Architecture reference:

- [Gateway Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/gateway_service.md)
- [Gateway Simulator](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/gateway_simulator.md)

#### Config (env vars)

| Variable | Default | Purpose |
|---|---|---|
| `ZK_GW_ID` | `gw_mock_1` | gateway identity for KV registration and NATS topics |
| `ZK_ACCOUNT_ID` | `9001` | account this gateway serves |
| `ZK_GRPC_PORT` | `51051` | port for `GatewayService` gRPC server |
| `ZK_NATS_URL` | `nats://localhost:4222` | NATS connection |
| `ZK_FILL_DELAY_MS` | `100` | simulated fill latency |
| `ZK_FILL_MODE` | `immediate` | `immediate` \| `partial` \| `manual` (see below) |
| `ZK_MOCK_BALANCES` | `{"USDT":100000,"BTC":10}` | initial balances (JSON) |

#### `grpc_handler.rs` — `GatewayService` implementation

**`PlaceOrder(GwPlaceOrderRequest) → GwCommandAck`**
1. Assign `exch_order_ref = format!("MOCK-{}", uuid::Uuid::new_v4())`
2. Store order in `in_memory_orders: HashMap<String, MockOrder>` keyed by `exch_order_ref`
3. Schedule fill via `tokio::time::sleep(fill_delay_ms)` in a background task:
   - on wake: publish `OrderReport { status: FILLED, exch_order_ref, ... }` to `zk.gw.<gw_id>.report`
   - update `in_memory_orders` to FILLED
4. Return `GwCommandAck { success: true, exch_order_ref }`

Fill modes:
- `immediate`: full fill after `fill_delay_ms`
- `partial`: 50% fill after `fill_delay_ms`, remainder after another `fill_delay_ms`
- `manual`: no automatic fill; test drives fills via `TriggerFill` admin RPC (see below)

**`CancelOrder(GwCancelOrderRequest) → GwCommandAck`**
1. Look up order by `exch_order_ref`; if not found → return `GwCommandAck { success: false }`
2. If order already in terminal state → return `GwCommandAck { success: false, error: "already terminal" }`
3. Abort pending fill task (via `CancellationToken`)
4. Publish `OrderReport { status: CANCELLED }` to `zk.gw.<gw_id>.report`
5. Return `GwCommandAck { success: true }`

**`QueryBalance(GwQueryBalanceRequest) → GwQueryBalanceResponse`**
- Return configured `ZK_MOCK_BALANCES` as `BalanceUpdate` entries per asset

**`QueryOrder(GwQueryOrderRequest) → GwQueryOrderResponse`**
- Look up by `exch_order_ref` in `in_memory_orders`; return current `OrderReport`

**`BatchPlaceOrders` / `BatchCancelOrders`**: iterate and call the single-order handler per entry.

#### `nats_publisher.rs`

On startup: publish `GwSystemEvent { event_type: GW_EVENT_STARTED, gw_id }` to `zk.gw.<gw_id>.system`.
On reconnect (if NATS disconnects and reconnects): re-publish `GW_EVENT_STARTED`.

#### `admin_handler.rs` — test control RPC (optional, for `manual` fill mode)

A second gRPC server on `ZK_ADMIN_PORT` (default `51052`) with an admin-only service:

```proto
service MockGwAdmin {
  rpc TriggerFill(TriggerFillRequest) returns (CommandAck);
  rpc SetFillMode(SetFillModeRequest) returns (CommandAck);
  rpc SetBalance(SetBalanceRequest)  returns (CommandAck);
  rpc Reset(ResetRequest)            returns (CommandAck);
}
```

- `TriggerFill(exch_order_ref)` — immediately fill a specific order (for deterministic tests)
- `SetFillMode(mode)` — switch fill mode at runtime
- `SetBalance(asset, amount)` — override balance for a test scenario
- `Reset` — clear all orders, restore initial balances (between test cases)

#### `kv_registration.rs`

On startup, register `svc.gw.<gw_id>` in NATS KV `zk.svc.registry.v1`:

```json
{
  "service_type": "gw",
  "service_id": "gw_mock_1",
  "transport": { "protocol": "grpc", "address": "mock-gw:51051" },
  "account_ids": [9001],
  "venue": "MOCK",
  "capabilities": ["place_order", "cancel_order", "query_balance", "query_order"]
}
```

Heartbeat every 5s, TTL 20s (using `KvRegistryClient` from `zk-infra-rs` once available;
directly via `async-nats` JetStream KV API in Phase 2 before infra module is complete).

### 2.2 Dockerfile

`docker/dev/Dockerfile.mock-gw`:
```dockerfile
FROM rust:1.80-slim AS builder
WORKDIR /build
COPY rust/ rust/
RUN cargo build -p zk-mock-gw --release

FROM debian:bookworm-slim
COPY --from=builder /build/target/release/zk-mock-gw /usr/local/bin/
CMD ["zk-mock-gw"]
```

## Tests

### Unit tests

- `test_place_order_assigns_exch_ref`: call `PlaceOrder` on in-memory handler → assert `exch_order_ref` non-empty, format `MOCK-*`
- `test_cancel_pending_order_aborts_fill`: place order with `fill_delay_ms=10_000`, immediately cancel → assert `CANCELLED` report published, no `FILLED` report
- `test_cancel_already_terminal_returns_error`: fill order, then cancel → assert `GwCommandAck { success: false }`
- `test_partial_fill_mode_two_reports`: fill mode `partial` → assert two `OrderReport` events (partial, then filled)
- `test_query_balance_returns_configured_balances`: set `ZK_MOCK_BALANCES={"USDT":50000}` → `QueryBalance` response has `USDT: 50000`
- `test_manual_fill_mode_no_auto_fill`: place order, wait 2× `fill_delay_ms` → assert no `OrderReport` published until `TriggerFill` called

### Integration tests (docker-compose)

- `test_mock_gw_kv_registration`:
  1. start `zk-mock-gw`
  2. watch NATS KV → `svc.gw.gw_mock_1` appears within 5s
  3. stop → entry expires within 25s

- `test_mock_gw_startup_system_event`:
  1. subscribe to `zk.gw.gw_mock_1.system`
  2. start `zk-mock-gw` → assert `GwSystemEvent { GW_EVENT_STARTED }` within 2s

- `test_mock_gw_place_fill_roundtrip`:
  1. subscribe to `zk.gw.gw_mock_1.report`
  2. call `PlaceOrder` via gRPC → `GwCommandAck { success: true }`
  3. assert `OrderReport { status: FILLED }` on NATS within `fill_delay_ms + 200ms`

- `test_mock_gw_cancel_before_fill`:
  1. set `fill_delay_ms=5000`
  2. place order → cancel immediately
  3. assert `OrderReport { status: CANCELLED }` within 500ms
  4. wait 5s → assert no `FILLED` report

- `test_mock_gw_admin_trigger_fill`:
  1. set `fill_mode=manual`
  2. place order → assert no fill report after 500ms
  3. call `TriggerFill(exch_order_ref)` via admin RPC
  4. assert `OrderReport { status: FILLED }` within 200ms

- `test_mock_gw_reset_clears_state`:
  1. place 3 orders
  2. call admin `Reset`
  3. `QueryOrder` for any previous `exch_order_ref` → not found

## Exit criteria

- [ ] `cargo build -p zk-mock-gw` succeeds
- [ ] unit tests pass: `cargo test -p zk-mock-gw`
- [ ] `test_mock_gw_kv_registration` passes — TTL expiry confirmed within 25s
- [ ] `test_mock_gw_startup_system_event` passes
- [ ] `test_mock_gw_place_fill_roundtrip` completes in < `fill_delay_ms + 200ms`
- [ ] `test_mock_gw_cancel_before_fill` passes — no FILLED report after cancel
- [ ] `test_mock_gw_admin_trigger_fill` passes — manual fill mode fully controllable
- [ ] `docker compose up mock-gw` starts cleanly from `docker/dev/docker-compose.yml`
- [ ] `svc.gw.gw_mock_1` entry in KV contains correct `account_ids`, `capabilities`, `transport.address`
- [ ] No `unimplemented!()` or `todo!()` in delivered code
