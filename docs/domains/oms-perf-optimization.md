# OMS Performance Optimization

## Purpose

This document defines a concrete performance-oriented refactor plan for the Rust OMS core and service path.

The goal is to reduce per-order latency and tail jitter in the current OMS hot path, with particular focus on:

- `zk-oms-rs` core order placement and report handling
- `zk-oms-svc` writer-task dispatch path
- data layout inherited from the original Python implementation

This note is not a generic optimization wishlist. It is based on the current code shape in:

- `rust/crates/zk-oms-rs/src/oms_core.rs`
- `rust/crates/zk-oms-rs/src/order_mgr.rs`
- `rust/crates/zk-oms-rs/src/models.rs`
- `rust/crates/zk-oms-svc/src/oms_actor.rs`
- `rust/crates/zk-oms-svc/src/latency.rs`

## Problem Statement

The current Rust OMS was migrated from a Python design where large objects were cheap to pass around because most operations moved references, not owned copies.

In the Rust implementation, several hot-path structures are owned values containing:

- `String`
- protobuf-generated structs
- nested `Vec`
- cloned config / refdata payloads

As a result, the OMS place/report path pays real costs for:

- cloning request and order payloads
- cloning resolved context/config on every order
- constructing large event snapshots eagerly
- re-looking up data that was just computed
- cloning state again when publishing to the read replica

These costs are not catastrophic individually, but they are on the per-order path and therefore directly affect:

- `oms_through = t3 - t1`
- p50 latency under steady load
- p99 latency during bursts
- cache locality and allocator pressure

## Current Hot Path Review

## Place order path

Current place-order flow:

1. gRPC handler receives `PlaceOrderRequest`
2. writer task converts command to `OmsMessage::PlaceOrder`
3. `OmsCore::process_order` clones `OrderRequest`
4. `OmsCore::resolve_context` clones refdata/config structures
5. `OrderManager::create_order` clones `OrderRequest` into `OmsOrder`
6. `OrderManager::create_order` inserts a cloned `OmsOrder` into `order_dict`
7. `OmsCore` clones `OrderContext` into `context_cache`
8. `OmsCore` clones `gw_req` out of `OmsOrder` to build `SendOrderToGw`
9. `OmsCore` returns `PersistOrder(Box<OmsOrder>)`
10. writer task applies the persisted order into the snapshot writer, which clones again

This shape means the place-order path currently does more ownership work than actual business logic.

### Main current costs

#### 1. Duplicated order ownership

`OmsOrder` is built once but then immediately exists in multiple owned forms:

- inside `order_dict`
- inside `PersistOrder`
- inside the read replica snapshot

This is convenient, but expensive.

#### 2. Heavy `OrderContext`

`OrderContext` currently carries cloned config/refdata payloads rather than cheap handles or compact derived fields.

This means each order pays to clone:

- `InstrumentRefData`
- `OmsRouteEntry`
- `GwConfigEntry`
- `InstrumentTradingConfig`
- several derived strings

#### 3. Stored cold data inside hot live order state

`OmsOrder` contains:

- `oms_req`
- `gw_req`
- `cancel_req`
- `trades`
- `order_inferred_trades`
- `exec_msgs`
- `fees`

Not all of this belongs in the hot mutable state used for every request.

#### 4. Eager snapshot/event materialization

`OrderUpdateEvent` currently clones:

- full `order_state`
- optional last trade
- optional inferred trade
- optional fee
- optional exec message

This is correct for downstream consumers, but expensive when done eagerly on every report.

#### 5. Writer loop serializes non-core I/O

The single OMS writer awaits downstream gateway gRPC in-line.

That means:

- one slow gateway call stalls later commands
- queue wait becomes part of `oms_through`
- core compute cost and transport cost are mixed in one serialized path

## Report path

The report path has similar issues:

- repeated string cloning around `gw_key` and `exch_order_ref`
- cloned `Trade`, `Fee`, and `ExecMessage` into event payloads
- cloned order snapshots for publish
- cloned order again for persistence

The report path is also where reconciliation, reservations, and state publication converge, so reducing data movement here matters for both placement and post-trade lifecycle latency.

## Design Goals

The target design should:

- minimize owned heap data in the hot mutable OMS state
- use copy-cheap identifiers in the core path
- keep full payload reconstruction at the boundary
- preserve correctness and service semantics
- support incremental migration
- keep observability good enough for debugging and audit

More specifically, the design should:

- reduce clone count on place-order path
- reduce hash cost by preferring integer keys over strings where practical
- improve cache locality of live order state
- separate hot state from cold audit/history payloads
- allow explicit capacity planning for hot maps

## Non-Goals

This design does not require:

- immediate protobuf contract redesign
- replacing all existing `String` fields in one step
- removing the single-writer OMS model
- changing OMS correctness semantics beyond separately-tracked bug fixes

This design also does not assume:

- lock-free multi-writer OMS mutation
- unsafe memory tricks
- arena allocation across the entire service

## Decision Summary

### 1. Use integer IDs in core state, not full cloned metadata

Hot OMS state should move from owned domain payloads toward compact IDs and derived flags.

Core rule:

- hot path uses IDs
- boundary code materializes full structs only when publishing or persisting

Decision:

- use plain integer IDs rather than thin newtypes in the first implementation

Rationale:

- lowest friction for migration from current code
- simplest hashing and storage model
- easiest interop with existing account/order integer fields
- avoids extra adapter noise while the refactor is still moving quickly

Note:

- this is a pragmatism decision, not a statement that newtypes are bad
- if accidental cross-table misuse becomes a real maintenance problem later, thin newtypes can still be introduced after the data model stabilizes

