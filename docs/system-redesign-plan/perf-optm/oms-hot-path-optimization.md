# OMS Hot-Path Latency Optimization (ON HOLD)

**Status: ON HOLD** — planned, not yet started. No code changes made.

Three workstreams targeting `t3-t1` (oms_through) and overall writer loop throughput.

**Benchmarking requirement**: After each optimization step, run:
```bash
cd zkbot/rust && cargo run -p zk-trade-tools --release -- oms-latency-bench \
  --oms-id oms_dev_1 --rates 50,100,200,500 --warmup-s 3 --measure-s 10 \
  --track-order-update-latency true --track-metric-latency true
```

## Implementation Order

1. **C1-C6** (incremental snapshots) — changes `OmsSnapshot` type signatures, do first
2. **B1** → **B2** (Arc config → Arc context) — widest type-sig impact in OmsCore
3. **B3** (remove req.clone) — independent, small
4. **B4** → **B5** (OrderPlacement + cleanup) — depends on B2
5. **B6** (remove stored reqs) — depends on B4
6. **A1-A5** (GW egress) — independent of B/C, biggest latency win, mechanically simpler

C first because it changes `OmsSnapshot` and `OmsSnapshotWriter` signatures that B and A both interact with.

---

## Workstream A: Decouple Gateway gRPC from Writer Loop

### Context

The writer loop awaits `gw_pool.send_order().await` inline (oms_actor.rs:300). A slow gateway stalls all subsequent commands, inflating t3-t1 under load. The gRPC reply to callers also blocks until the GW send completes.

### Design

```text
BEFORE:  cmd_rx → OmsCore → gw_pool.send_order().await → reply
AFTER:   cmd_rx → OmsCore → gw_egress_tx.try_send(job) → reply  (non-blocking)
                                    │
                        per-GW egress task (bounded mpsc)
                            gw_client.send_order().await
                            send GwSendComplete back to writer for latency tracking
```

### Files

| File | Changes |
|------|---------|
| `zk-oms-svc/src/gw_egress.rs` | **NEW** — `GwEgressPool`, `GwEgressJob`, per-GW egress task |
| `zk-oms-svc/src/oms_actor.rs` | Replace inline GW awaits with `gw_egress.send()`; add `GwSendComplete` command |
| `zk-oms-svc/src/config.rs` | Add `gw_egress_channel_buf: usize` (default 1024) |
| `zk-oms-svc/src/main.rs` | Build `GwEgressPool` after startup reconcile, pass to `spawn_writer` |
| `zk-oms-svc/src/lib.rs` | Add `pub mod gw_egress;` |

### Step A1: New `gw_egress.rs`

```rust
pub enum GwEgressJob {
    SendOrder { request: ExchSendOrderRequest, latency_ctx: Option<SendLatencyCtx> },
    BatchSendOrders { request: ExchBatchSendOrdersRequest },
    CancelOrder { request: ExchCancelOrderRequest },
    BatchCancelOrders { request: ExchBatchCancelOrdersRequest },
}

pub struct SendLatencyCtx {
    pub order_id: i64,
    pub order_created_at_ms: i64,
    pub t1_ns: i64,
}

pub struct GwEgressPool {
    senders: HashMap<String, mpsc::Sender<GwEgressJob>>,
    handles: HashMap<String, JoinHandle<()>>,
}
```

- `GwEgressPool::from_client_pool(gw_pool, cmd_tx, channel_buf, shutdown)` — for each client in `gw_pool.take_clients()`, clone the `GatewayClient`, spawn a per-GW egress loop.
- Each egress task: recv job → execute gRPC → for `SendOrder` with latency_ctx, capture t3/t3r and send `OmsCommand::GwSendComplete` back via `cmd_tx`.
- `send(&self, gw_key, job) -> Result<(), GwError>` — `try_send` (non-blocking). Returns `ChannelFull` on backpressure.
- `contains(&self, gw_key)`, `gw_keys()` — same interface as `GwClientPool`.

### Step A2: Add `GwSendComplete` to `OmsCommand` + handle in writer

```rust
OmsCommand::GwSendComplete { order_id, order_created_at_ms, t1_ns, t3_ns, t3r_ns }
```

