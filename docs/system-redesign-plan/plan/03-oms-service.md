# Phase 3: OMS gRPC Service

## Goal

Wrap `zk-oms-rs` in a production-ready gRPC service binary (`zk-oms-svc`) that:
- serves `zk.oms.v1.OMSService`
- publishes order/balance/position events to NATS
- writes OMS state to Redis on every transition
- warm-starts from Redis and reconciles from gateway on startup
- self-registers in NATS KV

## Prerequisites

- Phase 1 complete (`zk-infra-rs` modules available)
- Phase 2 complete (`zk-mock-gw` running in docker-compose with `svc.gw.gw_mock_1` registered in KV)

## Deliverables

### 2.1 `zk-oms-svc` binary

Location: `zkbot/rust/crates/zk-oms-svc/`

#### `main.rs` — bootstrap
1. load `OmsSvcConfig` from env (`ZK_NATS_URL`, `ZK_PG_URL`, `ZK_REDIS_URL`, `ZK_OMS_ID`, `ZK_GRPC_PORT`)
2. init tracing + metrics
3. connect to NATS, PG, Redis via `zk-infra-rs`
4. load `cfg.oms_instance` and `cfg.account_binding` from PG
5. load instrument/risk config via `ConfdataManager` (from PG)
6. warm-start: load order/position/balance snapshots from Redis
7. watch `svc.gw.*` in NATS KV; connect to resolved gateways via `zk.gateway.v1.GatewayService` gRPC
8. reconcile: `QueryOrder` + `QueryBalance` for each bound account
9. start tonic gRPC server on `ZK_GRPC_PORT`
10. register `svc.oms.<oms_id>` in NATS KV via `KvRegistryClient`
11. start NATS subscriber for gateway report topics (`zk.gw.<gw_id>.report`, `zk.gw.<gw_id>.balance`, `zk.gw.<gw_id>.system`)
12. start periodic tasks (order resync 60s, balance resync 60s, cleanup 10min)

#### `oms_state.rs` — OmsCore actor + read replica

The OMS service uses a **single-writer actor + read replica** pattern to separate mutation from reads without locking `OmsCore`.

```
                   mpsc::Sender<OmsCommand>
PlaceOrder gRPC ──────────────────────────────────► OmsCore writer task (single)
CancelOrder gRPC ─────────────────────────────────►   process_message()
NATS report sub ──────────────────────────────────►   dispatch GW / NATS / Redis
                                                       take_snapshot()
                                                       replica.store(Arc::new(snap))
                                                               │
                                                    ArcSwap<OmsSnapshot>
                                                               │
QueryOpenOrders gRPC ◄──── replica.load() ◄──────────────────┘
QueryBalances gRPC   ◄──── replica.load()
QueryPositions gRPC  ◄──── replica.load()
```

```rust
// Shared read replica type
pub type ReadReplica = Arc<ArcSwap<OmsSnapshot>>;

// Writer task — single tokio::spawn, owns OmsCore exclusively
async fn oms_writer_loop(
    mut core: OmsCore,
    mut rx: mpsc::Receiver<OmsCommand>,
    replica: ReadReplica,
    nats: NatsPublisher,
    redis: RedisWriter,
    gw_clients: HashMap<String, GatewayClient>,
    shutdown: CancellationToken,
) {
    loop {
        tokio::select! {
            Some(cmd) = rx.recv() => {
                let actions = core.process_message(cmd.to_oms_message());
                for action in actions {
                    handle_action(action, &gw_clients, &nats, &redis).await;
                }
                // atomically publish new state to all readers
                replica.store(Arc::new(core.take_snapshot()));
                cmd.ack(/* success */);
            }
            _ = shutdown.cancelled() => break,
        }
    }
}
```

`OmsSnapshot` is defined in `zk-oms-rs` as a pure data type (no infra deps):

```rust
#[derive(Clone, Debug)]
pub struct OmsSnapshot {
    pub orders: HashMap<i64, OmsOrder>,
    pub open_order_ids: HashMap<i64, HashSet<i64>>,  // account_id → {order_id}
    pub balances: HashMap<String, Balance>,            // "account_id:asset" → balance
    pub positions: HashMap<String, OmsPosition>,       // "account_id:instrument:side" → position
    pub panic_accounts: HashSet<i64>,
    pub seq: u64,           // monotonic, incremented per mutation
    pub snapshot_ts_ms: i64,
}

impl OmsCore {
    pub fn take_snapshot(&self) -> OmsSnapshot { ... }
}
```

`arc-swap` is a pure concurrency primitive (no I/O). `replica.store()` is a single atomic pointer swap. `replica.load()` returns an `Arc<OmsSnapshot>` — the reader holds a ref to the snapshot at that instant; a concurrent writer swap does not invalidate it.

#### `grpc_handler.rs` — `OMSService` implementation

Implement all RPCs from [05-api-contracts.md](../05-api-contracts.md) §2.1.

The handler struct holds both the command sender (for mutations) and the read replica (for queries):

```rust
pub struct OmsGrpcHandler {
    cmd_tx: mpsc::Sender<OmsCommand>,  // mutations → writer task
    replica: ReadReplica,              // reads → lock-free snapshot
}
```

`PlaceOrder`:
1. validate `order_id` uniqueness (check `replica.load().orders`)
2. send `OmsCommand::PlaceOrder(req, reply_tx)` via `cmd_tx`
3. await `reply_rx` — writer task processes it, updates replica, sends ack
4. return `CommandAck`