### 2. Split hot state from cold state

Live order state should contain only the fields needed for:

- routing
- risk checks
- state transitions
- reservations
- position updates
- snapshot identity

Cold or append-only audit data should move out of the main hot mutable struct.

### 3. Replace heavy `OrderContext` with compact resolved metadata

The current cloned `OrderContext` should be replaced by a compact resolved structure that stores only what the hot path needs.

### 4. Keep immutable reference/config tables outside the hot objects

Static or rarely-changing config/refdata should live in lookup tables owned by `ConfdataManager`.

Live orders should point into these tables via IDs or interned handles.

Decision:

- instrument codes and exchange symbols should be interned
- avoid `Arc<str>` on the hot path unless a specific boundary requires shared ownership

Rationale:

- these strings are highly repetitive and stable across many orders
- interning removes repeated heap allocation and repeated string clone work
- the core path does not need atomic refcount overhead for stable metadata

Rule of thumb:

- stable refdata/config strings: intern
- dynamically-created identifiers such as exchange order refs: do not force them into the same global intern table by default

For dynamic IDs:

- keep a dedicated lightweight owned representation
- if they are repeatedly referenced in hot live state, use an OMS-local symbol table or side table
- do not default to `Arc<str>` as the main representation

### 5. Target a clean rewrite, not a staged migration

This effort should target the end-state design directly rather than preserving the current Python-shaped Rust structures and gradually trimming them.

Rationale:

- nothing is in production yet
- there is no need to preserve an inefficient internal shape for rollout safety
- a staged migration would duplicate work by forcing temporary adapters and half-optimized intermediate states
- the current hot-path representation is wrong enough that patching around it is lower value than replacing it

Practical implication:

- implement the target data model directly
- keep external protobuf/API contracts stable where useful
- do not preserve current internal structs unless they still fit the target design

## Target Data Model

## Writer-Owned Data Structures

The OMS writer should own the canonical mutable in-memory state.

This state should be optimized for:

- mutation cost
- cache locality
- low clone/copy overhead
- direct use in validation, state transition, reservation, and routing logic

The writer-owned state should not be optimized for:

- direct protobuf serialization
- direct external readability
- preserving the old Python object shape

### Top-level writer state

Suggested top-level shape:

```rust
struct OmsState {
    metadata: MetadataTables,
    orders: OrderStore,
    reservations: ReservationStore,
    positions: PositionStore,
    balances: BalanceStore,
    pending_reports: PendingReportStore,
    panic_accounts: HashSet<i64>,
    retry_state: RetryState,
}
```

Suggested responsibilities:

- `metadata`:
  - immutable or generation-scoped lookup tables
- `orders`:
  - OMS-owned live order state and cold detail logs
- `reservations`:
  - OMS-local buy/sell protection ledger
- `positions`:
  - OMS-owned executable exposure
- `balances`:
  - exchange-owned balance cache
- `pending_reports`:
  - out-of-order report buffering by gateway/exchange order ref
- `panic_accounts`:
  - trading blocklist
- `retry_state`:
  - order/cancel recheck counters and policy

### Metadata tables

Suggested shape:

```rust
struct MetadataTables {
    instrument_by_code: HashMap<InternedStrId, u32>,
    instrument_by_gw_symbol: HashMap<(u32, InternedStrId), u32>,
    instruments: Vec<InstrumentMeta>,

    route_by_account: HashMap<i64, RouteMeta>,
    gw_by_key: HashMap<InternedStrId, u32>,
    gws: Vec<GwMeta>,

    asset_by_symbol: HashMap<InternedStrId, u32>,
    assets: Vec<AssetMeta>,

    trading_by_instrument: HashMap<u32, TradingMeta>,

    strings: InternTable,
}
```

Key rule:

- all stable config/refdata strings should enter through `InternTable`
- hot live state should carry only integer IDs derived from these tables

#### Stable-handle container pattern (`slotmap` / generational arena)

For the metadata layer, a generic stable-handle container can be a good fit.

Generic pattern:

- store values in slots
- access them through opaque handles instead of raw vector indices
- keep handles stable across insert/grow
- invalidate removed entries safely with generation/version checks

This pattern appears in Rust as:

- `slotmap`
- generational arena
- stable-handle arena

Why it fits metadata well:

- metadata entities such as instruments, gateways, and assets have stable identity
- the reload path needs identity to remain stable even when config iteration order changes
- it avoids accidental coupling between "ID" and current vector position

Recommended OMS use:

- strong fit for `MetadataTables`
- possible future fit for a unified order record store
- not needed for reservation, balance, position, or snapshot index maps

Pragmatic rule:

- if metadata continues to use plain `u32` IDs, the implementation must preserve ID stability explicitly across reload
- if that becomes too error-prone, transition the metadata entity tables to a stable-handle container

Performance note:

- this is primarily a correctness and maintainability tool for stable identity
- it is not automatically faster than plain `u32` vector indexing on the hot path
- use it where stable identity matters more than raw lookup micro-cost

### Intern table

Suggested shape:

```rust
struct InternTable {
    ids_by_str: HashMap<Box<str>, u32>,
    strs_by_id: Vec<Box<str>>,
}
```

Rules:

- intended for stable or frequently-reused strings
- no `Arc` by default
- intern once on config load or boundary decode
- use integer `InternedStrId` in hot state

Suggested use:

- instrument codes
- gateway keys
- exchange symbols
- assets
- exchange account codes
- source IDs if repeated and bounded

Do not automatically intern:

