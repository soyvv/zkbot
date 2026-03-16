use std::collections::HashMap;

use tracing::error;
use zk_proto_rs::{
    zk::{
        common::v1::{InstrumentRefData, LongShortType},
        exch_gw::v1::PositionReport,
        oms::v1::{Position, PositionUpdateEvent},
    },
    ods::OmsRouteEntry,
};

use crate::models::{ExchPositionSnapshot, OmsManagedPosition, PositionDelta, ReconcileStatus};

/// Consecutive divergence count before promoting to DivergedPersistent.
const RECONCILE_PERSISTENT_THRESHOLD: u32 = 3;

/// Manages OMS-owned executable position state and exchange-reported position cache.
///
/// Position qty is OMS-owned and reconciled against exchange.
/// Exchange owns margin, PnL, and other economics fields.
pub struct PositionManager {
    /// account_id → instrument_code → OMS-managed position
    pub(crate) managed: HashMap<i64, HashMap<String, OmsManagedPosition>>,
    /// account_id → instrument_code → exchange-reported position snapshot
    pub(crate) exch: HashMap<i64, HashMap<String, ExchPositionSnapshot>>,
    /// account_id → (instrument_id_exchange → InstrumentRefData)
    symbol_map: HashMap<i64, HashMap<String, InstrumentRefData>>,
}

impl PositionManager {
    pub fn new(
        symbol_map: HashMap<i64, HashMap<String, InstrumentRefData>>,
        account_routes: &[OmsRouteEntry],
    ) -> Self {
        let mut managed = HashMap::new();
        let mut exch = HashMap::new();
        for r in account_routes {
            managed.entry(r.account_id).or_insert_with(HashMap::new);
            exch.entry(r.account_id).or_insert_with(HashMap::new);
        }
        Self {
            managed,
            exch,
            symbol_map,
        }
    }

    // ------------------------------------------------------------------
    // Config reload
    // ------------------------------------------------------------------

    pub fn reload_symbol_map(
        &mut self,
        symbol_map: HashMap<i64, HashMap<String, InstrumentRefData>>,
    ) {
        self.symbol_map = symbol_map;
    }

    pub fn reload_account_config(&mut self, routes: &[OmsRouteEntry]) {
        for r in routes {
            self.managed.entry(r.account_id).or_default();
            self.exch.entry(r.account_id).or_default();
        }
    }

    // ------------------------------------------------------------------
    // Initialisation
    // ------------------------------------------------------------------

    pub fn init_positions(&mut self, positions: Vec<OmsManagedPosition>) {
        for pos in positions {
            self.managed
                .entry(pos.account_id)
                .or_default()
                .insert(pos.instrument_code.clone(), pos);
        }
    }

    // ------------------------------------------------------------------
    // Read
    // ------------------------------------------------------------------

    pub fn get_position(
        &self,
        account_id: i64,
        instrument_code: &str,
    ) -> Option<&OmsManagedPosition> {
        self.managed
            .get(&account_id)?
            .get(instrument_code)
    }

    pub fn get_position_mut(
        &mut self,
        account_id: i64,
        instrument_code: &str,
        create_if_missing: bool,
    ) -> Option<&mut OmsManagedPosition> {
        if create_if_missing
            && !self
                .managed
                .entry(account_id)
                .or_default()
                .contains_key(instrument_code)
        {
            let pos = OmsManagedPosition::new(account_id, instrument_code, 0, false);
            self.managed
                .entry(account_id)
                .or_default()
                .insert(instrument_code.to_string(), pos);
        }
        self.managed.get_mut(&account_id)?.get_mut(instrument_code)
    }

    pub fn get_positions_for_account(&self, account_id: i64) -> Vec<&OmsManagedPosition> {
        self.managed
            .get(&account_id)
            .map(|m| m.values().collect())
            .unwrap_or_default()
    }

    // ------------------------------------------------------------------
    // Pre-trade: freeze position for sell orders
    // ------------------------------------------------------------------

