# Phase 4: Gateway Services

## Goal

Implement two gateway families:

- `zk-gw-svc` — a production-ready trading gateway binary for order execution and account queries
- `zk-rtmd-gw-svc` — a production-ready realtime market data gateway for RTMD publishing

Shared goals:

- register in NATS KV through the generic service discovery mechanism
- wrap venue SDKs/adapters behind reusable traits
- keep business-specific routing and subscription policy in Pilot, not in the registry protocol
- allow the RTMD gateway runtime to run either as a standalone service or embedded inside an AIO engine

The first concrete target is an **OKX** venue implementation (spot + perp). `zk-mock-gw`
(Phase 2) implements the trading-gateway interface and serves as the integration test harness
for all other phases; this phase extends the same venue coverage to both trading and RTMD paths.

## Prerequisites

- Phase 1 complete (`zk-infra-rs` modules available)
- Phase 2 complete (`zk-mock-gw` validates the `GatewayService` contract before this is built)
- docker-compose stack running (NATS, Vault, PostgreSQL, Pilot)
- Vault seeded with test API keys at `kv/trading/gw/<account_id>/api_key` etc.

## Deliverables

### 4.1 `zk-gw-svc` binary

Location: `zkbot/rust/crates/zk-gw-svc/`

Architecture reference:

- [Gateway Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/gateway_service.md)
- [Gateway Simulator](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/gateway_simulator.md)

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
   - initialize unified reconnect supervisor
6. start tonic gRPC server on `ZK_GRPC_PORT`
7. register `svc.gw.<gw_id>` in NATS KV via `KvRegistryClient`:
   - `service_type: "gw"`, `service_id: gw_id`
   - `account_ids: [account_id]`
   - `venue`, `capabilities`, `transport.address`
   - publish-contract capability metadata where needed
8. start heartbeat loop (5s interval, 20s TTL)
9. start NATS publisher for execution reports (from WebSocket → NATS bridge)

#### `grpc_handler.rs` — `GatewayService` implementation

Implement all RPCs from [api_contracts.md](../../system-arch/api_contracts.md) §2.2:

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

Publishing contract requirement:

- trade/fill facts must be exactly once
- order lifecycle events are at least once
- balance/position updates published after a trade/order update must already reflect that change
- explicit `-> 0` position transitions must be published even when the venue omits them

Compensating recovery requirement:

- on reconnect or suspected stream gap, gateway must run balance/position snapshot queries and use
  those to re-establish a correct state baseline