- every unique exchange-generated order reference
- every transient error message
- one-off dynamic payload fragments

Those should use a separate side-table or owned payload slot only where needed.

### Order store

Suggested shape:

```rust
struct OrderStore {
    live_by_order_id: HashMap<i64, LiveOrder>,
    open_order_ids_by_account: HashMap<i64, HashSet<i64>>,
    order_id_by_exch_ref: HashMap<(u32, DynStrId), i64>,
    external_order_ids_by_exch_ref: HashMap<(u32, DynStrId), i64>,
    detail_by_order_id: HashMap<i64, OrderDetailLog>,
    closed_lru: VecDeque<i64>,
}
```

Optional stable-handle variant:

```rust
struct OrderStore {
    orders: SlotMap<OrderKey, OrderRecord>,
    order_key_by_order_id: HashMap<i64, OrderKey>,
    open_order_keys_by_account: HashMap<i64, HashSet<OrderKey>>,
    order_key_by_exch_ref: HashMap<(u32, DynStrId), OrderKey>,
    external_order_keys_by_exch_ref: HashMap<(u32, DynStrId), OrderKey>,
    closed_lru: VecDeque<OrderKey>,
}
```

Where:

- `OrderKey` is an internal stable handle
- external `order_id: i64` remains the business identifier at API and persistence boundaries

Use this only if the implementation benefits from:

- unifying hot and cold order storage under one internal handle
- reducing duplicated `order_id` hashing across multiple side tables
- cleaner internal references for future side structures

Do not assume this is a free latency win.

Rule:

- metadata is the best candidate for stable-handle containers
- order storage is an optional second candidate
- tuple-key accounting stores and read-side query indexes should remain direct maps

#### `LiveOrder`

Suggested shape:

```rust
struct LiveOrder {
    order_id: i64,
    account_id: i64,
    instrument_id: u32,
    gw_id: u32,
    route_exch_account_sym: u32,
    source_sym: u32,

    order_status: i32,
    buy_sell_type: i32,
    open_close_type: i32,
    order_type: i32,
    tif_type: i32,

    price: f64,
    qty: f64,
    filled_qty: f64,
    filled_avg_price: f64,

    exch_order_ref_id: Option<DynStrId>,
    snapshot_version: u32,
    created_at: i64,
    updated_at: i64,

    is_external: bool,
    cancel_attempts: u32,

    accounting_flags: OrderAccountingFlags,
}
```

This is the main hot mutable order state.

It should not embed:

- full `OrderRequest`
- full gateway request
- vectors of trades/fees/messages
- cloned route/refdata/config payloads

#### `OrderDetailLog`

Suggested shape:

```rust
struct OrderDetailLog {
    order_id: i64,

    original_req: Option<Box<OrderRequest>>,
    last_gw_req: Option<Box<SendOrderRequest>>,
    cancel_req: Option<Box<CancelOrderRequest>>,

    trades: Vec<Trade>,
    inferred_trades: Vec<Trade>,
    exec_msgs: Vec<ExecMessage>,
    fees: Vec<Fee>,
}
```

Decision:

- this remains structurally intact and always present for OMS-managed orders
- but it is a cold side structure keyed by `order_id`, not embedded in `LiveOrder`

If later profiling shows the boxed request payloads are still too heavy, these can move to request-payload side tables without changing the hot state shape.

### Reservation store

Suggested shape:

```rust
struct ReservationStore {
    reservation_by_order_id: HashMap<i64, ReservationRecord>,
    total_reserved_by_account_asset: HashMap<(i64, u32), f64>,
    total_reserved_by_account_instrument: HashMap<(i64, u32), f64>,
}
```

Suggested `ReservationRecord`:

```rust
struct ReservationRecord {
    order_id: i64,
    account_id: i64,
    reserve_kind: ReserveKind,
    target_id: u32,
    reserved_qty: f64,
}
```

Where:

- `ReserveKind::CashAsset`
- `ReserveKind::InventoryInstrument`

This removes symbol-string hashing from the hot reservation path.

### Position store

Suggested shape:

```rust
struct PositionStore {
    managed_by_account_instrument: HashMap<(i64, u32), ManagedPosition>,
    exch_by_account_instrument: HashMap<(i64, u32), ExchPositionSnapshot>,
}
```

#### `ManagedPosition`

Suggested shape:

```rust
struct ManagedPosition {
    account_id: i64,
    instrument_id: u32,
    instrument_type: i32,
    is_short: bool,
    qty_total: f64,
    qty_frozen: f64,
    qty_available: f64,
    last_local_update_ts: i64,
    last_exch_sync_ts: i64,
    reconcile_status: ReconcileStatus,
    last_exch_qty: f64,
    first_diverged_ts: i64,
    divergence_count: u32,
}
```

Rule:

- managed positions are OMS-owned compact state
- instrument names are reconstructed via metadata when needed

### Balance store

Suggested shape:

```rust
struct BalanceStore {
    exch_by_account_asset: HashMap<(i64, u32), ExchBalanceSnapshot>,
    account_id_by_exch_account_sym: HashMap<u32, i64>,
}
```

Rule:

- exchange-owned cache may store protobuf or protobuf-adjacent shape directly
- no need to over-optimize this into a non-proto representation in the first rewrite

### Pending report store

Suggested shape:

```rust
struct PendingReportStore {
    reports_by_gw_exch_ref: HashMap<(u32, DynStrId), Vec<OrderReport>>,
}
```

Rule:

- buffered reports are not on the steady-state happy path
- storing the raw protobuf `OrderReport` is acceptable here
- optimize correctness and simplicity first

