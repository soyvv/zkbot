use std::collections::HashMap;

use im::{HashMap as ImHashMap, HashSet as ImHashSet};

use crate::models::{ExchBalanceSnapshot, ExchPositionSnapshot, OmsManagedPosition, OmsOrder};

/// Immutable point-in-time snapshot of OmsCore state.
///
/// # Reader/Writer separation
///
/// `OmsCore` is single-writer: exactly one tokio task drives `process_message`.
/// After each mutation the writer calls `OmsSnapshotWriter::apply_*` to update the
/// incremental state, then `publish()` to atomically swap `ArcSwap<OmsSnapshot>`
/// in the service binary. gRPC query handlers call `replica.load()` — an atomic
/// pointer load that never blocks the writer and never contends with other readers.
///
/// # Clone cost (incremental — not O(n) full copy)
///
/// The order-heavy fields use `im::HashMap` (Hash Array Mapped Trie with structural
/// sharing). `im::HashMap::clone()` is O(1); `insert()` is O(log32 n). The writer
/// only touches changed entries per mutation; the snapshot clone for publishing
/// is O(1) for all `im` fields plus O(accounts × assets) for the small balance maps.
///
/// # Backtest
///
/// This module is compiled only when the `replica` Cargo feature is enabled.
/// Backtest builds omit the feature → zero overhead from `im` and snapshot machinery.
#[derive(Clone, Debug)]
pub struct OmsSnapshot {
    /// order_id → order  (im::HashMap — O(1) clone, O(log n) insert)
    pub orders: ImHashMap<i64, OmsOrder>,
    /// account_id → {open_order_id}  (incremental insert/remove per mutation)
    pub open_order_ids_by_account: ImHashMap<i64, ImHashSet<i64>>,
    /// Accounts currently in panic mode.
    pub panic_accounts: ImHashSet<i64>,
    /// OMS-managed position state — account_id → instrument_code → position.
    pub managed_positions: HashMap<i64, HashMap<String, OmsManagedPosition>>,
    /// Exchange-reported position snapshots.
    pub exch_positions: HashMap<i64, HashMap<String, ExchPositionSnapshot>>,
    /// Exchange-owned balance snapshots — account_id → asset → balance.
    pub exch_balances: HashMap<i64, HashMap<String, ExchBalanceSnapshot>>,
    /// Monotonic counter — incremented by `OmsSnapshotWriter::publish()`.
    pub seq: u64,
    /// Wall-clock time of this snapshot in milliseconds since epoch.
    pub snapshot_ts_ms: i64,
}

// ---------------------------------------------------------------------------
// OmsSnapshotWriter — owned by the single OmsCore writer task
// ---------------------------------------------------------------------------

/// Maintains the incremental live state for the read replica.
///
/// Owned exclusively by the OmsCore writer task. After each `process_message`
/// call the task calls the appropriate `apply_*` method(s) driven by the
/// returned `Vec<OmsAction>`, then calls `publish()` to produce the next
/// snapshot for `ArcSwap::store()`.
pub struct OmsSnapshotWriter {
    orders: ImHashMap<i64, OmsOrder>,
    open_by_account: ImHashMap<i64, ImHashSet<i64>>,
    panic_accounts: ImHashSet<i64>,
    seq: u64,
}

impl OmsSnapshotWriter {
    pub fn new() -> Self {
        Self {
            orders: ImHashMap::new(),
            open_by_account: ImHashMap::new(),
            panic_accounts: ImHashSet::new(),
            seq: 0,
        }
    }

    /// Apply a persisted order update. Corresponds to `OmsAction::PersistOrder`.
    pub fn apply_persist_order(&mut self, order: &OmsOrder, set_closed: bool) {
        self.orders.insert(order.order_id, order.clone());
        let open_set = self.open_by_account.entry(order.account_id).or_default();
        if set_closed {
            open_set.remove(&order.order_id);
        } else {
            open_set.insert(order.order_id);
        }
    }

    pub fn apply_panic(&mut self, account_id: i64) {
        self.panic_accounts.insert(account_id);
    }

    pub fn apply_clear_panic(&mut self, account_id: i64) {
        self.panic_accounts.remove(&account_id);
    }

    /// Produce the next snapshot. Increments the sequence number.
    pub fn publish(
        &mut self,
        managed_positions: HashMap<i64, HashMap<String, OmsManagedPosition>>,
        exch_positions: HashMap<i64, HashMap<String, ExchPositionSnapshot>>,
        exch_balances: HashMap<i64, HashMap<String, ExchBalanceSnapshot>>,
        ts_ms: i64,
    ) -> OmsSnapshot {
        self.seq += 1;
        OmsSnapshot {
            orders: self.orders.clone(),
            open_order_ids_by_account: self.open_by_account.clone(),
            panic_accounts: self.panic_accounts.clone(),
            managed_positions,
            exch_positions,
            exch_balances,
            seq: self.seq,
            snapshot_ts_ms: ts_ms,
        }
    }
}

impl Default for OmsSnapshotWriter {
    fn default() -> Self {
        Self::new()
    }
}
