use std::collections::HashMap;

use tracing::error;
use zk_proto_rs::{
    zk::{
        common::v1::{InstrumentRefData, LongShortType},
        exch_gw::v1::BalanceUpdate,
        oms::v1::{Balance, BalanceUpdateEvent},
    },
    ods::{GwConfigEntry, OmsRouteEntry},
};

use crate::{
    models::{OmsPosition, PositionChange},
};

/// Manages OMS-tracked balances (bookkeeping) and exchange-synced balances.
/// Mirrors Python `BalanceManager`.
pub struct BalanceManager {
    /// account_id → symbol → OmsPosition  (OMS-maintained bookkeeping balance)
    pub(crate) balances: HashMap<i64, HashMap<String, OmsPosition>>,
    /// account_id → symbol → OmsPosition  (last exchange-reported balance)
    pub(crate) exch_balances: HashMap<i64, HashMap<String, OmsPosition>>,
    /// exch_account_code → OmsRouteEntry  (for mapping inbound balance updates)
    account_reverse_map: HashMap<String, OmsRouteEntry>,
    /// gw_key → GwConfigEntry
    gw_configs: HashMap<String, GwConfigEntry>,
    /// account_id → (instrument_id_exchange → InstrumentRefData)
    symbol_map: HashMap<i64, HashMap<String, InstrumentRefData>>,
}

impl BalanceManager {
    pub fn new(
        symbol_map: HashMap<i64, HashMap<String, InstrumentRefData>>,
        account_routes: &[OmsRouteEntry],
        gw_configs: &[GwConfigEntry],
    ) -> Self {
        let mut mgr = Self {
            balances: HashMap::new(),
            exch_balances: HashMap::new(),
            account_reverse_map: HashMap::new(),
            gw_configs: HashMap::new(),
            symbol_map,
        };
        mgr.reload_account_config(account_routes, gw_configs);
        mgr
    }

    // ------------------------------------------------------------------
    // Config reload
    // ------------------------------------------------------------------

    pub fn reload_symbol_map(&mut self, symbol_map: HashMap<i64, HashMap<String, InstrumentRefData>>) {
        self.symbol_map = symbol_map;
    }

    pub fn reload_account_config(&mut self, routes: &[OmsRouteEntry], gw_configs: &[GwConfigEntry]) {
        self.gw_configs = gw_configs.iter().map(|g| (g.gw_key.clone(), g.clone())).collect();
        self.account_reverse_map = routes
            .iter()
            .map(|r| (r.exch_account_id.clone(), r.clone()))
            .collect();
        for r in routes {
            self.balances.entry(r.account_id).or_default();
            self.exch_balances.entry(r.account_id).or_default();
        }
    }

    // ------------------------------------------------------------------
    // Initialisation
    // ------------------------------------------------------------------

    pub fn init_balances(&mut self, init_positions: Vec<OmsPosition>) {
        for pos in init_positions {
            // Update exchange-reported side
            let exch_copy = pos.clone();
            self.update_balance(exch_copy, true);

            // Seed OMS-managed side (clear exchange metadata)
            let mut oms_copy = pos;
            oms_copy.position_state.sync_timestamp = 0;
            oms_copy.position_state.is_from_exch = false;
            oms_copy.position_state.exch_data_raw = String::new();
            self.update_balance(oms_copy, false);
        }
    }

    // ------------------------------------------------------------------
    // Read
    // ------------------------------------------------------------------

    pub fn get_balances_for_account(&self, account_id: i64, use_exch_data: bool) -> Vec<&OmsPosition> {
        let src = if use_exch_data { &self.exch_balances } else { &self.balances };
        src.get(&account_id)
            .map(|m| m.values().collect())
            .unwrap_or_default()
    }

    pub fn get_balance_for_symbol(
        &mut self,
        account_id: i64,
        symbol: &str,
        is_short: bool,
        use_exch_data: bool,
        create_if_missing: bool,
    ) -> Option<&mut OmsPosition> {
        let src = if use_exch_data { &mut self.exch_balances } else { &mut self.balances };

        if create_if_missing && !src.entry(account_id).or_default().contains_key(symbol) {
            // Use SPOT as default instrument type when we can't look up refdata
            let pos = OmsPosition::new(
                account_id,
                symbol,
                zk_proto_rs::zk::common::v1::InstrumentType::InstTypeSpot as i32,
                is_short,
                use_exch_data,
            );
            src.entry(account_id).or_default().insert(symbol.to_string(), pos);
        }

        src.get_mut(&account_id)?.get_mut(symbol)
    }