### Retry state

Suggested shape:

```rust
struct RetryState {
    total_retries_orders: u32,
    total_retries_cancels: u32,
}
```

## Reader-Visible Data Structures

The reader-visible state should be optimized for:

- lock-free or low-contention reads
- query response assembly
- clear separation between OMS-owned internal compact state and exchange-owned cached payloads

The reader-visible state does not need to be identical to the writer-owned state.

### Reader-visible snapshot

Suggested top-level shape:

```rust
struct OmsSnapshot {
    metadata: Arc<SnapshotMetadata>,
    orders: im::HashMap<i64, SnapshotOrder>,
    order_details: im::HashMap<i64, SnapshotOrderDetail>,
    open_order_ids_by_account: im::HashMap<i64, im::HashSet<i64>>,
    panic_accounts: im::HashSet<i64>,

    managed_positions: HashMap<(i64, u32), SnapshotManagedPosition>,
    exch_positions: HashMap<(i64, u32), ExchPositionSnapshot>,
    exch_balances: HashMap<(i64, u32), ExchBalanceSnapshot>,

    seq: u64,
    snapshot_ts_ms: i64,
}
```

### Snapshot metadata

Suggested shape:

```rust
struct SnapshotMetadata {
    instrument_names: Vec<Box<str>>,
    asset_names: Vec<Box<str>>,
    gw_names: Vec<Box<str>>,
    exch_account_names: Vec<Box<str>>,
    source_names: Vec<Box<str>>,
}
```

Rule:

- snapshot readers should be able to reconstruct strings without touching writer-owned mutable state
- metadata can be shared across snapshots by `Arc`

### Snapshot order

Suggested shape:

```rust
struct SnapshotOrder {
    order_id: i64,
    account_id: i64,
    instrument_id: u32,
    gw_id: u32,
    source_sym: u32,
    order_status: i32,
    buy_sell_type: i32,
    open_close_type: i32,
    price: f64,
    qty: f64,
    filled_qty: f64,
    filled_avg_price: f64,
    exch_order_ref: Option<Box<str>>,
    created_at: i64,
    updated_at: i64,
    snapshot_version: u32,
    is_external: bool,
}
```

Rule:

- this is still compact internal reader state, not protobuf
- gRPC query handlers convert this into protobuf on demand
- trade/history payloads do not live here

### Snapshot order detail

Suggested shape:

```rust
struct SnapshotOrderDetail {
    order_id: i64,
    source_sym: u32,
    instrument_exch_sym: u32,
    exch_order_ref: Option<Box<str>>,
    error_msg: Box<str>,
    trades: Vec<Trade>,
    inferred_trades: Vec<Trade>,
    exec_msgs: Vec<ExecMessage>,
    fees: Vec<Fee>,
}
```

Rule:

- this is the reader-visible cold side structure keyed by `order_id`
- query handlers that need richer order/trade detail read from this map
- `SnapshotOrder` remains compact even if detailed order history grows
- order-detail clones happen only when detail changes, not on every snapshot order mutation

Intended use:

- `query_trade_details`
- `query_order_details` fields that require exchange refs, source IDs, or error details
- future audit/debug views

Not intended use:

- hot-path routing or order-state lookups
- open-order scans where only core order state is needed

### Snapshot managed position

Suggested shape:

```rust
struct SnapshotManagedPosition {
    account_id: i64,
    instrument_id: u32,
    instrument_type: i32,
    is_short: bool,
    qty_total: f64,
    qty_frozen: f64,
    qty_available: f64,
    last_local_update_ts: i64,
    last_exch_sync_ts: i64,
    reconcile_status: ReconcileStatus,
}
```

Rule:

- still compact
- query layer reconstructs `Position` protobuf when needed

### Exchange-owned snapshot cache

For exchange-owned caches:

- `ExchPositionSnapshot` can remain protobuf-shaped
- `ExchBalanceSnapshot` can remain protobuf-shaped

Rationale:

- these are already boundary-originated payloads
- they are not the main internal hot-state optimization target
- query handlers often want them close to wire shape already

## Writer/Reader Boundary Rules

### Writer-owned canonical state

Writer-owned state is authoritative for:

- live order state
- reservations
- managed positions
- pending reports
- panic mode

### Reader-visible snapshot state

Reader-visible snapshot state is optimized for:

- query assembly
- immutable sharing
- fast read access

It is not responsible for:

- order mutation
- reservation mutation
- reconciliation mutation

### Conversion rules

- writer hot state -> reader snapshot:
  - compact internal forms
- reader snapshot -> gRPC query response:
  - reconstruct protobuf on demand
- exchange inbound cache -> writer and reader:
  - store protobuf-shaped cache as-is where practical

## Core identifiers

Introduce cheap identifiers for hot-path entities.

Suggested first set:

- `InstrumentId(u32)`
- `GwId(u16)` or `GwId(u32)`
- `AccountId(i64)` remains as-is
- `AssetId(u32)`
- `ExchSymbolId(u32)` if needed

Rules:

- IDs must be stable for a given loaded config generation
- IDs are local OMS-process identifiers, not externally-visible API values
- conversion from external string/proto fields happens once at ingress or config-load time
- IDs are plain integers in the first implementation

## Instrument metadata table

Replace repeated cloning of `InstrumentRefData` with an immutable metadata table.

Suggested shape:

```rust
struct InstrumentMeta {
    instrument_id: InstrumentId,
    instrument_code_sym: u32,
    instrument_exch_sym: u32,
    instrument_type: i32,
    base_asset_id: Option<AssetId>,
    quote_asset_id: Option<AssetId>,
    settlement_asset_id: Option<AssetId>,
    qty_precision: i32,
    price_precision: i32,
}
```

