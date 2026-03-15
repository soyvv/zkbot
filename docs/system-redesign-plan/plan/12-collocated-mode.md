# Phase 11: Collocated Mode (2-layer / All-in-One)

## Goal

Implement `zk-collocated-svc` — a single-binary deployment mode that eliminates gRPC/NATS overhead on the critical trading path by running Engine, OMS, and optionally a Gateway plugin in the same process, communicating via in-process channels and direct function calls.

Two sub-modes:

- **2-layer** (Engine + OMS in-process, GW via gRPC): `tokio::sync::mpsc` channel replaces gRPC between Engine and OMS; `GatewayPlugin` trait replaces gRPC calls to a separate gateway process
- **All-in-one** (Engine + OMS + GW plugin in one process): same as 2-layer but GW is also in-process; no gRPC or NATS on the critical path

NATS is still used for external event publishing (Recorder, Monitor) and control messages in both modes.

## Prerequisites

- Phase 3 complete (`zk-oms-svc`; `zk-oms-rs` domain crate available)
- Phase 4 complete (`VenueAdapter` trait for gateway; mock-gw validated)
- Phase 7 complete (`zk-engine-rs` domain crate; `Strategy` trait)
- Phase 1 complete (`zk-infra-rs` for NATS publishing and KV registration)

## Deliverables

### 9.1 `GatewayPlugin` trait — `zk-infra-rs`

Location: `zkbot/rust/crates/zk-infra-rs/src/gateway_plugin.rs`

```rust
#[async_trait]
pub trait GatewayPlugin: Send + Sync {
    async fn place_order(&self, req: GwPlaceOrderRequest) -> Result<GwCommandAck>;
    async fn cancel_order(&self, req: GwCancelOrderRequest) -> Result<GwCommandAck>;
    async fn batch_place_orders(&self, req: GwBatchPlaceOrdersRequest) -> Result<GwCommandAck>;
    async fn batch_cancel_orders(&self, req: GwBatchCancelOrdersRequest) -> Result<GwCommandAck>;
    async fn query_balance(&self, req: GwQueryBalanceRequest) -> Result<GwQueryBalanceResponse>;
    async fn query_order(&self, req: GwQueryOrderRequest) -> Result<GwQueryOrderResponse>;
}
```

Two implementations:

**`GrpcGatewayPlugin`** (3-layer — forwards to remote gRPC):
```rust
pub struct GrpcGatewayPlugin {
    client: GatewayServiceClient<Channel>,
}
// All methods: forward to gRPC, return response
```

**`InProcessGatewayPlugin`** (2-layer/AIO — calls `VenueAdapter` directly):
```rust
pub struct InProcessGatewayPlugin {
    adapter: Arc<dyn VenueAdapter>,
}
// All methods: call adapter directly, no network
```

**`MockGatewayPlugin`** (for testing):
```rust
pub struct MockGatewayPlugin {
    nats: NatsClient,
    fill_delay_ms: u64,
    balances: HashMap<String, Decimal>,
}
// PlaceOrder: schedules fill via NATS after fill_delay_ms
```

### 9.2 OMS inter-thread command channel

The `OmsCommandChannel` replaces gRPC in 2-layer mode:

```rust
pub enum OmsCommand {
    PlaceOrder(PlaceOrderRequest, oneshot::Sender<CommandAck>),
    CancelOrder(CancelOrderRequest, oneshot::Sender<CommandAck>),
    QueryOpenOrders(QueryOpenOrdersRequest, oneshot::Sender<QueryOpenOrdersResponse>),
    QueryBalances(QueryBalancesRequest, oneshot::Sender<QueryBalancesResponse>),
    QueryPositions(QueryPositionsRequest, oneshot::Sender<QueryPositionsResponse>),
}

pub struct OmsCommandSender(mpsc::Sender<OmsCommand>);
pub struct OmsCommandReceiver(mpsc::Receiver<OmsCommand>);

impl OmsCommandSender {
    pub async fn place_order(&self, req: PlaceOrderRequest) -> Result<CommandAck> {
        let (tx, rx) = oneshot::channel();
        self.0.send(OmsCommand::PlaceOrder(req, tx)).await?;
        rx.await.map_err(Into::into)
    }
    // ... similar for other commands
}
```

OMS task runs a loop receiving from `OmsCommandReceiver` and dispatching to `OmsCore`.

### 9.3 `zk-collocated-svc` binary

Location: `zkbot/rust/crates/zk-collocated-svc/`

#### `main.rs` — bootstrap for 2-layer mode

```
ZK_MODE=2layer | ZK_MODE=all-in-one
```

**2-layer startup:**

1. load config from env (`ZK_STRATEGY_KEY`, `ZK_OMS_ID`, `ZK_GW_IDS`, `ZK_ACCOUNT_IDS`, etc.)
2. init tracing + metrics
3. connect to NATS
4. init `GatewayPlugin` per account:
   - `ZK_MODE=2layer`: `InProcessGatewayPlugin` wrapping `VenueAdapter` (OKX/mock)
   - calls `VenueAdapter::query_balance` + `VenueAdapter::query_order` for reconciliation