    /// Check available position qty and freeze it for a sell order.
    pub fn check_and_freeze(
        &mut self,
        account_id: i64,
        instrument_code: &str,
        qty: f64,
        is_short: bool,
    ) -> Result<(), String> {
        let pos = self
            .managed
            .get(&account_id)
            .and_then(|m| m.get(instrument_code));
        let avail = pos.map(|p| p.qty_available).unwrap_or(0.0);
        if avail < qty {
            return Err(format!(
                "Not enough available position for {}: need {}, have {}",
                instrument_code, qty, avail
            ));
        }
        // Freeze: move from available to frozen
        let pos = self
            .managed
            .entry(account_id)
            .or_default()
            .entry(instrument_code.to_string())
            .or_insert_with(|| {
                OmsManagedPosition::new(account_id, instrument_code, 0, is_short)
            });
        pos.qty_available -= qty;
        pos.qty_frozen += qty;
        Ok(())
    }

    /// Unfreeze position qty (e.g. on cancel).
    pub fn unfreeze(&mut self, account_id: i64, instrument_code: &str, qty: f64) {
        if let Some(pos) = self
            .managed
            .get_mut(&account_id)
            .and_then(|m| m.get_mut(instrument_code))
        {
            let release = qty.min(pos.qty_frozen);
            pos.qty_frozen -= release;
            pos.qty_available += release;
        }
    }

    // ------------------------------------------------------------------
    // Fill: apply position delta
    // ------------------------------------------------------------------

    /// Apply a position delta from a fill. Returns list of changed instrument codes.
    /// Handles sign flip (total goes negative → flip long/short).
    pub fn apply_delta(&mut self, delta: &PositionDelta, ts: i64) -> Vec<String> {
        let mut changed = Vec::new();
        changed.push(delta.instrument_code.clone());

        let pos = self
            .managed
            .entry(delta.account_id)
            .or_default()
            .entry(delta.instrument_code.clone())
            .or_insert_with(|| {
                OmsManagedPosition::new(
                    delta.account_id,
                    &delta.instrument_code,
                    0,
                    delta.is_short,
                )
            });

        pos.qty_total += delta.total_change;
        pos.qty_frozen = (pos.qty_frozen + delta.frozen_change).max(0.0);

        // Clamp available to total
        let new_avail = pos.qty_available + delta.avail_change;
        pos.qty_available = if new_avail > pos.qty_total {
            pos.qty_total
        } else {
            new_avail
        };
        pos.last_local_update_ts = ts;

        // Handle sign flip (total goes negative → flip long/short)
        if pos.qty_total < 0.0 {
            pos.is_short = !pos.is_short;
            pos.qty_total = pos.qty_total.abs();
            pos.qty_available = pos.qty_available.abs();
        }

        changed
    }

    // ------------------------------------------------------------------
    // Exchange sync: merge gateway position entries
    // ------------------------------------------------------------------

    /// Merge gateway-reported position entries (non-spot instrument types).
    /// Returns the account_id that was updated.
    pub fn merge_gw_position_entries(
        &mut self,
        entries: &[PositionReport],
        account_id: i64,
        ts: i64,
    ) {
        let no_managed = self
            .managed
            .get(&account_id)
            .map(|m| m.is_empty())
            .unwrap_or(true);

        for entry in entries {
            // Translate exchange symbol to OMS instrument_id via refdata
            let symbol = match self
                .symbol_map
                .get(&account_id)
                .and_then(|m| m.get(&entry.instrument_code))
            {
                Some(ref_data) => ref_data.instrument_id.clone(),
                None => {
                    error!(
                        instrument_code = entry.instrument_code,
                        "refdata not found for position entry; skipping"
                    );
                    continue;
                }
            };

            let is_short = entry.long_short_type == LongShortType::LsShort as i32;

            // Update exchange position snapshot
            let exch_snap = self
                .exch
                .entry(account_id)
                .or_default()
                .entry(symbol.clone())
                .or_insert_with(|| ExchPositionSnapshot {
                    account_id,
                    instrument_code: symbol.clone(),
                    symbol_exch: Some(entry.instrument_code.clone()),
                    position_state: Position::default(),
                    exch_data_raw: String::new(),
                    sync_ts: 0,
                });
            exch_snap.symbol_exch = Some(entry.instrument_code.clone());
            exch_snap.position_state.account_id = account_id;
            exch_snap.position_state.instrument_code = symbol.clone();
            exch_snap.position_state.total_qty = entry.qty;
            exch_snap.position_state.avail_qty = entry.avail_qty;
            exch_snap.position_state.frozen_qty = entry.qty - entry.avail_qty;
            exch_snap.position_state.long_short_type = entry.long_short_type;
            exch_snap.position_state.instrument_type = entry.instrument_type;
            exch_snap.position_state.update_timestamp = entry.update_timestamp;
            exch_snap.position_state.sync_timestamp = ts;
            exch_snap.exch_data_raw = entry.message_raw.clone();
            exch_snap.sync_ts = ts;

            // Seed managed position on first sync if empty
            let managed_sync_ts = self
                .managed
                .get(&account_id)
                .and_then(|m| m.get(&symbol))
                .map(|p| p.last_exch_sync_ts);
            let should_seed = no_managed || managed_sync_ts.is_none_or(|t| t == 0);
            if should_seed {
                let pos = self
                    .managed
                    .entry(account_id)
                    .or_default()
                    .entry(symbol.clone())
                    .or_insert_with(|| {
                        OmsManagedPosition::new(account_id, &symbol, entry.instrument_type, is_short)
                    });
                pos.qty_total = entry.qty;
                pos.qty_available = entry.avail_qty;
                pos.qty_frozen = entry.qty - entry.avail_qty;
                pos.is_short = is_short;
                pos.symbol_exch = Some(entry.instrument_code.clone());
                pos.last_exch_sync_ts = ts;
            }

            // Run reconcile check
            self.update_reconcile_status(account_id, &symbol, ts);
        }
    }

