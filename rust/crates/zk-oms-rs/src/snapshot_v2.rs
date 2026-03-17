use std::collections::HashMap;
use std::sync::Arc;

use im::{HashMap as ImHashMap, HashSet as ImHashSet};

use zk_proto_rs::zk::{
    common::v1::LongShortType,
    oms::v1::{ExecMessage, Fee, Order, Position, Trade},
};

use crate::models::{ExchBalanceSnapshot, ExchPositionSnapshot, ReconcileStatus};

// ---------------------------------------------------------------------------
// SnapshotMetadata — Arc-shared name tables for reader-side string resolution
// ---------------------------------------------------------------------------

/// Immutable name tables copied from `InternTable` + metadata vectors.
/// Shared across snapshots via `Arc` — rebuilt only on config reload.
#[derive(Clone, Debug, Default)]
pub struct SnapshotMetadata {
    /// instrument_names[instrument_id] = instrument_code string
    pub instrument_names: Vec<Box<str>>,
    /// instrument_exch_names[instrument_id] = exchange symbol string
    pub instrument_exch_names: Vec<Box<str>>,
    /// asset_names[asset_id] = asset name string
    pub asset_names: Vec<Box<str>>,
    /// gw_names[gw_id] = gateway key string
    pub gw_names: Vec<Box<str>>,
    /// source_names[source_sym] = source identifier string.
    /// Indexed by InternTable string ID (covers all interned strings).
    pub source_names: Vec<Box<str>>,
}

impl SnapshotMetadata {
    pub fn instrument_name(&self, instrument_id: u32) -> &str {
        self.instrument_names
            .get(instrument_id as usize)
            .map(|s| &**s)
            .unwrap_or("")
    }

    pub fn instrument_exch_name(&self, instrument_id: u32) -> &str {
        self.instrument_exch_names
            .get(instrument_id as usize)
            .map(|s| &**s)
            .unwrap_or("")
    }

    pub fn asset_name(&self, asset_id: u32) -> &str {
        self.asset_names
            .get(asset_id as usize)
            .map(|s| &**s)
            .unwrap_or("")
    }

    pub fn gw_name(&self, gw_id: u32) -> &str {
        self.gw_names
            .get(gw_id as usize)
            .map(|s| &**s)
            .unwrap_or("")
    }

    pub fn source_name(&self, source_sym: u32) -> &str {
        self.source_names
            .get(source_sym as usize)
            .map(|s| &**s)
            .unwrap_or("")
    }
}

// ---------------------------------------------------------------------------
// SnapshotOrder — compact hot-path order (integer IDs, no strings)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct SnapshotOrder {
    pub order_id: i64,
    pub account_id: i64,
    pub instrument_id: u32,
    pub gw_id: u32,
    pub source_sym: u32,
    pub order_status: i32,
    pub buy_sell_type: i32,
    pub open_close_type: i32,
    pub order_type: i32,
    pub tif_type: i32,
    pub price: f64,
    pub qty: f64,
    pub filled_qty: f64,
    pub filled_avg_price: f64,
    pub exch_order_ref: Option<Box<str>>,
    pub created_at: i64,
    pub updated_at: i64,
    pub snapshot_version: u32,
    pub is_external: bool,
}