Lookup views:

- `instrument_code -> InstrumentId`
- `(gw_id, exch_symbol) -> InstrumentId`
- `InstrumentId -> InstrumentMeta`

String storage rule:

- `instrument_code` and `instrument_exch` should come from an intern table
- hot live state should carry IDs, not cloned string payloads
- string reconstruction should happen only at API/persistence/publish boundaries

## Route metadata table

Replace per-order route cloning with an immutable route table.

Suggested shape:

```rust
struct RouteMeta {
    account_id: i64,
    gw_id: GwId,
    gw_key_sym: u32,
    exch_account_sym: u32,
}
```

Lookup:

- `account_id -> RouteMeta`

## Trading config table

Replace cloned per-order trading config with a compact immutable table.

Suggested shape:

```rust
struct TradingMeta {
    instrument_id: InstrumentId,
    bookkeeping_balance: bool,
    balance_check: bool,
    use_margin: bool,
    max_order_size: Option<f64>,
}
```

Lookup:

- `InstrumentId -> TradingMeta`

## Compact resolved context

Replace the current `OrderContext` in the hot path with a compact `ResolvedOrderMeta`.

Suggested shape:

```rust
struct ResolvedOrderMeta {
    account_id: i64,
    gw_id: GwId,
    instrument_id: InstrumentId,
    fund_asset_id: Option<AssetId>,
    pos_instrument_id: Option<InstrumentId>,
    bookkeeping_balance: bool,
    balance_check: bool,
    use_margin: bool,
    max_order_size: Option<f64>,
}
```

This should be cacheable by `order_id` if needed, but should not contain large cloned config payloads.

## Split order state

Replace the current all-in-one `OmsOrder` with a split model.

### Hot live order state

Suggested shape:

```rust
struct LiveOrder {
    order_id: i64,
    account_id: i64,
    instrument_id: InstrumentId,
    gw_id: GwId,
    source_id: SourceId,
    order_status: i32,
    buy_sell_type: i32,
    open_close_type: i32,
    price: f64,
    qty: f64,
    filled_qty: f64,
    filled_avg_price: f64,
    created_at: i64,
    updated_at: i64,
    snapshot_version: u32,
    exch_order_ref_id: Option<u64>,
    is_external: bool,
    cancel_attempts: u32,
}
```

This struct should be small, cache-friendly, and free of large nested payloads.

### Cold audit/order detail state

Suggested shape:

```rust
struct OrderDetailLog {
    order_id: i64,
    original_req_id: Option<u64>,
    last_gw_req_id: Option<u64>,
    cancel_req_id: Option<u64>,
    trades: Vec<Trade>,
    inferred_trades: Vec<Trade>,
    exec_msgs: Vec<ExecMessage>,
    fees: Vec<Fee>,
}
```

Decision:

- cold order detail should remain structurally intact
- but the structure should refer to large request/payload data through IDs or side-table references rather than embedding duplicated owned payloads into hot state

That means:

- keep the order-detail concept always-on
- do not lazily invent it later on first trade/error
- but keep heavyweight request/message payloads referenced indirectly

The main objective is:

- preserve audit/readability semantics
- while keeping the hot state-transition struct compact

## Boundary reconstruction

At publish/persist boundaries:

- reconstruct proto `Order`
- reconstruct `OrderUpdateEvent`
- reconstruct gateway requests

This keeps external behavior stable while reducing internal hot-path copying.

## Target Control Flow

## Place order

Target place-order flow:

1. ingress validates external payload
2. resolve `instrument_code -> InstrumentId`
3. resolve `account_id -> RouteMeta`
4. build compact `ResolvedOrderMeta`
5. mutate `LiveOrder` in-place
6. emit lightweight action payloads:
   - `SendOrderToGw { gw_id, order_id, ...compact fields... }`
   - `PersistOrder { order_id }`
7. service boundary reconstructs full publish/persist structs as required

Key property:

- no cloned full `OrderContext`
- no cloned full `OmsOrder` on the happy path

## Report handling

Target report flow:

1. resolve order via `order_id` or `(gw_id, exch_order_ref)`
2. mutate `LiveOrder`
3. append audit payload only if present
4. update reservation / position / balance managers using IDs or compact fields
5. construct publish event only once at boundary

Key property:

- mutate one live order record
- avoid multiple copies of the same order snapshot

## Proposed Action Refactor

Current `OmsAction` variants often carry large owned payloads.

Target direction:

- hot actions should carry IDs plus compact data
- boundary layer should expand into full protobuf payloads only when required

Decision:

- do not defer action slimming behind the hot/cold order split
- slim `OmsAction` as part of the same refactor stream

Rationale:

- carrying full payloads inside `OmsAction` is itself one of the current hot-path costs
- keeping fat actions while slimming state would preserve a major copy boundary
- action slimming and order-state slimming reinforce each other and should be designed together

Example direction:

```rust
enum OmsAction {
    SendOrderToGw {
        gw_id: GwId,
        order_id: i64,
        instrument_id: InstrumentId,
        price: f64,
        qty: f64,
        buy_sell_type: i32,
        open_close_type: i32,
        order_created_at: i64,
    },
    PersistOrder {
        order_id: i64,
        set_expire: bool,
        set_closed: bool,
    },
    PublishOrderUpdate {
        order_id: i64,
        include_last_trade: bool,
        include_last_fee: bool,
        include_exec_message: bool,
        include_inferred_trade: bool,
    },
}
```