    // ------------------------------------------------------------------
    // Reconciliation
    // ------------------------------------------------------------------

    fn update_reconcile_status(&mut self, account_id: i64, instrument_code: &str, ts: i64) {
        let exch_qty = self
            .exch
            .get(&account_id)
            .and_then(|m| m.get(instrument_code))
            .map(|s| s.position_state.total_qty);

        if let Some(pos) = self
            .managed
            .get_mut(&account_id)
            .and_then(|m| m.get_mut(instrument_code))
        {
            pos.last_exch_qty = exch_qty.unwrap_or(0.0);
            let diff = (pos.qty_total - pos.last_exch_qty).abs();
            if diff < 1e-9 {
                pos.reconcile_status = ReconcileStatus::InSync;
                pos.first_diverged_ts = 0;
                pos.divergence_count = 0;
            } else if pos.reconcile_status == ReconcileStatus::InSync
                || pos.reconcile_status == ReconcileStatus::Unknown
            {
                pos.reconcile_status = ReconcileStatus::DivergedTransient;
                pos.first_diverged_ts = ts;
                pos.divergence_count = 1;
            } else {
                pos.divergence_count += 1;
                if pos.divergence_count >= RECONCILE_PERSISTENT_THRESHOLD {
                    pos.reconcile_status = ReconcileStatus::DivergedPersistent;
                }
            }
        }
    }

    /// Check reconcile status for a position.
    pub fn check_reconcile(
        &self,
        account_id: i64,
        instrument_code: &str,
    ) -> ReconcileStatus {
        self.managed
            .get(&account_id)
            .and_then(|m| m.get(instrument_code))
            .map(|p| p.reconcile_status)
            .unwrap_or(ReconcileStatus::Unknown)
    }

    // ------------------------------------------------------------------
    // Publish
    // ------------------------------------------------------------------

    /// Build a `PositionUpdateEvent` from managed positions for an account.
    pub fn build_position_snapshot(
        &self,
        account_id: i64,
        ts: i64,
    ) -> PositionUpdateEvent {
        let positions = self.get_positions_for_account(account_id);
        PositionUpdateEvent {
            account_id,
            position_snapshots: positions.iter().map(|p| p.to_proto()).collect(),
            timestamp: ts,
        }
    }

    // ------------------------------------------------------------------
    // Snapshot helpers (for read replica)
    // ------------------------------------------------------------------

    pub fn snapshot_managed(&self) -> HashMap<i64, HashMap<String, OmsManagedPosition>> {
        self.managed.clone()
    }

    pub fn snapshot_exch(&self) -> HashMap<i64, HashMap<String, ExchPositionSnapshot>> {
        self.exch.clone()
    }
}