impl SnapshotOrder {
    /// Build a proto `Order` for gRPC response, resolving IDs via metadata.
    pub fn to_proto_order(
        &self,
        meta: &SnapshotMetadata,
        detail: Option<&SnapshotOrderDetail>,
    ) -> Order {
        Order {
            order_id: self.order_id,
            account_id: self.account_id,
            instrument: meta.instrument_name(self.instrument_id).to_string(),
            instrument_exch: meta.instrument_exch_name(self.instrument_id).to_string(),
            gw_key: meta.gw_name(self.gw_id).to_string(),
            order_status: self.order_status,
            buy_sell_type: self.buy_sell_type,
            open_close_type: self.open_close_type,
            price: self.price,
            qty: self.qty,
            filled_qty: self.filled_qty,
            filled_avg_price: self.filled_avg_price,
            source_id: meta.source_name(self.source_sym).to_string(),
            exch_order_ref: self
                .exch_order_ref
                .as_ref()
                .map(|s| s.to_string())
                .unwrap_or_default(),
            snapshot_version: self.snapshot_version as i32,
            created_at: self.created_at,
            updated_at: self.updated_at,
            error_msg: detail.map(|d| d.error_msg.to_string()).unwrap_or_default(),
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// SnapshotOrderDetail — cold-path trade/fee/history data
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct SnapshotOrderDetail {
    pub order_id: i64,
    pub source_sym: u32,
    pub instrument_exch_sym: u32,
    pub exch_order_ref: Option<Box<str>>,
    pub error_msg: Box<str>,
    pub trades: Vec<Trade>,
    pub inferred_trades: Vec<Trade>,
    pub exec_msgs: Vec<ExecMessage>,
    pub fees: Vec<Fee>,
}

// ---------------------------------------------------------------------------
// SnapshotManagedPosition — compact position (integer IDs)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct SnapshotManagedPosition {
    pub account_id: i64,
    pub instrument_id: u32,
    pub instrument_type: i32,
    pub is_short: bool,
    pub qty_total: f64,
    pub qty_frozen: f64,
    pub qty_available: f64,
    pub last_local_update_ts: i64,
    pub last_exch_sync_ts: i64,
    pub reconcile_status: ReconcileStatus,
}

impl SnapshotManagedPosition {
    pub fn to_proto(&self, meta: &SnapshotMetadata) -> Position {
        Position {
            account_id: self.account_id,
            instrument_code: meta.instrument_name(self.instrument_id).to_string(),
            instrument_type: self.instrument_type,
            long_short_type: if self.is_short {
                LongShortType::LsShort as i32
            } else {
                LongShortType::LsLong as i32
            },
            total_qty: self.qty_total,
            frozen_qty: self.qty_frozen,
            avail_qty: self.qty_available,
            update_timestamp: self.last_local_update_ts,
            sync_timestamp: self.last_exch_sync_ts,
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// OmsSnapshotV2
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct OmsSnapshotV2 {
    pub metadata: Arc<SnapshotMetadata>,

    pub orders: ImHashMap<i64, SnapshotOrder>,
    pub order_details: ImHashMap<i64, SnapshotOrderDetail>,
    pub open_order_ids_by_account: ImHashMap<i64, ImHashSet<i64>>,
    pub panic_accounts: ImHashSet<i64>,

    pub managed_positions: HashMap<(i64, u32), SnapshotManagedPosition>,
    pub exch_positions: HashMap<(i64, u32), ExchPositionSnapshot>,
    pub exch_balances: HashMap<(i64, u32), ExchBalanceSnapshot>,

    /// Exchange positions that couldn't be resolved to an integer instrument_id.
    pub unknown_exch_positions: Vec<ExchPositionSnapshot>,
    /// Exchange balances that couldn't be resolved to an integer asset_id.
    pub unknown_exch_balances: Vec<ExchBalanceSnapshot>,

    // Query-oriented indexes
    pub managed_position_ids_by_account: HashMap<i64, Vec<u32>>,
    pub exch_position_ids_by_account: HashMap<i64, Vec<u32>>,
    pub balance_asset_ids_by_account: HashMap<i64, Vec<u32>>,
    pub order_ids_by_exch_ref: HashMap<Box<str>, Vec<i64>>,

    pub seq: u64,
    pub snapshot_ts_ms: i64,
}

// ---------------------------------------------------------------------------
// OmsSnapshotWriterV2
// ---------------------------------------------------------------------------

pub struct OmsSnapshotWriterV2 {
    orders: ImHashMap<i64, SnapshotOrder>,
    order_details: ImHashMap<i64, SnapshotOrderDetail>,
    open_by_account: ImHashMap<i64, ImHashSet<i64>>,
    panic_accounts: ImHashSet<i64>,
    metadata: Arc<SnapshotMetadata>,
    seq: u64,
}

impl OmsSnapshotWriterV2 {
    pub fn new(metadata: Arc<SnapshotMetadata>) -> Self {
        Self {
            orders: ImHashMap::new(),
            order_details: ImHashMap::new(),
            open_by_account: ImHashMap::new(),
            panic_accounts: ImHashSet::new(),
            metadata,
            seq: 0,
        }
    }

    pub fn update_metadata(&mut self, metadata: Arc<SnapshotMetadata>) {
        self.metadata = metadata;
    }

    pub fn apply_order_update(&mut self, snapshot_order: SnapshotOrder, set_closed: bool) {
        let order_id = snapshot_order.order_id;
        let account_id = snapshot_order.account_id;
        self.orders.insert(order_id, snapshot_order);

        let open_set = self.open_by_account.entry(account_id).or_default();
        if set_closed {
            open_set.remove(&order_id);
        } else {
            open_set.insert(order_id);
        }
    }

    pub fn apply_order_detail(&mut self, detail: SnapshotOrderDetail) {
        self.order_details.insert(detail.order_id, detail);
    }

    pub fn apply_panic(&mut self, account_id: i64) {
        self.panic_accounts.insert(account_id);
    }

    pub fn apply_clear_panic(&mut self, account_id: i64) {
        self.panic_accounts.remove(&account_id);
    }

    pub fn publish(
        &mut self,
        managed_positions: HashMap<(i64, u32), SnapshotManagedPosition>,
        exch_positions: HashMap<(i64, u32), ExchPositionSnapshot>,
        exch_balances: HashMap<(i64, u32), ExchBalanceSnapshot>,
        unknown_exch_positions: Vec<ExchPositionSnapshot>,
        unknown_exch_balances: Vec<ExchBalanceSnapshot>,
        ts_ms: i64,
    ) -> OmsSnapshotV2 {
        self.seq += 1;

        let managed_position_ids_by_account = build_account_index(&managed_positions);
        let exch_position_ids_by_account = build_account_index(&exch_positions);
        let balance_asset_ids_by_account = build_account_index(&exch_balances);
        let order_ids_by_exch_ref = build_exch_ref_index(&self.orders);

        OmsSnapshotV2 {
            metadata: self.metadata.clone(),
            orders: self.orders.clone(),
            order_details: self.order_details.clone(),
            open_order_ids_by_account: self.open_by_account.clone(),
            panic_accounts: self.panic_accounts.clone(),
            managed_positions,
            exch_positions,
            exch_balances,
            unknown_exch_positions,
            unknown_exch_balances,
            managed_position_ids_by_account,
            exch_position_ids_by_account,
            balance_asset_ids_by_account,
            order_ids_by_exch_ref,
            seq: self.seq,
            snapshot_ts_ms: ts_ms,
        }
    }
}

// ---------------------------------------------------------------------------
// Index builders
// ---------------------------------------------------------------------------

fn build_account_index<V>(map: &HashMap<(i64, u32), V>) -> HashMap<i64, Vec<u32>> {
    let mut index: HashMap<i64, Vec<u32>> = HashMap::new();
    for &(account_id, sub_id) in map.keys() {
        index.entry(account_id).or_default().push(sub_id);
    }
    index
}

fn build_exch_ref_index(orders: &ImHashMap<i64, SnapshotOrder>) -> HashMap<Box<str>, Vec<i64>> {
    let mut index: HashMap<Box<str>, Vec<i64>> = HashMap::new();
    for (order_id, order) in orders.iter() {
        if let Some(ref exch_ref) = order.exch_order_ref {
            index.entry(exch_ref.clone()).or_default().push(*order_id);
        }
    }
    index
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_snapshot_order(order_id: i64, account_id: i64) -> SnapshotOrder {
        SnapshotOrder {
            order_id,
            account_id,
            instrument_id: 1,
            gw_id: 0,
            source_sym: 0,
            order_status: 1,
            buy_sell_type: 1,
            open_close_type: 0,
            order_type: 1,
            tif_type: 0,
            price: 100.0,
            qty: 10.0,
            filled_qty: 0.0,
            filled_avg_price: 0.0,
            exch_order_ref: Some("EX123".into()),
            created_at: 1000,
            updated_at: 1000,
            snapshot_version: 1,
            is_external: false,
        }
    }

    fn make_test_metadata() -> Arc<SnapshotMetadata> {
        Arc::new(SnapshotMetadata {
            instrument_names: vec!["".into(), "BTC-USDT".into()],
            instrument_exch_names: vec!["".into(), "BTCUSDT".into()],
            asset_names: vec!["BTC".into(), "USDT".into()],
            gw_names: vec!["gw1".into()],
            source_names: vec!["".into(), "strategy-1".into()],
        })
    }

    #[test]
    fn writer_publish_increments_seq() {
        let meta = make_test_metadata();
        let mut writer = OmsSnapshotWriterV2::new(meta);
        let snap1 = writer.publish(HashMap::new(), HashMap::new(), HashMap::new(), vec![], vec![], 1000);
        assert_eq!(snap1.seq, 1);
        let snap2 = writer.publish(HashMap::new(), HashMap::new(), HashMap::new(), vec![], vec![], 2000);
        assert_eq!(snap2.seq, 2);
    }

    #[test]
    fn apply_order_update_open_and_close() {
        let meta = make_test_metadata();
        let mut writer = OmsSnapshotWriterV2::new(meta);
        let snap_order = make_test_snapshot_order(1001, 42);

        writer.apply_order_update(snap_order.clone(), false);
        assert!(writer.open_by_account[&42].contains(&1001));
        assert_eq!(writer.orders[&1001].instrument_id, 1);
        assert_eq!(writer.orders[&1001].exch_order_ref.as_deref(), Some("EX123"));

        let mut closed = snap_order;
        closed.order_status = 4;
        writer.apply_order_update(closed, true);
        assert!(!writer.open_by_account[&42].contains(&1001));
    }

    #[test]
    fn snapshot_clone_is_independent() {
        let meta = make_test_metadata();
        let mut writer = OmsSnapshotWriterV2::new(meta);
        let snap_order = make_test_snapshot_order(1001, 42);
        writer.apply_order_update(snap_order, false);

        let snap1 = writer.publish(HashMap::new(), HashMap::new(), HashMap::new(), vec![], vec![], 1000);
        writer.apply_panic(42);
        let snap2 = writer.publish(HashMap::new(), HashMap::new(), HashMap::new(), vec![], vec![], 2000);

        assert!(!snap1.panic_accounts.contains(&42));
        assert!(snap2.panic_accounts.contains(&42));
    }

    #[test]
    fn to_proto_order_resolves_strings() {
        let meta = make_test_metadata();
        let snap_order = make_test_snapshot_order(1001, 42);
        let proto = snap_order.to_proto_order(&meta, None);

        assert_eq!(proto.instrument, "BTC-USDT");
        assert_eq!(proto.instrument_exch, "BTCUSDT");
        assert_eq!(proto.gw_key, "gw1");
        assert_eq!(proto.order_id, 1001);
    }

    #[test]
    fn to_proto_position_resolves_strings() {
        let meta = make_test_metadata();
        let pos = SnapshotManagedPosition {
            account_id: 42,
            instrument_id: 1,
            instrument_type: 1,
            is_short: false,
            qty_total: 10.0,
            qty_frozen: 2.0,
            qty_available: 8.0,
            last_local_update_ts: 1000,
            last_exch_sync_ts: 900,
            reconcile_status: ReconcileStatus::InSync,
        };
        let proto = pos.to_proto(&meta);
        assert_eq!(proto.instrument_code, "BTC-USDT");
        assert_eq!(proto.long_short_type, LongShortType::LsLong as i32);
        assert!((proto.total_qty - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn query_indexes_populated() {
        let meta = make_test_metadata();
        let mut writer = OmsSnapshotWriterV2::new(meta);

        let snap_order = make_test_snapshot_order(1001, 42);
        writer.apply_order_update(snap_order, false);

        let mut managed = HashMap::new();
        managed.insert(
            (42_i64, 1_u32),
            SnapshotManagedPosition {
                account_id: 42,
                instrument_id: 1,
                instrument_type: 1,
                is_short: false,
                qty_total: 10.0,
                qty_frozen: 0.0,
                qty_available: 10.0,
                last_local_update_ts: 0,
                last_exch_sync_ts: 0,
                reconcile_status: ReconcileStatus::Unknown,
            },
        );

        let snap = writer.publish(managed, HashMap::new(), HashMap::new(), vec![], vec![], 1000);
        assert_eq!(snap.managed_position_ids_by_account[&42], vec![1]);
        let ex_ref: Box<str> = "EX123".into();
        assert_eq!(snap.order_ids_by_exch_ref[&ex_ref], vec![1001]);
    }

    #[test]
    fn order_detail_separate_from_order() {
        let meta = make_test_metadata();
        let mut writer = OmsSnapshotWriterV2::new(meta);

        let snap_order = make_test_snapshot_order(1001, 42);
        writer.apply_order_update(snap_order, false);

        let detail = SnapshotOrderDetail {
            order_id: 1001,
            source_sym: 0,
            instrument_exch_sym: 1,
            exch_order_ref: Some("EX123".into()),
            error_msg: "".into(),
            trades: vec![],
            inferred_trades: vec![],
            exec_msgs: vec![],
            fees: vec![],
        };
        writer.apply_order_detail(detail);

        let snap = writer.publish(HashMap::new(), HashMap::new(), HashMap::new(), vec![], vec![], 1000);
        assert!(snap.orders.contains_key(&1001));
        assert!(snap.order_details.contains_key(&1001));
    }

    #[test]
    fn update_metadata_reflected_in_next_snapshot() {
        let meta1 = Arc::new(SnapshotMetadata {
            instrument_names: vec!["OLD".into()],
            ..Default::default()
        });
        let mut writer = OmsSnapshotWriterV2::new(meta1);

        let snap1 = writer.publish(HashMap::new(), HashMap::new(), HashMap::new(), vec![], vec![], 1000);
        assert_eq!(snap1.metadata.instrument_name(0), "OLD");

        let meta2 = Arc::new(SnapshotMetadata {
            instrument_names: vec!["NEW".into()],
            ..Default::default()
        });
        writer.update_metadata(meta2);

        let snap2 = writer.publish(HashMap::new(), HashMap::new(), HashMap::new(), vec![], vec![], 2000);
        assert_eq!(snap2.metadata.instrument_name(0), "NEW");
        assert_eq!(snap1.metadata.instrument_name(0), "OLD");
    }
}