Writer handles it by calling `tracker.record_order_sent(...)` and returning immediately (no OmsCore processing).

### Step A3: Replace inline GW sends in action dispatch (oms_actor.rs:298-322)

Four action match arms change from `gw_pool.*().await` to `gw_egress.send(gw_key, job)`. Remove direct `tracker.record_order_sent()` call (moved to egress task feedback).

### Step A4: Add `GwClientPool::take_clients()` method

Returns `HashMap<String, GatewayClient>` by draining the pool. `GwClientPool` stays around for `query_account_balance` at startup but is emptied after egress pool is built.

### Step A5: Wire in main.rs + update spawn_writer signature

After startup reconcile, before `spawn_writer`:
```rust
let gw_egress = GwEgressPool::from_client_pool(
    &mut gw_pool, cmd_tx.clone(), cfg.gw_egress_channel_buf, shutdown.clone(),
);
```
Pass `gw_egress` to `spawn_writer` (replaces `gw_pool` parameter).

### Reply semantics change

- gRPC reply returns after OmsCore processing (before GW send).
- `success=false` only if `try_send` fails (channel full = backpressure).
- GW-level gRPC errors logged in egress task, don't propagate to gRPC reply.
- Order stays in "Pending" state; GW report (or timeout → recheck) determines final state.

---

## Workstream B: Reduce Hot-Path Clones in OmsCore

### Context

PlaceOrder path clones `OrderRequest` defensively (oms_core.rs:191), then `resolve_context` clones `InstrumentRefData` (12 strings + HashMap), `GwConfigEntry` (8 strings), `OmsRouteEntry` (3 strings) from config maps. The full `OrderContext` is cloned into `context_cache` (oms_core.rs:324). `create_order` clones the completed `OmsOrder` into `order_dict` and returns a copy (order_mgr.rs:159). The `gw_req` is then cloned out of that copy (oms_core.rs:347-350).

### Files

| File | Changes |
|------|---------|
| `zk-oms-rs/src/config.rs` | Wrap config map values in `Arc` |
| `zk-oms-rs/src/models.rs` | `OrderContext` fields → `Arc<T>`; add `CachedOrderContext`; remove `gw_req`/`oms_req` from `OmsOrder` |
| `zk-oms-rs/src/oms_core.rs` | Remove defensive `req.clone()`; use Arc context; use `OrderPlacement` return |
| `zk-oms-rs/src/order_mgr.rs` | `create_order` returns `OrderPlacement`; remove `gw_req`/`oms_req` storage |

### Step B1: Arc-wrap config data in ConfdataManager

**File**: `zk-oms-rs/src/config.rs`

```rust
pub struct ConfdataManager {
    pub account_routes: HashMap<i64, Arc<OmsRouteEntry>>,
    pub gw_configs: HashMap<String, Arc<GwConfigEntry>>,
    pub refdata: HashMap<String, Arc<InstrumentRefData>>,
    pub trading_configs: HashMap<String, Arc<InstrumentTradingConfig>>,
    // ... derived maps also Arc values
}
```

`Arc::clone()` is ~1ns vs deep-clone of 12+ string allocations for `InstrumentRefData`.

### Step B2: OrderContext uses Arc refs + slim CachedOrderContext for context_cache

**File**: `zk-oms-rs/src/models.rs`

```rust
pub struct OrderContext {
    pub account_id: i64,
    pub fund_symbol: Option<String>,
    pub pos_symbol: Option<String>,
    pub route: Option<Arc<OmsRouteEntry>>,
    pub trading_config: Option<Arc<InstrumentTradingConfig>>,
    pub symbol_ref: Option<Arc<InstrumentRefData>>,
    pub gw_config: Option<Arc<GwConfigEntry>>,
    pub errors: Vec<String>,
}

/// Compact cached context for report/cancel paths.
/// Only fields actually read after order creation.
pub struct CachedOrderContext {
    pub account_id: i64,
    pub fund_symbol: Option<String>,
    pub pos_symbol: Option<String>,
    pub trading_config: Option<Arc<InstrumentTradingConfig>>,
}
```