This is not mandatory in phase 1, but it is the cleanest end-state.

## Capacity Planning

Pre-allocation is not the main optimization, but it is still useful.

The refactored design should explicitly pre-size hot maps using config-driven estimates.

Suggested configurable capacities:

- `max_live_orders`
- `max_cached_closed_orders`
- `max_pending_reports`
- `max_accounts`
- `max_assets_per_account`
- `max_positions_per_account`

Priority pre-sized structures:

- `order_dict`
- `open_order_ids`
- `context_cache` or replacement resolved-meta cache
- `exch_ref_to_order_id`
- `pending_order_reports`
- reservation maps

Expected effect:

- smaller warm-up penalty
- fewer resize spikes under burst load
- modest p50 improvement
- more meaningful p99 improvement

## Writer / Service Considerations

## Current issue

The OMS writer currently awaits gateway gRPC directly in the single-writer loop.

This is simple and correct, but it means:

- core logic and downstream I/O are serialized
- queue wait is added to `oms_through`
- one slow gateway blocks unrelated commands

## Target direction

After hot data-path cleanup, the next service-layer optimization should be parallel action execution with a single-writer core.

Core rule:

- state mutation stays inside the writer-owned core
- action execution happens after `process_message` returns
- side effects are dispatched to bounded async executors
- ordering is preserved per entity or per sink key, not globally

This keeps correctness anchored in the single writer while removing unnecessary I/O wait from the mutation loop.

### Design principle

The OMS writer should be treated as:

- the only place that mutates canonical state
- the only place that decides action order
- the source of truth for per-command sequencing

The writer should not also be the place that:

- waits on gateway round trips
- waits on Redis persistence
- waits on NATS publish completion

Instead:

- the writer computes actions
- the writer attaches enough ordering metadata to those actions
- executor workers perform the I/O in parallel

### Action classes

Actions emitted by the core should be divided into execution classes.

Suggested classes:

- `GatewayAction`
  - send order
  - cancel order
  - batch send
  - batch cancel
- `PersistAction`
  - persist order
  - persist balance
  - persist position
- `PublishAction`
  - publish order update
  - publish balance update
  - publish position update
- `LocalAction`
  - snapshot update
  - latency bookkeeping
  - immediate reply bookkeeping

Rule:

- `LocalAction` remains inside the writer loop
- the other three classes are candidates for async execution

### Action envelope

Each async action should be wrapped in a small execution envelope.

Suggested shape:

```rust
struct ActionEnvelope<A> {
    global_seq: u64,
    action_seq_in_cmd: u16,
    emitted_at_ns: i64,
    ack_group: Option<u64>,
    shard_key: u64,
    payload: A,
}
```

Meaning:

- `global_seq`:
  - monotonic sequence assigned by the writer
  - useful for debugging and failure tracing
- `action_seq_in_cmd`:
  - preserves the original intra-command action order
- `emitted_at_ns`:
  - latency accounting at enqueue time
- `ack_group`:
  - optional handle used when a caller must wait for a specific action completion
- `shard_key`:
  - precomputed worker routing key

The action payload itself should already be compact and executor-ready.

### Executor topology

Suggested service layout:

```rust
single_writer_core
    -> gw_executor_pool
    -> persist_executor_pool
    -> publish_executor_pool
```

Each executor pool should be:

- bounded
- sequential per shard
- parallel across shards

This means the service should prefer:

- `N` shard queues with one worker per queue

instead of:

- one giant unordered worker pool

because correctness still depends on per-key ordering.

### Recommended shard keys

#### Gateway executor

Recommended key:

- `order_id` hashed into a configurable number of gateway-executor shards per gateway

Suggested shape:

- `gateway_shard = hash(order_id) % gw_executor_shards_per_gateway`
- routing key = `(gw_id, gateway_shard)`

Rationale:

- current single-FIFO-per-`gw_id` execution becomes the dominant bottleneck once the core hot path is slimmed down
- place and cancel for the same order naturally stay ordered when both route by `order_id`
- the shard count can be tuned per deployment without changing the core ownership model

Required constraint:

- OMS may only use `order_id`-sharded gateway execution if the gateway service contract is also `accepted and queued internally`, not `already sent to venue`

Why that constraint matters:

- once ACK means internal queue acceptance, OMS does not need one globally ordered outbound stream per `gw_id`
- the gateway remains responsible for any venue-specific internal ordering or rate limiting behind its own queue

Tradeoff:

- `gw_id` sharding is simpler and preserves one outbound FIFO per gateway
- `order_id` sharding provides materially more concurrency and is the better long-term shape once ACK semantics are queue-based

Recommended implementation:

- make gateway-executor shard count configurable
- default routing to `order_id` hashing
- keep a fallback mode that routes only by `gw_id` if a venue requires strict single-stream semantics

#### Persist executor

Suggested shard keys:

- orders: `order_id`
- balances: `account_id`
- positions: `account_id`

Practical simplification:

- use `account_id` as the first persist shard key

Rationale:

- it preserves order for all writes related to one account
- it avoids overtaking between position and balance persistence for the same account
- it is simpler than maintaining different pools per entity type

#### Publish executor

Suggested shard keys:

- order updates: `order_id`
- balance updates: `account_id`
- position updates: `account_id`

Practical simplification:

- use `account_id` for balance/position publish
- use `order_id` for order update publish

If a single publish pool is used, the routing function should still preserve those logical order domains.

### Ordering rules

Global ordering across all actions is not required.

Required ordering is local:

- same order:
  - order persistence must remain ordered
  - order publish events must remain ordered
  - cancel/send actions must not overtake earlier actions for that order