Steps 3–4: gateway dispatch, Redis write, NATS publish all happen inside the writer task, not in the gRPC handler.

`CancelOrder`: same pattern — send command, await ack.

`Panic` / `ClearPanic`: send command, writer task sets/clears `panic_accounts` in `OmsCore`, swaps replica, publishes `OMSSystemEvent`.

`QueryOpenOrders`:
```rust
let snap = self.replica.load();  // atomic load — never blocks writer
let order_ids = snap.open_order_ids.get(&account_id).cloned().unwrap_or_default();
let orders: Vec<_> = order_ids.iter().filter_map(|id| snap.orders.get(id)).cloned().collect();
```

`QueryBalances` / `QueryPositions`: same — `replica.load()` then filter snapshot maps.

`ReloadConfig`: send `OmsCommand::ReloadConfig` → writer task reloads `ConfdataManager` from PG, swaps replica, publishes `OMSSystemEvent`.

#### `nats_handler.rs` — gateway report subscriber

`handle_order_report(msg: OrderReport)`:
1. call `OmsCore::process_message(OmsMessage::ExchReport(report))`
2. for each `OmsAction` returned:
   - `PublishOrderUpdate` → publish to NATS, write to Redis
   - `PublishBalanceUpdate` → publish to NATS, write to Redis
   - `SendOrder` / `SendCancel` → dispatch to gateway gRPC (for OMS-generated orders like emergency cancel)
3. on terminal status (`FILLED`, `CANCELLED`, `REJECTED`): update Redis entry TTL to 1h, remove from `open_orders_key` set

`handle_gw_system_event(event: GwSystemEvent)`:
- if `GW_EVENT_STARTED`: trigger full order + balance resync for affected accounts

#### `redis_writer.rs` — Redis state sync

Write on every state transition:
- `HSET oms:<oms_id>:order:<order_id>` — full order snapshot (MessagePack or JSON)
- `SADD oms:<oms_id>:open_orders:<account_id> <order_id>` (only for non-terminal status)
- `SET oms:<oms_id>:balance:<account_id>:<asset>` — balance snapshot
- `SET oms:<oms_id>:position:<account_id>:<instrument>:<side>` — position snapshot

### 2.2 Resolve `zk-oms-rs` stubs

Complete before this phase closes:
- `OmsCore::calc_balance_changes_for_report` — implement spot/margin balance bookkeeping (Phase 2b per plan)
- Add max-size bound to `pending_order_reports` cache (configurable via `ConfdataManager`)

## Tests

### Unit tests

- `test_place_order_idempotency`: same `(order_id, source_id, idempotency_key)` submitted twice → second returns `CommandAck{success:false, error: DUPLICATE}`, no second gateway call
- `test_panic_blocks_orders`: set panic, attempt PlaceOrder → `CommandAck{success:false, error: PANIC_MODE}`
- `test_order_state_redis_write`: place order → verify Redis `order_key` and `open_orders_key` are set
- `test_terminal_order_redis_cleanup`: fill order → verify `open_orders_key` no longer contains order_id
- `test_reload_config_updates_confdatamanager`: call `ReloadConfig`, verify updated limits reflected
- `test_gw_report_triggers_order_update_nats`: inject `OrderReport(FILLED)` → verify NATS event published on correct topic

### Integration tests (docker-compose)

- `test_oms_startup_warmstart_and_reconcile`:
  1. pre-populate Redis with one open order snapshot
  2. start OMS, check it warm-loads the order
  3. mock-gw returns same order from `QueryOrder` → order state converges
- `test_oms_place_and_fill_roundtrip`:
  1. place order via `PlaceOrder` gRPC
  2. mock-gw fills it → publishes `OrderReport(FILLED)` to `zk.gw.gw_mock_1.report`
  3. OMS processes report → publishes `OrderUpdateEvent(FILLED)` to NATS
  4. verify Redis reflects FILLED status with 1h TTL
- `test_oms_kv_registration`:
  1. start OMS
  2. NATS KV watch → confirm `svc.oms.oms_dev_1` entry present within 5s
  3. stop OMS → confirm entry expires within 25s
- `test_oms_gateway_reconnect`:
  1. start OMS, confirm state reconciled
  2. restart mock-gw
  3. mock-gw publishes `GW_EVENT_STARTED`
  4. OMS triggers resync → verify balance update event published

### Parity tests

- `test_oms_parity_place_order`: submit identical PlaceOrder to Python OMS and Rust OMS service; compare `OrderUpdateEvent` payloads field-by-field
- `test_oms_parity_fill_sequence`: inject same `OrderReport` sequence into both; compare resulting order state

## Exit criteria

- [ ] `cargo build -p zk-oms-svc` succeeds
- [ ] unit tests pass: `cargo test -p zk-oms-svc`
- [ ] integration tests pass: `cargo test -p zk-oms-svc -- --ignored`
- [ ] parity tests pass: `cargo test -p zk-oms-svc -- parity`
- [ ] `test_oms_place_and_fill_roundtrip` completes in < 500ms end-to-end (place → fill → NATS event)
- [ ] `svc.oms.oms_dev_1` appears in NATS KV within 5s of startup
- [ ] OMS startup (warm-load + gateway reconcile) completes in < 10s against docker-compose stack
- [ ] `calc_balance_changes_for_report` stub resolved — balance events contain non-null fields for spot fills
- [ ] no `unimplemented!()` or `todo!()` in delivered code
