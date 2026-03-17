use std::collections::HashMap;

use crate::models::{ExchPositionSnapshot, ReconcileStatus};
use crate::models_v2::{ManagedPositionV2, PositionDeltaV2};

/// Consecutive divergence count before promoting to DivergedPersistent.
const RECONCILE_PERSISTENT_THRESHOLD: u32 = 3;

/// Manages OMS-owned position state using integer instrument IDs.
pub struct PositionStoreV2 {
    /// (account_id, instrument_id) → managed position
    pub(crate) managed: HashMap<(i64, u32), ManagedPositionV2>,
    /// (account_id, instrument_id) → exchange-reported snapshot
    pub(crate) exch: HashMap<(i64, u32), ExchPositionSnapshot>,
}

impl PositionStoreV2 {
    pub fn new() -> Self {
        Self {
            managed: HashMap::new(),
            exch: HashMap::new(),
        }
    }

    // ------------------------------------------------------------------
    // Initialisation
    // ------------------------------------------------------------------

    /// Insert managed positions, keyed by (account_id, instrument_id).
    pub fn init_positions(&mut self, positions: Vec<ManagedPositionV2>) {
        for pos in positions {
            self.managed
                .insert((pos.account_id, pos.instrument_id), pos);
        }
    }

    // ------------------------------------------------------------------
    // Read
    // ------------------------------------------------------------------

    pub fn get_position(
        &self,
        account_id: i64,
        instrument_id: u32,
    ) -> Option<&ManagedPositionV2> {
        self.managed.get(&(account_id, instrument_id))
    }

    pub fn get_position_mut(
        &mut self,
        account_id: i64,
        instrument_id: u32,
    ) -> Option<&mut ManagedPositionV2> {
        self.managed.get_mut(&(account_id, instrument_id))
    }

    /// Get or create a position entry, inserting a default if missing.
    pub fn get_or_create_position(
        &mut self,
        account_id: i64,
        instrument_id: u32,
        instrument_type: i32,
        is_short: bool,
    ) -> &mut ManagedPositionV2 {
        self.managed
            .entry((account_id, instrument_id))
            .or_insert_with(|| {
                ManagedPositionV2::new(account_id, instrument_id, instrument_type, is_short)
            })
    }

    /// Iterate all managed positions.
    pub fn all_positions(&self) -> impl Iterator<Item = &ManagedPositionV2> {
        self.managed.values()
    }

    /// Iterate all exchange position snapshots.
    pub fn all_exch_positions(&self) -> impl Iterator<Item = &ExchPositionSnapshot> {
        self.exch.values()
    }

    /// Collect all managed positions for a given account_id.
    pub fn get_positions_for_account(&self, account_id: i64) -> Vec<&ManagedPositionV2> {
        self.managed
            .iter()
            .filter(|((aid, _), _)| *aid == account_id)
            .map(|(_, pos)| pos)
            .collect()
    }

    // ------------------------------------------------------------------
    // Pre-trade: freeze position for sell orders
    // ------------------------------------------------------------------

    /// Check available position qty and freeze it for a sell order.
    pub fn check_and_freeze(
        &mut self,
        account_id: i64,
        instrument_id: u32,
        qty: f64,
        is_short: bool,
    ) -> Result<(), String> {
        let avail = self
            .managed
            .get(&(account_id, instrument_id))
            .map(|p| p.qty_available)
            .unwrap_or(0.0);
        if avail < qty {
            return Err(format!(
                "Not enough available position for instrument {}: need {}, have {}",
                instrument_id, qty, avail
            ));
        }
        // Freeze: move from available to frozen
        let pos = self
            .managed
            .entry((account_id, instrument_id))
            .or_insert_with(|| {
                ManagedPositionV2::new(account_id, instrument_id, 0, is_short)
            });
        pos.qty_available -= qty;
        pos.qty_frozen += qty;
        Ok(())
    }

    /// Unfreeze position qty (e.g. on cancel).
    pub fn unfreeze(&mut self, account_id: i64, instrument_id: u32, qty: f64) {
        if let Some(pos) = self.managed.get_mut(&(account_id, instrument_id)) {
            let release = qty.min(pos.qty_frozen);
            pos.qty_frozen -= release;
            pos.qty_available += release;
        }
    }

    // ------------------------------------------------------------------
    // Fill: apply position delta
    // ------------------------------------------------------------------