- same account:
  - balance updates must remain ordered
  - position updates must remain ordered
  - account-scoped persistence should not overtake itself
- same gateway shard:
  - outbound RPC stream should remain ordered within the selected shard
- same order:
  - all send/cancel actions for that order must route to the same gateway shard

Examples of invalid reordering:

- `PersistOrder(closed)` before `PersistOrder(open)` for the same `order_id`
- `PublishOrderUpdate(fill2)` before `PublishOrderUpdate(fill1)` for the same `order_id`
- `SendCancelToGw` before the corresponding `SendOrderToGw` for the same order

Examples of acceptable parallelism:

- account A publish running while account B persist runs
- gateway X shard 1 send running while gateway X shard 2 send runs
- gateway X send running while gateway Y cancel runs
- order 1 publish running while order 2 publish runs

### Snapshot update policy

The read replica snapshot should remain writer-owned.

Rule:

- do not move snapshot mutation into async workers

Reason:

- snapshot state is part of the reader-consistency model
- letting publish/persist workers mutate it would create multiple writers and ordering ambiguity

Therefore:

- writer mutates snapshot incrementally based on actions
- writer publishes snapshot immediately after local state transition
- async workers only perform external side effects

This means:

- readers observe OMS-internal state promptly
- external persistence/publish may lag slightly behind
- lag is bounded and measurable

### ACK semantics

Parallel action execution changes the point at which the service can safely reply.

This must be an explicit contract.

Required contract change:

- OMS place/cancel success means: request was validated and accepted for asynchronous processing by OMS
- gateway place/cancel success means: request was validated and accepted for asynchronous processing by the gateway
- neither ACK means the order was already enqueued, sent to the venue, or accepted by the venue

Recommended ACK classes:

- `AcceptedForAsyncProcessing`
  - core validated the request
  - service accepted the request for asynchronous processing
- `Persisted`
  - persistence action completed

Not recommended as the primary synchronous contract:

- `SentToGateway`
- `SentToVenue`
- `VenueAccepted`

Recommended first implementation:

- place/cancel gRPC reply returns after OMS validates the request and accepts it for asynchronous processing
- OMS to gateway gRPC returns after the gateway validates the request and accepts it for asynchronous processing
- persistence and publish remain asynchronous side effects with explicit durability/error policy

Implication:

- validation failure is synchronous and fails the caller
- internal queue overflow or send failure after ACK is asynchronous and must come back as a report or synthetic failure event

Practical rule:

- do not expand ACK semantics accidentally
- do not silently weaken them either
- document exactly which action completion unblocks the caller

### Request handling model

Suggested flow for a place order:

1. gRPC handler sends command to writer
2. writer mutates core state via `process_message`
3. writer updates the snapshot locally
4. writer enqueues side-effect envelopes
5. writer replies to caller once the request is accepted for asynchronous processing
6. gateway service later sends report or synthetic failure back into OMS
7. report/failure re-enters the writer via command channel

Suggested flow for a report:

1. NATS report enters writer
2. writer mutates core state and reservations/positions/balances
3. writer updates snapshot locally
4. writer enqueues persist and publish actions
5. writer returns immediately to the next command

### Failure handling

Async executors need explicit failure policy.

Gateway action failure:

- if validation fails, return failure to the caller immediately
- if internal dispatch/enqueue fails after ACK, emit an explicit synthetic failure event back into the writer
- if downstream send fails after enqueue, emit an explicit synthetic failure event back into the writer
- record structured error with `order_id`, `gw_id`, `global_seq`
- do not let worker-local failure silently vanish

Persistence failure:

- do not block the writer
- increment error metrics
- log with entity key and sequence
- optionally enqueue bounded retry if desired later

Publish failure:

- same rule as persistence
- failure must be observable, but should not mutate OMS state retroactively

Rule:

- the writer remains authoritative
- worker failures do not roll back writer state
- any compensating behavior must be explicit and modeled separately

### Backpressure and bounds

Every async executor queue must be bounded.

Why:

- otherwise latency improvement turns into unbounded memory growth under downstream slowdown

Recommended controls:

- bounded MPSC per shard
- per-pool queue depth metrics
- reject or degrade policy if enqueue fails

Suggested behavior if a queue is full:

- for ACK-critical actions:
  - fail the request explicitly
- for non-critical side effects:
  - log and count
  - optionally shed or retry depending on durability requirements

### Metrics

Parallel execution only helps if it is measurable.

Add at least:

- writer time:
  - command dequeue to `process_message` complete
- enqueue time:
  - action emitted to worker accepted
- executor queue wait:
  - enqueue to worker start
- side-effect execution time:
  - worker start to worker complete
- per-pool queue depth
- per-shard hottest queue depth
- failure counts by action type

This separates:

- OMS core compute cost
- writer queueing cost
- downstream I/O cost

which the current design mixes together.

### Sharding evolution

This design is compatible with later account sharding of the core.

Recommended evolution order:

1. keep one writer core
2. parallelize post-core action execution
3. validate metrics and behavior
4. only then consider account-sharded core mutation if needed

Reason:

- most of the avoidable latency today comes from inline side-effect wait
- removing that wait is lower risk than splitting core mutation ownership

### Non-goals of this step

This step should not:

- make snapshot updates multi-writer
- make reservation/position logic concurrent
- require account-plus-symbol sharding of core mutation
- require a redesign of inbound report ordering

### Recommended first implementation

The first implementation should be intentionally conservative:

