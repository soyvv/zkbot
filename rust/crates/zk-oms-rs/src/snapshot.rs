use std::collections::HashMap;

use im::{HashMap as ImHashMap, HashSet as ImHashSet};

use crate::models::{OmsOrder, OmsPosition};

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
    /// OMS-maintained balance/position ledger — account_id → symbol → position.
    /// Full clone per mutation, but tiny (O(accounts × assets)).
    pub balances: HashMap<i64, HashMap<String, OmsPosition>>,
    /// Last exchange-reported balance/position — same structure as `balances`.
    pub exch_balances: HashMap<i64, HashMap<String, OmsPosition>>,
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
///
/// ## Usage (writer loop, in zk-oms-svc)
///
/// ```ignore
/// // Startup: build initial state from warm-loaded OmsCore
/// let (initial_snap, mut writer) = core.take_snapshot();
/// replica.store(Arc::new(initial_snap));
///
/// // Per mutation:
/// let actions = core.process_message(msg);
/// let mut balances_dirty = false;
/// for action in &actions {
///     match action {
///         OmsAction::PersistOrder { order, set_closed, .. } =>
///             writer.apply_persist_order(order, *set_closed),
///         OmsAction::PublishBalanceUpdate(_) => balances_dirty = true,
///         _ => {}
///     }
///     dispatch_action(action, ...).await;
/// }
/// // Also handle Panic/DontPanic on the message before process_message:
/// //   OmsMessage::Panic { account_id } => writer.apply_panic(account_id)
/// //   OmsMessage::DontPanic { account_id } => writer.apply_clear_panic(account_id)
///
/// let (bal, exch_bal) = if balances_dirty {
///     (core.balance_mgr.snapshot_balances(), core.balance_mgr.snapshot_exch_balances())
/// } else {
///     let prev = replica.load();
///     (prev.balances.clone(), prev.exch_balances.clone())
/// };
/// replica.store(Arc::new(writer.publish(bal, exch_bal, gen_timestamp_ms())));
/// ```
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
    ///
    /// O(log n) — only the changed entry is updated; all other nodes are shared
    /// with the previous snapshot via structural sharing.
    pub fn apply_persist_order(&mut self, order: &OmsOrder, set_closed: bool) {
        self.orders.insert(order.order_id, order.clone());
        let open_set = self.open_by_account.entry(order.account_id).or_default();
        if set_closed {
            open_set.remove(&order.order_id);
        } else {
            open_set.insert(order.order_id);
        }
    }

    /// Mark an account as panicked (order placement blocked).
    pub fn apply_panic(&mut self, account_id: i64) {
        self.panic_accounts.insert(account_id);
    }

    /// Clear panic mode for an account.
    pub fn apply_clear_panic(&mut self, account_id: i64) {
        self.panic_accounts.remove(&account_id);
    }

    /// Produce the next snapshot. Increments the sequence number.
    ///
    /// - `im` field clones: O(1) each (just Arc ref-count increments on HAMT roots)
    /// - `balances` / `exch_balances`: full clone, but tiny (pass previous snapshot's
    ///   values when balances have not changed this mutation to skip even that)
    pub fn publish(
        &mut self,
        balances: HashMap<i64, HashMap<String, OmsPosition>>,
        exch_balances: HashMap<i64, HashMap<String, OmsPosition>>,
        ts_ms: i64,
    ) -> OmsSnapshot {
        self.seq += 1;
        OmsSnapshot {
            orders: self.orders.clone(),
            open_order_ids_by_account: self.open_by_account.clone(),
            panic_accounts: self.panic_accounts.clone(),
            balances,
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

// ---------------------------------------------------------------------------
// Balance snapshot helpers — called by writer loop for balance-dirty mutations
// ---------------------------------------------------------------------------

impl crate::balance_mgr::BalanceManager {
    /// Clone the OMS-maintained balance ledger.
    /// Tiny (one entry per asset per account) — full clone is acceptable.
    pub fn snapshot_balances(&self) -> HashMap<i64, HashMap<String, OmsPosition>> {
        self.balances.clone()
    }

    /// Clone the last exchange-reported balance ledger.
    pub fn snapshot_exch_balances(&self) -> HashMap<i64, HashMap<String, OmsPosition>> {
        self.exch_balances.clone()
    }
}