- in streaming mode, if balance/position pushes arrive before or only slightly after recent
  order/trade/cancel transitions, gateway must use compensating queries before publishing the
  authoritative downstream state

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
    async fn connect(&self) -> Result<()>;
    async fn place_order(&self, req: VenuePlaceOrder) -> Result<VenueCommandAck>;
    async fn cancel_order(&self, req: VenueCancelOrder) -> Result<VenueCommandAck>;
    async fn query_balance(&self, req: VenueBalanceQuery) -> Result<Vec<VenueBalanceFact>>;
    async fn query_order(&self, req: VenueOrderQuery) -> Result<Vec<VenueOrderFact>>;
    async fn query_trades(&self, req: VenueTradeQuery) -> Result<Vec<VenueTradeFact>>;
    async fn query_positions(&self, req: VenuePositionQuery) -> Result<Vec<VenuePositionFact>>;
    async fn next_event(&self) -> Result<VenueEvent>;
}
```

Adaptor constraint:

- keep the adaptor minimal
- venue adaptors translate commands/queries and surface venue facts/events
- semantic unification for trade/order/position/balance publication is implemented in the gateway service layer, not inside individual adaptors
- reconnect policy/state machine is implemented in the gateway service layer, not inside individual adaptors
- adaptor may operate in `streaming`, `query_after_action`, `periodic_query`, or `hybrid` mode, but
  all modes must feed the same `VenueEvent` pipeline

### 4.2 `zk-rtmd-gw-svc` binary

Location:

- Rust option: `zkbot/rust/crates/zk-rtmd-gw-svc/`
- Python option: `zkbot/services/zk-rtmd-gw/`

Architecture reference:

- [Market Data Gateway Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/market_data_gateway_service.md)
- [Pilot Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/pilot_service.md)

Implementation targets:

1. load `RtmdGwConfig` from env:
   - `ZK_NATS_URL`, `ZK_PILOT_API_URL`
   - `ZK_LOGICAL_ID`, `ZK_VENUE`
   - `ZK_RUNTIME_INFO`
2. connect to NATS and Pilot
3. call Pilot bootstrap / start API to claim the RTMD logical instance and retrieve:
   - granted `logical_id`
   - registration metadata/profile
   - effective venue scope
   - initial desired subscription set
4. initialize `RtmdVenueAdapter`
5. start RTMD gRPC query server for current/bounded-history queries
6. register `svc.mdgw.<logical_id>` in KV
   - include query endpoint transport
   - include supported query families in registration capabilities/metadata
7. start the control loop:
   - subscribe to `zk.control.mdgw.<logical_id>.reload`
   - optionally also `zk.control.mdgw.<venue>.reload` for global venue-wide reloads
8. run `SubscriptionManager.reconcile(initial_desired_set)`
9. start publisher loop and heartbeat/supervision

Required modules:

- `adapter.rs` / `adapter.py`
- `subscription_manager.rs` / `subscription_manager.py`
- `publisher.rs` / `publisher.py`
- `query.rs` / `query.py`
- `control.rs` / `control.py`
- registry integration using the shared KV registration client

Initial phase boundary for RTMD gateway:

- Phase 1: single active RTMD publisher per scope, correct subscription aggregation, lease expiry, and restart recovery
- Phase 2: hot-standby or fast-takeover RTMD gateway HA
- Phase 3: more advanced redundant ingestion only if needed, with strict publisher fencing

## Tests

### Unit tests

- `test_normalize_okx_order_report`: inject raw OKX JSON WebSocket event → assert `OrderReport` fields (status, qty, price, exch_order_ref)
- `test_normalize_okx_balance_update`: inject raw OKX balance push → assert `BalanceUpdate` per asset
- `test_sign_okx_request`: assert HMAC-SHA256 signature matches expected for known timestamp+body
- `test_venue_adapter_place_order_error_mapping`: map OKX error codes to `GwCommandAck{success:false, error_code}`
- `test_gateway_trade_exactly_once`: duplicate venue trade input does not produce duplicate normalized trade fact
- `test_gateway_order_at_least_once`: repeated order-state publication remains consumer-safe and preserves at-least-once semantics
- `test_gateway_balance_position_causality`: a balance/position update published after a trade/order event reflects that event
- `test_gateway_streaming_balance_position_trust_window`: near-concurrent balance/position push triggers compensating query before final publication
- `test_gateway_explicit_zero_position_publish`: when a previously published position disappears from query results, gateway emits an explicit zero-position update
- `test_gateway_reconnect_runs_compensating_queries`: reconnect triggers balance/position snapshot queries before returning to live state
- `test_gateway_config_split_generic_and_adaptor_specific`: gateway loads generic config and typed adaptor config separately
- `test_gateway_query_after_action_mode_synthesizes_events`: a query-driven adaptor emits `VenueEvent` facts after place/cancel without venue push delivery
- `test_gateway_periodic_query_mode_reconstructs_stream`: periodic polling mode converts query deltas into venue facts consumed by the same semantic pipeline
- `test_rtmd_subscription_manager_reconcile_add_remove`: desired set changes → assert adapter subscribe/unsubscribe diff is minimal and correct
- `test_rtmd_subscription_manager_dedups_same_stream`: multiple leases for the same effective stream key → assert one upstream subscribe and refcounted removal
- `test_rtmd_publisher_topic_mapping`: normalized RTMD event publishes to expected deterministic topic
- `test_rtmd_query_current_tick`: query API returns current tick snapshot for instrument
- `test_rtmd_query_klines`: query API returns bounded recent kline history
- `test_rtmd_reload_reconciles_from_pilot_snapshot`: reload event triggers fresh desired-state fetch and resubscribe diff
- `test_embedded_rtmd_runtime_uses_shared_components`: embedded mode reuses the same adapter/subscription manager interfaces as standalone mode

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
- `test_rtmd_gw_kv_registration`:
  1. start standalone `zk-rtmd-gw-svc`
  2. watch NATS KV → confirm `svc.mdgw.<logical_id>` appears within 5s
  3. stop process → confirm entry expires within 25s
- `test_rtmd_gw_dynamic_reload`:
  1. seed Pilot/PG desired subscription rows
  2. start `zk-rtmd-gw-svc`
  3. update desired set and publish reload
  4. assert venue adapter subscription set converges to the new desired state
- `test_rtmd_gw_publishes_orderbook_and_kline`:
  1. subscribe to `zk.rtmd.orderbook.*` and `zk.rtmd.kline.*`
  2. feed venue sandbox or fixture events
  3. assert normalized payloads are published
- `test_rtmd_gw_query_endpoint_registration`:
  1. start standalone `zk-rtmd-gw-svc`
  2. inspect `svc.mdgw.<logical_id>` registration
  3. assert query endpoint transport and supported query capabilities are present
- `test_rtmd_gw_query_current_and_history`:
  1. call RTMD gRPC query API
  2. assert current snapshot and bounded kline history are returned for an instrument
- `test_rtmd_gw_restart_rebuilds_interest_from_kv`:
  1. create active `zk.rtmd.subs.v1` leases
  2. start RTMD gateway and confirm publishing
  3. kill and restart RTMD gateway
  4. assert it rebuilds upstream subscriptions from KV and resumes publishing
- `test_embedded_rtmd_gw_in_aio_engine`:
  1. start engine in `engine+mdgw` mode
  2. assert both engine and mdgw registrations appear when configured
  3. assert RTMD topics continue publishing from the embedded runtime

Deferred HA tests:

- `test_rtmd_gw_hot_standby_takeover`:
  1. start active and standby RTMD gateways for the same logical scope
  2. confirm only active publishes
  3. kill active
  4. assert standby takes ownership and resumes publishing without duplicate publisher overlap

## Exit criteria

- [ ] `cargo build -p zk-gw-svc` succeeds
- [ ] unit tests pass: `cargo test -p zk-gw-svc`
- [ ] `test_gw_vault_credential_fetch` passes with dev Vault
- [ ] `test_gw_kv_registration` passes: entry appears and expires correctly
- [ ] `test_gw_system_event_on_startup` passes within 5s
- [ ] OKX venue adapter compiles; sandbox `QueryBalance` returns data (real credentials optional in CI via fixture)
- [ ] RTMD gateway runtime exists as a shared component set, not a one-off venue binary
- [ ] standalone RTMD gateway registers in KV and publishes deterministic RTMD topics
- [ ] standalone RTMD gateway exposes a gRPC query API for current snapshots and bounded recent history
- [ ] RTMD registration advertises the query endpoint and supported query families
- [ ] RTMD gateway aggregates duplicate leases for the same stream into one upstream venue subscription
- [ ] RTMD gateway restart rebuilds live interest from `zk.rtmd.subs.v1`
- [ ] Pilot-driven RTMD policy/default reload converges without process restart
- [ ] RTMD gateway HA hot-standby takeover is tracked as a later phase milestone, not required for initial delivery
- [ ] embedded RTMD gateway mode works inside the AIO engine using the same shared runtime components
- [ ] No `unimplemented!()` in delivered code; OKX adapter stubs must be marked with `// TODO(phase-4)` and tracked
