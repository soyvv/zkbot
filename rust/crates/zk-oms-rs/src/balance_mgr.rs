use std::collections::HashMap;

use zk_proto_rs::zk::{
    exch_gw::v1::PositionReport,
    ods::v1::{GwConfigEntry, OmsRouteEntry},
    oms::v1::{Balance, BalanceUpdateEvent},
};

use crate::models::ExchBalanceSnapshot;

/// Manages exchange-owned asset balances.
///
/// Balance is exchange-owned state. OMS caches it but does not synthesize
/// or mutate it from order lifecycle events. The only writes come from
/// gateway balance updates.
pub struct BalanceManager {
    /// account_id → asset → exchange-reported balance snapshot
    pub(crate) exch_balances: HashMap<i64, HashMap<String, ExchBalanceSnapshot>>,
    /// exch_account_code → OmsRouteEntry (for mapping inbound updates)
    account_reverse_map: HashMap<String, OmsRouteEntry>,
    /// gw_key → GwConfigEntry
    gw_configs: HashMap<String, GwConfigEntry>,
}

impl BalanceManager {
    pub fn new(account_routes: &[OmsRouteEntry], gw_configs: &[GwConfigEntry]) -> Self {
        let mut mgr = Self {
            exch_balances: HashMap::new(),
            account_reverse_map: HashMap::new(),
            gw_configs: HashMap::new(),
        };
        mgr.reload_account_config(account_routes, gw_configs);
        mgr
    }

    // ------------------------------------------------------------------
    // Config reload
    // ------------------------------------------------------------------

    pub fn reload_account_config(
        &mut self,
        routes: &[OmsRouteEntry],
        gw_configs: &[GwConfigEntry],
    ) {
        self.gw_configs = gw_configs
            .iter()
            .map(|g| (g.gw_key.clone(), g.clone()))
            .collect();
        self.account_reverse_map = routes
            .iter()
            .map(|r| (r.exch_account_id.clone(), r.clone()))
            .collect();
        for r in routes {
            self.exch_balances.entry(r.account_id).or_default();
        }
    }

    // ------------------------------------------------------------------
    // Initialisation
    // ------------------------------------------------------------------

    pub fn init_balances(&mut self, balances: Vec<ExchBalanceSnapshot>) {
        for snap in balances {
            self.exch_balances
                .entry(snap.account_id)
                .or_default()
                .insert(snap.asset.clone(), snap);
        }
    }

    // ------------------------------------------------------------------
    // Read
    // ------------------------------------------------------------------

    pub fn get_balance(&self, account_id: i64, asset: &str) -> Option<&ExchBalanceSnapshot> {
        self.exch_balances.get(&account_id)?.get(asset)
    }

    pub fn get_balances_for_account(&self, account_id: i64) -> Vec<&ExchBalanceSnapshot> {
        self.exch_balances
            .get(&account_id)
            .map(|m| m.values().collect())
            .unwrap_or_default()
    }

    // ------------------------------------------------------------------
    // Account reverse map lookup (used by OmsCore gateway adapter)
    // ------------------------------------------------------------------

    pub fn resolve_account_id(&self, exch_account_code: &str) -> Option<i64> {
        self.account_reverse_map
            .get(exch_account_code)
            .map(|r| r.account_id)
    }

    // ------------------------------------------------------------------
    // Exchange balance update
    // ------------------------------------------------------------------

    /// Merge gateway-reported spot/asset balance entries.
    /// Called by OmsCore after classifying entries as balance-like (spot assets).
    pub fn merge_gw_balance_entries(
        &mut self,
        entries: &[PositionReport],
        account_id: i64,
        ts: i64,
    ) {
        for entry in entries {
            // For spot-like entries, instrument_code is the asset name (USDT, BTC, etc.)
            let asset = entry.instrument_code.clone();

            let snap = self
                .exch_balances
                .entry(account_id)
                .or_default()
                .entry(asset.clone())
                .or_insert_with(|| ExchBalanceSnapshot {
                    account_id,
                    asset: asset.clone(),
                    symbol_exch: Some(entry.instrument_code.clone()),
                    balance_state: Balance::default(),
                    exch_data_raw: String::new(),
                    sync_ts: 0,
                });

            snap.symbol_exch = Some(entry.instrument_code.clone());
            snap.balance_state.account_id = account_id;
            snap.balance_state.asset = asset;
            snap.balance_state.total_qty = entry.qty;
            snap.balance_state.avail_qty = entry.avail_qty;
            snap.balance_state.frozen_qty = entry.qty - entry.avail_qty;
            snap.balance_state.update_timestamp = entry.update_timestamp;
            snap.balance_state.sync_timestamp = ts;
            snap.balance_state.is_from_exch = true;
            snap.exch_data_raw = entry.message_raw.clone();
            snap.balance_state.exch_data_raw = entry.message_raw.clone();
            snap.sync_ts = ts;
        }
    }

    // ------------------------------------------------------------------
    // Publish
    // ------------------------------------------------------------------

    /// Build a `BalanceUpdateEvent` from exchange-owned balance state.
    pub fn build_balance_snapshot(&self, account_id: i64, ts: i64) -> BalanceUpdateEvent {
        let balances = self.get_balances_for_account(account_id);
        BalanceUpdateEvent {
            account_id,
            balance_snapshots: balances
                .iter()
                .map(|snap| snap.balance_state.clone())
                .collect(),
            timestamp: ts,
        }
    }

    // ------------------------------------------------------------------
    // Snapshot helpers (for read replica)
    // ------------------------------------------------------------------

    pub fn snapshot_exch_balances(&self) -> HashMap<i64, HashMap<String, ExchBalanceSnapshot>> {
        self.exch_balances.clone()
    }
}