- one single-writer OMS core
- one bounded gateway executor pool sharded by `(gw_id, order_id_hash)`
- one bounded persistence executor pool sharded by `account_id`
- one bounded publish executor pool sharded by `order_id` for order updates and `account_id` for account-level updates
- snapshot remains updated synchronously in the writer
- OMS gRPC reply means `accepted for asynchronous processing`
- gateway gRPC reply means `accepted for asynchronous processing`

This should provide most of the service-layer latency win without changing core correctness ownership.

## Rewrite Plan

This should be implemented as a target-state rewrite of the OMS internal model.

Recommended execution order:

1. define the new metadata tables and intern table
2. define writer-owned `OmsState`
3. define compact reader-visible `OmsSnapshot`
4. rewrite place-order path against the new data model
5. rewrite report path against the new data model
6. rewrite action boundary so `OmsAction` carries compact payloads
7. rebuild gRPC query handlers on top of compact snapshot reconstruction
8. reintroduce persistence, NATS publish, and benchmark wiring on the new structures

Recommended rule:

- do not keep the old internal structs alive in parallel longer than necessary
- once the new state model exists, move call sites to it directly
- keep external behavior tests and benchmark baselines as the main safety rail

## Compatibility Strategy

To keep the migration safe:

- preserve public proto contracts
- preserve Redis/NATS message schemas in early phases
- introduce internal IDs only inside `zk-oms-rs` and `zk-oms-svc`
- keep debug helpers that map IDs back to names

Recommended helper utilities:

- `instrument_name(id) -> &str`
- `gw_name(id) -> &str`
- `asset_name(id) -> &str`
- `debug_order(order_id) -> formatted rich dump`

This keeps operator ergonomics acceptable while optimizing the internal representation.

## Risks

### 1. Audit/read-model complexity increases

Splitting hot and cold state makes the code less direct than the current “one rich order struct” design.

Mitigation:

- clear module boundaries
- helper builders for boundary reconstruction
- thorough parity tests

### 2. Config reload becomes more subtle

If internal IDs are derived from config load, reload semantics must be explicit.

Mitigation:

- use generation-scoped immutable tables
- avoid mutating IDs in-place
- rebuild compact lookup tables atomically on reload

### 3. Debugging becomes less string-native

IDs are cheaper but less readable.

Mitigation:

- good lookup helpers
- good tracing fields
- explicit debug dump tools

### 4. Incremental rollout can create mixed representations

During migration, some paths may use IDs and others may still use strings/full protos.

Mitigation:

- phase boundaries should be explicit
- prefer adapters at module boundaries
- avoid half-refactoring one hot path without finishing its adjacent boundary code

## Testing Strategy

Each phase should keep or improve parity coverage.

Required test categories:

- existing `oms_parity` behavior must remain green
- add focused tests for:
  - config ID resolution
  - boundary reconstruction correctness
  - report handling with ID-based lookups
  - config reload with immutable metadata tables
  - out-of-order report handling
  - panic/reject semantics
  - GW send failure semantics

Performance verification should include:

- `cargo bench -p zk-oms-rs --bench oms_latency`
- `cargo bench -p zk-oms-svc --bench oms_svc_latency`
- service-level QPS sweep from `zk-trade-tools`

Measure:

- p50 / p90 / p99 `oms_through`
- p50 / p99 ack latency
- allocations per order if profiling tools are available
- peak RSS under sustained load

## Recommended Rewrite Scope

The rewrite should include, in one coherent design:

- metadata interning and integer IDs
- compact writer-owned OMS state
- compact reader-visible snapshot
- hot/cold order split
- compact `OmsAction`
- on-demand protobuf reconstruction at boundaries

Optional in the same rewrite, but not mandatory for correctness:

- per-GW bounded egress tasks

This is the point where the OMS core should become a Rust-native low-latency state machine rather than a direct Python-shaped port with incremental optimizations.

## Read Replica Rule

Decision:

- internal maintained data in the read replica should use compact internal structures
- convert internal structures to protobuf only when query handlers or publishers need protobuf output
- exchange-owned cache data can keep protobuf-shaped storage as-is

Rationale:

- internally-maintained OMS state is exactly where compact representation saves cost
- exchange cache data is already boundary-shaped and externally sourced, so storing protobuf as-is is acceptable
- this avoids unnecessary conversion churn on inbound exchange cache updates while still optimizing OMS-owned state

Concrete rule:

- OMS-owned order / position / reservation / derived state:
  - compact internal structures in memory
- exchange-owned balance / position snapshots:
  - store protobuf or protobuf-adjacent cache shape as-is
- outbound API and NATS paths:
  - reconstruct/copy protobuf on demand

## Resolved Decisions

The following design choices are now fixed for this plan:

- IDs should be plain integers in the first implementation
- instrument code and exchange symbol strings should be interned
- avoid `Arc` as the default hot-path string representation
- cold order detail should remain structurally intact but refer to heavyweight payloads by ID/reference
- read-replica snapshots should keep OMS-owned data in internal compact form and exchange-owned cache in protobuf form
- `OmsAction` slimming should happen immediately as part of the refactor, not as a deferred cleanup

## Final Recommendation

The OMS should be refactored toward an ID-based, compact-core design.

The strongest expected wins are:

- remove cloned full config/refdata from the place path
- remove duplicated full-order ownership on the hot path
- separate hot live order state from cold audit/history payloads

Pre-allocation should be included, but as supporting work.

The core architectural change is:

- stop treating Rust OMS state like Python object graphs
- treat the core path as a compact state machine over IDs and numeric fields
- reconstruct rich payloads only at publish, persistence, and API boundaries