The report path (oms_core.rs:700-744) only reads: `trading_config.bookkeeping_balance`, `trading_config.balance_check`, `trading_config.use_margin`, `fund_symbol`, `pos_symbol`, `account_id`. The `symbol_ref`, `gw_config`, `route` fields are never accessed from the cache on the report path.

Change `order_mgr.context_cache: HashMap<i64, OrderContext>` → `HashMap<i64, CachedOrderContext>`.

Update `process_order_report` and `process_cancel` to use `CachedOrderContext` fields.

Remove `order: Option<OmsOrder>` field from `OrderContext` — this was only used for `exch_order_ref` access which can be done via `order_mgr.get_order_by_id()` directly.

### Step B3: Remove defensive `req.clone()` in `process_order`

**File**: `zk-oms-rs/src/oms_core.rs`

Change `process_order_inner` to return `Result<Vec<OmsAction>, (String, OrderRequest)>` — on error, return the request back so the error handler can use it without the upfront clone.

```rust
fn process_order(&mut self, req: OrderRequest) -> Vec<OmsAction> {
    match self.process_order_inner(req) {
        Ok(actions) => actions,
        Err((e, req)) => { /* error handling using returned req */ }
    }
}
```

### Step B4: `create_order` returns `OrderPlacement`, not cloned `OmsOrder`

**File**: `zk-oms-rs/src/order_mgr.rs`

```rust
pub struct OrderPlacement {
    pub order_id: i64,
    pub gw_req: Option<ExchSendOrderRequest>,  // taken, not cloned
    pub created_at: i64,
    pub terminal: bool,
}
```

`create_order` builds `OmsOrder` with `gw_req: None` (the gw_req is built separately and returned in `OrderPlacement`), inserts into `order_dict`, returns `OrderPlacement`.

Then `process_order_inner` uses `OrderPlacement` fields directly:
- `SendOrderToGw` gets `placement.gw_req` (moved, not cloned)
- `PersistOrder` clones from `order_mgr.get_order_by_id()` (unavoidable — order must stay in dict AND go to Redis)
- Terminal check uses `placement.terminal` (no redundant lookup at oms_core.rs:365-369)

### Step B5: Remove unused `_ts` and redundant terminal lookup

**File**: `zk-oms-rs/src/oms_core.rs`

- Remove `_ts` computation at line 211-215 (unused variable).
- Terminal status from `OrderPlacement` eliminates `get_order_by_id` re-lookup at lines 365-369.

### Step B6: Remove `gw_req` and `oms_req` from live OmsOrder

**File**: `zk-oms-rs/src/models.rs` + `order_mgr.rs`

After order is placed:
- `gw_req` consumed by `SendOrderToGw` action — not stored in OmsOrder.
- `oms_req` only used by `resolve_context_for_order` for instrument code (oms_core.rs:1164), which has `order_state.instrument` as equivalent fallback.

Remove both fields from `OmsOrder`. This reduces per-order memory and eliminates the `gw_req.clone()` at line 347-350.

**Safe because**: `PersistedOrder` doesn't persist these fields. Cancel path uses `order.exch_order_ref` and `order_state` fields, not `gw_req`. `resolve_context_for_order` just uses `order_state.instrument` directly.

---

## Workstream C: Incremental Snapshot Publication for Positions/Balances

### Context

`publish_snapshot` (oms_actor.rs:354-378) clones the full managed/exchange position and balance maps every time they're dirty. These are `std::HashMap<i64, HashMap<String, T>>` — a deep clone that's O(total entries across all accounts). On every fill or balance update, this clones the entire position/balance state even though only one entry changed.

Orders already use `im::HashMap` (O(1) clone, structural sharing). Positions and balances should follow the same pattern.

### Current flow (oms_actor.rs:354-378)

```rust
fn publish_snapshot(..., balances_dirty, positions_dirty) {
    let (managed_pos, exch_pos) = if positions_dirty {
        (core.position_mgr.snapshot_managed(),   // clones entire HashMap
         core.position_mgr.snapshot_exch())       // clones entire HashMap
    } else {
        let prev = replica.load();
        (prev.managed_positions.clone(),          // clones entire HashMap anyway!
         prev.exch_positions.clone())
    };
    let exch_bal = if balances_dirty {
        core.balance_mgr.snapshot_exch_balances() // clones entire HashMap
    } else {
        let prev = replica.load();
        prev.exch_balances.clone()                // clones entire HashMap anyway!
    };
    let snap = writer.publish(managed_pos, exch_pos, exch_bal, ts);
    replica.store(Arc::new(snap));
}
```