    // ------------------------------------------------------------------
    // Write helpers
    // ------------------------------------------------------------------

    pub fn update_balance(&mut self, pos: OmsPosition, update_exch_data: bool) {
        let src = if update_exch_data { &mut self.exch_balances } else { &mut self.balances };
        src.entry(pos.account_id).or_default().insert(pos.symbol.clone(), pos);
    }

    pub fn create_balance_change(&self, account_id: i64, symbol: &str, is_short: bool) -> PositionChange {
        PositionChange {
            account_id,
            symbol: symbol.to_string(),
            is_short,
            avail_change: 0.0,
            frozen_change: 0.0,
            total_change: 0.0,
        }
    }

    /// Validate that proposed balance changes don't go negative.
    pub fn check_changes(&self, changes: &[PositionChange]) -> Result<(), String> {
        for pc in changes {
            if let Some(pos_map) = self.balances.get(&pc.account_id) {
                if let Some(pos) = pos_map.get(&pc.symbol) {
                    let p = &pos.position_state;
                    if p.avail_qty + pc.avail_change < 0.0 || p.total_qty + pc.total_change < 0.0 {
                        return Err(format!(
                            "Not enough available balance for {}",
                            p.instrument_code
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// Apply balance changes and return the set of modified symbols.
    pub fn apply_changes(&mut self, changes: &[PositionChange], ts: i64) -> Vec<String> {
        let mut changed = Vec::new();
        for pc in changes {
            changed.push(pc.symbol.clone());
            let pos_map = self.balances.entry(pc.account_id).or_default();
            if let Some(pos) = pos_map.get_mut(&pc.symbol) {
                let p = &mut pos.position_state;
                p.total_qty += pc.total_change;
                p.frozen_qty += pc.frozen_change;

                // Clamp avail_qty to total_qty
                let new_avail = p.avail_qty + pc.avail_change;
                p.avail_qty = if new_avail > p.total_qty { p.total_qty } else { new_avail };
                p.update_timestamp = ts;

                // Handle sign flip (total goes negative → flip long/short)
                if p.total_qty < 0.0 {
                    pos.is_short = !pos.is_short;
                    pos.position_state.long_short_type = if pos.is_short {
                        LongShortType::LsShort as i32
                    } else {
                        LongShortType::LsLong as i32
                    };
                    pos.position_state.total_qty = pos.position_state.total_qty.abs();
                    pos.position_state.avail_qty = pos.position_state.avail_qty.abs();
                }
            }
        }
        changed
    }

    // ------------------------------------------------------------------
    // Exchange balance update
    // ------------------------------------------------------------------

    /// Merge a gateway `BalanceUpdate` into exchange-side balances.
    /// Returns the account_id that was updated, or None on error.
    pub fn merge_gw_balance_update(&mut self, update: &BalanceUpdate, ts: i64) -> Option<i64> {
        if update.balances.is_empty() {
            return None;
        }
        let exch_account_code = &update.balances[0].exch_account_code;
        let account_id = match self.account_reverse_map.get(exch_account_code) {
            Some(r) => r.account_id,
            None => {
                error!(exch_account_code, "unknown account code in balance update");
                return None;
            }
        };

        // Only seed OMS-managed balance on first sync (when our managed map is empty)
        let no_oms_balance = self.balances.get(&account_id).map(|m| m.is_empty()).unwrap_or(true);

        // Spot-like types: SPOT, ETF, STOCK (instrument_type < 10 roughly)
        const SPOT_LIKE: &[i32] = &[1, 6, 7]; // INST_TYPE_SPOT, ETF, STOCK

        for b in &update.balances {
            let is_spot_like = SPOT_LIKE.contains(&b.instrument_type);
            let symbol = if is_spot_like {
                b.instrument_code.clone()
            } else {
                match self
                    .symbol_map
                    .get(&account_id)
                    .and_then(|m| m.get(&b.instrument_code))
                {
                    Some(ref_data) => ref_data.instrument_id.clone(),
                    None => {
                        error!(instrument_code = b.instrument_code, "refdata not found for balance; skipping");
                        continue;
                    }
                }
            };

            let is_short = b.long_short_type == LongShortType::LsShort as i32;

            // Update exchange-side balance
            {
                let exch_pos = self.exch_balances
                    .entry(account_id)
                    .or_default()
                    .entry(symbol.clone())
                    .or_insert_with(|| OmsPosition::new(account_id, &symbol, b.instrument_type, is_short, true));

                exch_pos.symbol_exch = Some(b.instrument_code.clone());
                exch_pos.is_short = is_short;
                exch_pos.position_state.frozen_qty = b.qty - b.avail_qty;
                exch_pos.position_state.avail_qty = b.avail_qty;
                exch_pos.position_state.total_qty = b.qty;
                exch_pos.position_state.long_short_type = b.long_short_type;
                exch_pos.position_state.instrument_type = b.instrument_type;
                exch_pos.position_state.update_timestamp = b.update_timestamp;
                exch_pos.position_state.sync_timestamp = ts;
                exch_pos.position_state.exch_data_raw = b.message_raw.clone();
            }

            // Optionally seed OMS-managed balance
            let oms_pos_sync_ts = self
                .balances
                .get(&account_id)
                .and_then(|m| m.get(&symbol))
                .map(|p| p.position_state.sync_timestamp);

            let should_seed = no_oms_balance || oms_pos_sync_ts.is_none_or(|t| t == 0);
            if should_seed {
                let mut seeded = self.exch_balances[&account_id][&symbol].clone();
                seeded.position_state.is_from_exch = false;
                seeded.position_state.exch_data_raw = String::new();
                self.balances
                    .entry(account_id)
                    .or_default()
                    .insert(symbol.clone(), seeded);
            }
        }
        Some(account_id)
    }

    /// Build a full position snapshot for an account.
    pub fn build_position_snapshot(
        &self,
        account_id: i64,
        use_exch_data: bool,
        ts: i64,
    ) -> zk_proto_rs::zk::oms::v1::PositionUpdateEvent {
        let positions = self.get_balances_for_account(account_id, use_exch_data);
        let mut event = zk_proto_rs::zk::oms::v1::PositionUpdateEvent::default();
        event.account_id = account_id;
        event.position_snapshots = positions.iter().map(|p| p.position_state.clone()).collect();
        event.timestamp = ts;
        event
    }

    /// Build a full balance snapshot for an account using the new `Balance` proto type.
    pub fn build_balance_snapshot(
        &self,
        account_id: i64,
        use_exch_data: bool,
        ts: i64,
    ) -> BalanceUpdateEvent {
        let positions = self.get_balances_for_account(account_id, use_exch_data);
        BalanceUpdateEvent {
            account_id,
            balance_snapshots: positions
                .iter()
                .map(|p| Balance {
                    account_id,
                    asset: p.position_state.instrument_code.clone(),
                    total_qty: p.position_state.total_qty,
                    frozen_qty: p.position_state.frozen_qty,
                    avail_qty: p.position_state.avail_qty,
                    sync_timestamp: p.position_state.sync_timestamp,
                    update_timestamp: p.position_state.update_timestamp,
                    is_from_exch: p.position_state.is_from_exch,
                    exch_data_raw: p.position_state.exch_data_raw.clone(),
                })
                .collect(),
            timestamp: ts,
        }
    }

    /// Build a partial position snapshot for a set of symbols.
    pub fn build_position_snapshot_for_symbols(
        &self,
        account_id: i64,
        symbols: &[String],
        use_exch_data: bool,
        ts: i64,
    ) -> zk_proto_rs::zk::oms::v1::PositionUpdateEvent {
        let src = if use_exch_data { &self.exch_balances } else { &self.balances };
        let snapshots: Vec<_> = symbols
            .iter()
            .filter_map(|s| src.get(&account_id)?.get(s))
            .map(|p| p.position_state.clone())
            .collect();

        let mut event = zk_proto_rs::zk::oms::v1::PositionUpdateEvent::default();
        event.account_id = account_id;
        event.position_snapshots = snapshots;
        event.timestamp = ts;
        event
    }
}