    /// Apply a position delta from a fill. Handles sign flip (total goes negative → flip
    /// long/short).
    pub fn apply_delta(&mut self, delta: &PositionDeltaV2, ts: i64) {
        let pos = self
            .managed
            .entry((delta.account_id, delta.instrument_id))
            .or_insert_with(|| {
                ManagedPositionV2::new(
                    delta.account_id,
                    delta.instrument_id,
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
    }

    // ------------------------------------------------------------------
    // Reconciliation
    // ------------------------------------------------------------------

    /// Compare managed vs exchange position and update reconcile status.
    pub fn update_reconcile_status(&mut self, account_id: i64, instrument_id: u32, ts: i64) {
        let exch_qty = self
            .exch
            .get(&(account_id, instrument_id))
            .map(|s| s.position_state.total_qty);

        if let Some(pos) = self.managed.get_mut(&(account_id, instrument_id)) {
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
    pub fn check_reconcile(&self, account_id: i64, instrument_id: u32) -> ReconcileStatus {
        self.managed
            .get(&(account_id, instrument_id))
            .map(|p| p.reconcile_status)
            .unwrap_or(ReconcileStatus::Unknown)
    }
}

impl Default for PositionStoreV2 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models_v2::PositionDeltaV2;
    use zk_proto_rs::zk::oms::v1::Position;

    fn make_position(account_id: i64, instrument_id: u32, qty: f64) -> ManagedPositionV2 {
        let mut pos = ManagedPositionV2::new(account_id, instrument_id, 0, false);
        pos.qty_total = qty;
        pos.qty_available = qty;
        pos
    }

    #[test]
    fn freeze_and_unfreeze() {
        let mut store = PositionStoreV2::new();
        store.init_positions(vec![make_position(1, 10, 100.0)]);

        store.check_and_freeze(1, 10, 30.0, false).unwrap();
        let pos = store.get_position(1, 10).unwrap();
        assert_eq!(pos.qty_available, 70.0);
        assert_eq!(pos.qty_frozen, 30.0);

        store.unfreeze(1, 10, 30.0);
        let pos = store.get_position(1, 10).unwrap();
        assert_eq!(pos.qty_available, 100.0);
        assert_eq!(pos.qty_frozen, 0.0);
    }

    #[test]
    fn freeze_insufficient_qty_fails() {
        let mut store = PositionStoreV2::new();
        store.init_positions(vec![make_position(1, 10, 20.0)]);

        let result = store.check_and_freeze(1, 10, 50.0, false);
        assert!(result.is_err());
    }

    #[test]
    fn apply_delta_sign_flip() {
        let mut store = PositionStoreV2::new();
        let mut pos = ManagedPositionV2::new(1, 10, 0, false);
        pos.qty_total = 5.0;
        pos.qty_available = 5.0;
        store.init_positions(vec![pos]);

        // Sell 8 → total becomes -3 → sign flip to short with qty 3
        let delta = PositionDeltaV2 {
            account_id: 1,
            instrument_id: 10,
            is_short: false,
            avail_change: -8.0,
            frozen_change: 0.0,
            total_change: -8.0,
        };
        store.apply_delta(&delta, 1000);

        let pos = store.get_position(1, 10).unwrap();
        assert!(pos.is_short);
        assert!((pos.qty_total - 3.0).abs() < 1e-9);
        assert!((pos.qty_available - 3.0).abs() < 1e-9);
    }

    #[test]
    fn reconcile_status_transitions() {
        let mut store = PositionStoreV2::new();
        let mut pos = ManagedPositionV2::new(1, 10, 0, false);
        pos.qty_total = 100.0;
        store.init_positions(vec![pos]);

        // No exch data yet → diverge
        store.update_reconcile_status(1, 10, 1000);
        assert_eq!(store.check_reconcile(1, 10), ReconcileStatus::DivergedTransient);

        // Second divergence
        store.update_reconcile_status(1, 10, 2000);
        assert_eq!(store.check_reconcile(1, 10), ReconcileStatus::DivergedTransient);

        // Third divergence → persistent
        store.update_reconcile_status(1, 10, 3000);
        assert_eq!(store.check_reconcile(1, 10), ReconcileStatus::DivergedPersistent);

        // Insert matching exch snapshot → in sync
        store.exch.insert(
            (1, 10),
            ExchPositionSnapshot {
                account_id: 1,
                instrument_code: String::new(),
                symbol_exch: None,
                position_state: Position {
                    total_qty: 100.0,
                    ..Default::default()
                },
                exch_data_raw: String::new(),
                sync_ts: 0,
            },
        );
        store.update_reconcile_status(1, 10, 4000);
        assert_eq!(store.check_reconcile(1, 10), ReconcileStatus::InSync);
    }
}