### Design

Move positions and balances into `OmsSnapshotWriter` as `im::HashMap` fields with incremental `apply_*` methods — exactly mirroring how orders work today.

### Files

| File | Changes |
|------|---------|
| `zk-oms-rs/src/snapshot.rs` | Change `OmsSnapshot` position/balance fields to `im::HashMap`; add `apply_*` methods to writer |
| `zk-oms-svc/src/oms_actor.rs` | Apply incremental balance/position updates via writer; simplify `publish_snapshot` |

### Step C1: Change `OmsSnapshot` position/balance fields to `im::HashMap`

**File**: `zk-oms-rs/src/snapshot.rs`

```rust
pub struct OmsSnapshot {
    pub orders: ImHashMap<i64, OmsOrder>,
    pub open_order_ids_by_account: ImHashMap<i64, ImHashSet<i64>>,
    pub panic_accounts: ImHashSet<i64>,
    // CHANGED: std::HashMap → im::HashMap for O(1) clone
    pub managed_positions: ImHashMap<i64, ImHashMap<String, OmsManagedPosition>>,
    pub exch_positions: ImHashMap<i64, ImHashMap<String, ExchPositionSnapshot>>,
    pub exch_balances: ImHashMap<i64, ImHashMap<String, ExchBalanceSnapshot>>,
    pub seq: u64,
    pub snapshot_ts_ms: i64,
}
```

### Step C2: Add incremental `apply_*` methods to `OmsSnapshotWriter`

**File**: `zk-oms-rs/src/snapshot.rs`

Add fields + methods to writer:

```rust
pub struct OmsSnapshotWriter {
    orders: ImHashMap<i64, OmsOrder>,
    open_by_account: ImHashMap<i64, ImHashSet<i64>>,
    panic_accounts: ImHashSet<i64>,
    // NEW: incremental position/balance state
    managed_positions: ImHashMap<i64, ImHashMap<String, OmsManagedPosition>>,
    exch_positions: ImHashMap<i64, ImHashMap<String, ExchPositionSnapshot>>,
    exch_balances: ImHashMap<i64, ImHashMap<String, ExchBalanceSnapshot>>,
    seq: u64,
}

impl OmsSnapshotWriter {
    pub fn apply_balance(&mut self, account_id: i64, asset: &str, snap: &ExchBalanceSnapshot) {
        let acct = self.exch_balances.entry(account_id).or_default();
        acct.insert(asset.to_string(), snap.clone());
    }

    pub fn apply_managed_position(&mut self, account_id: i64, instrument: &str, pos: &OmsManagedPosition) {
        let acct = self.managed_positions.entry(account_id).or_default();
        acct.insert(instrument.to_string(), pos.clone());
    }

    pub fn apply_exch_position(&mut self, account_id: i64, instrument: &str, pos: &ExchPositionSnapshot) {
        let acct = self.exch_positions.entry(account_id).or_default();
        acct.insert(instrument.to_string(), pos.clone());
    }

    pub fn init_positions(
        &mut self,
        managed: HashMap<i64, HashMap<String, OmsManagedPosition>>,
        exch: HashMap<i64, HashMap<String, ExchPositionSnapshot>>,
        balances: HashMap<i64, HashMap<String, ExchBalanceSnapshot>>,
    ) {
        for (acct, instruments) in managed {
            let im_inner: ImHashMap<String, OmsManagedPosition> = instruments.into_iter().collect();
            self.managed_positions.insert(acct, im_inner);
        }
        // ... same for exch and balances
    }

    pub fn publish(&mut self, ts_ms: i64) -> OmsSnapshot {
        self.seq += 1;
        OmsSnapshot {
            orders: self.orders.clone(),                       // O(1)
            open_order_ids_by_account: self.open_by_account.clone(), // O(1)
            panic_accounts: self.panic_accounts.clone(),       // O(1)
            managed_positions: self.managed_positions.clone(), // O(1) — was O(n)
            exch_positions: self.exch_positions.clone(),       // O(1) — was O(n)
            exch_balances: self.exch_balances.clone(),         // O(1) — was O(n)
            seq: self.seq,
            snapshot_ts_ms: ts_ms,
        }
    }
}
```