5. init `OmsCore` with `InProcessGatewayPlugin` instances
6. create `OmsCommandChannel` pair
7. spawn OMS task: `tokio::spawn(oms_loop(oms_core, receiver, nats_publisher))`
8. init `TradingClient` with `OmsCommandSender` instead of gRPC channel:
   ```rust
   // In-process variant: no gRPC, no KV discovery needed
   TradingClient::from_oms_channel(sender, nats, account_ids, instance_id)
   ```
9. register `svc.oms.<oms_id>` and `svc.engine.<execution_id>` in NATS KV
10. POST to Pilot REST `POST /v1/strategy-executions/start`
11. init strategy via `zk-engine-rs`
12. start engine event loop

**All-in-one startup** (same but skips NATS KV registration for OMS/GW — they're internal):

- OMS events still published to NATS for Recorder/Monitor
- only `svc.engine.<execution_id>` registered in KV (OMS not separately discoverable)

#### `oms_loop.rs` — OMS task

```rust
pub async fn oms_loop(
    mut oms_core: OmsCore,
    mut receiver: OmsCommandReceiver,
    nats_publisher: NatsPublisher,
    redis_writer: RedisWriter,
    shutdown: CancellationToken,
) {
    loop {
        tokio::select! {
            Some(cmd) = receiver.0.recv() => {
                match cmd {
                    OmsCommand::PlaceOrder(req, reply) => {
                        let actions = oms_core.process_message(OmsMessage::Order(...));
                        // dispatch GatewayPlugin.place_order for SendOrder actions
                        // write Redis, publish NATS
                        let _ = reply.send(ack);
                    }
                    // ...
                }
            }
            _ = shutdown.cancelled() => break,
        }
    }
}
```

### 9.4 Latency benchmarks

A benchmark binary (`zk-collocated-bench`) measures critical path latency:

```
cargo bench -p zk-collocated-svc
```

Benchmarks:
- `bench_place_order_inprocess`: Engine → OmsCommandChannel → OmsCore → MockGatewayPlugin (no network) — target < 50µs p99
- `bench_place_order_3layer`: Engine → OMS gRPC (loopback) → OmsCore → GatewayGrpc (loopback) — baseline comparison
- `bench_nats_publish_overhead`: measure NATS publish latency for `OrderUpdateEvent` — expected ~100µs

## Tests

### Unit tests

- `test_oms_command_channel_place_order`: send `PlaceOrder` via channel → assert `CommandAck{success:true}` returned via oneshot
- `test_oms_command_channel_backpressure`: fill channel (capacity 1000) → assert sender blocks (backpressure) rather than dropping
- `test_mock_gateway_plugin_fill`: call `place_order` on `MockGatewayPlugin` → assert NATS `OrderReport(FILLED)` published after `fill_delay_ms`
- `test_in_process_gateway_plugin_place_order`: `InProcessGatewayPlugin` wrapping mock `VenueAdapter` → assert `VenueAdapter::place_order` called with correct args
- `test_grpc_gateway_plugin_place_order`: `GrpcGatewayPlugin` → assert gRPC call forwarded (mock gRPC server)
- `test_collocated_oms_loop_processes_commands`: spawn `oms_loop` task; send 10 `PlaceOrder` commands → assert all acknowledged

### Integration tests (docker-compose)

- `test_collocated_2layer_startup_and_trade`:
  1. start `zk-collocated-svc` in 2-layer mode with `MockGatewayPlugin`
  2. inject synthetic tick → strategy places order
  3. assert `OrderUpdateEvent(BOOKED)` published to NATS within 100ms
  4. mock-gw fills → assert `OrderUpdateEvent(FILLED)` within 500ms
  5. assert Redis updated correctly
- `test_collocated_all_in_one_no_grpc`:
  1. start `zk-collocated-svc` in all-in-one mode
  2. verify no gRPC port open (netstat check)
  3. inject tick → fill roundtrip works correctly via internal channels
- `test_collocated_nats_events_reach_recorder`:
  1. start collocated-svc + Recorder
  2. complete trade roundtrip
  3. assert Recorder writes to MongoDB and `trd.trade_oms` as normal
- `test_collocated_kv_registration`:
  1. start collocated-svc (2-layer mode)
  2. assert `svc.oms.<oms_id>` and `svc.engine.<execution_id>` appear in NATS KV

### Latency benchmarks

- `bench_place_order_inprocess` must complete in < 50µs p99
- `bench_place_order_3layer` baseline documented (expected ~1-2ms loopback)
- ratio between 3-layer and in-process documented as performance improvement evidence

## Exit criteria

- [ ] `cargo build -p zk-collocated-svc` succeeds
- [ ] unit tests pass: `cargo test -p zk-collocated-svc`
- [ ] `test_collocated_2layer_startup_and_trade` passes end-to-end
- [ ] `test_collocated_all_in_one_no_grpc` confirms no open gRPC port
- [ ] `test_collocated_nats_events_reach_recorder` passes (NATS publishing still works in collocated mode)
- [ ] Latency benchmark: in-process `PlaceOrder` p99 < 50µs (no network)
- [ ] `GatewayPlugin` trait has all three implementations: `GrpcGatewayPlugin`, `InProcessGatewayPlugin`, `MockGatewayPlugin`
- [ ] No `unimplemented!()` or `todo!()` in `zk-collocated-svc`
- [ ] Architecture decision on 2-layer vs all-in-one for first production deployment documented