Note: `publish()` signature changes — no longer takes position/balance maps as arguments.

### Step C3: Apply incremental updates in oms_actor action dispatch

**File**: `zk-oms-svc/src/oms_actor.rs`

```rust
OmsAction::PersistBalance { account_id, ref asset, ref snapshot } => {
    writer.apply_balance(account_id, asset, snapshot);  // NEW: incremental
    if let Err(e) = redis.write_balance(account_id, asset, snapshot).await {
        warn!(...);
    }
}
OmsAction::PersistPosition { account_id, ref instrument_code, ref side, ref position } => {
    writer.apply_managed_position(account_id, instrument_code, position);  // NEW: incremental
    if let Err(e) = redis.write_position(account_id, instrument_code, side, position).await {
        warn!(...);
    }
}
```

### Step C4: Simplify `publish_snapshot`

**File**: `zk-oms-svc/src/oms_actor.rs`

```rust
fn publish_snapshot(
    writer: &mut OmsSnapshotWriter,
    replica: &ReadReplica,
) {
    let snap = writer.publish(gen_timestamp_ms());  // O(1) — all im fields
    replica.store(Arc::new(snap));
}
```

No more `core` parameter needed. No more `balances_dirty` / `positions_dirty` flags.

### Step C5: Initialize writer with warm-start data

**File**: `zk-oms-svc/src/oms_actor.rs` (in `oms_writer_loop` startup)

```rust
let (initial_snap, mut writer) = core.take_snapshot();
writer.init_positions(
    core.position_mgr.snapshot_managed(),
    core.position_mgr.snapshot_exch(),
    core.balance_mgr.snapshot_exch_balances(),
);
```

### Step C6: Update gRPC query handlers for im::HashMap

**File**: `zk-oms-svc/src/grpc_handler.rs`

The `im::HashMap` API is identical to `std::HashMap` for reads (`.get()`, `.values()`, `.iter()`), so only import changes are needed.

### Impact

| Operation | Before | After |
|-----------|--------|-------|
| `publish_snapshot` (position dirty) | O(total managed + exch positions) deep clone | O(1) im::HashMap clone |
| `publish_snapshot` (balance dirty) | O(total balances) deep clone | O(1) im::HashMap clone |
| `publish_snapshot` (clean) | O(total) clone from prev snapshot | O(1) im::HashMap clone |
| Per balance/position update | Single entry write + full map clone | Single entry write (O(log32 n)) |
| gRPC queries | Unchanged | Unchanged |

---

## Expected Impact Summary

| Change | Eliminated | Est. savings |
|--------|-----------|-------------|
| GW egress decoupling (A) | ~500µs+ GW gRPC RTT under load | **Dominant win** |
| Arc config (B1-B2) | ~26 string allocs in resolve_context | ~200-400ns |
| Remove req.clone (B3) | 1 OrderRequest clone | ~50-100ns |
| OrderPlacement (B4) | OmsOrder clone + gw_req clone | ~100-200ns |
| Remove stored reqs (B6) | gw_req clone, smaller OmsOrder | ~50-100ns |
| Incremental snapshots (C) | O(n) full map clone per fill/balance update | O(1) structural sharing |

## Verification

```bash
# Compile
cargo check -p zk-oms-rs
cargo check -p zk-oms-svc
cargo check -p zk-backtest-rs

# Unit tests (parity tests validate OmsCore logic unchanged)
cargo test -p zk-oms-rs
cargo test -p zk-backtest-rs

# E2E bench
docker exec zk-dev-redis-1 redis-cli flushall
make oms-e2e-bench
# Expect: t3-t1 p50 < 100µs (was ~670µs)
# Expect: fill path latency reduced (no full map clone per fill)
```
