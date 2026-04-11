use std::collections::HashMap;

use zk_proto_rs::{
    ods::{GwConfigEntry, OmsConfigEntry, OmsRouteEntry},
    zk::{
        common::v1::{
            BasicOrderType, InstrumentRefData, InstrumentType, TimeInForceType,
        },
        exch_gw::v1::{
            order_report_entry::Report, BalanceUpdate, OrderReport, OrderReportType, PositionReport,
        },
        oms::v1::{Balance, OrderCancelRequest, OrderRequest},
    },
};

use zk_oms_rs::{
    config::{ConfdataManager, InstrumentTradingConfig},
    models::{ExchBalanceSnapshot, OmsAction, OmsManagedPosition, OmsMessage},
    oms_core::OmsCore,
};
use zk_strategy_sdk_rs::models::{StrategyCancel, StrategyOrder};

use crate::{match_policy::MatchPolicy, models::BtOmsOutput, simulator::SimulatorCore};

const SIM_GW_KEY: &str = "SIM1";

/// Backtester OMS: wraps the Rust `OmsCore` and `SimulatorCore`.
///
/// Converts `StrategyOrder` / `StrategyCancel` into OMS messages,
/// feeds resulting gateway requests into the simulator, and converts
/// simulator order reports back through OMS to produce `BtOmsOutput` events.
pub struct BacktestOms {
    oms: OmsCore,
    simulator: SimulatorCore,
    /// Simulated exchange-owned balances: account_id -> asset -> qty.
    /// Mirrors live behavior where canonical balances are exchange-owned.
    sim_balances: HashMap<i64, HashMap<String, f64>>,
}

impl BacktestOms {
    /// Create a backtester OMS.
    ///
    /// - `refdata`: instruments traded in this backtest session.
    /// - `account_ids`: strategy accounts (all routed to the SIM1 simulator gateway).
    /// - `match_policy`: fill simulation policy.
    /// - `init_balances`: optional initial holdings `account_id -> symbol -> qty`.
    /// - `init_positions`: optional initial instrument positions `account_id -> instrument_code -> qty`.
    pub fn new(
        refdata: Vec<InstrumentRefData>,
        account_ids: Vec<i64>,
        match_policy: Box<dyn MatchPolicy>,
        init_balances: Option<&HashMap<i64, HashMap<String, f64>>>,
        init_positions: Option<&HashMap<i64, HashMap<String, f64>>>,
    ) -> Self {
        let gw_configs = vec![GwConfigEntry {
            gw_key: SIM_GW_KEY.to_string(),
            exch_name: SIM_GW_KEY.to_string(),
            cancel_required_fields: vec!["order_id".to_string()],
            ..Default::default()
        }];

        let account_routes: Vec<OmsRouteEntry> = account_ids
            .iter()
            .map(|&acc_id| OmsRouteEntry {
                account_id: acc_id,
                gw_key: SIM_GW_KEY.to_string(),
                exch_account_id: acc_id.to_string(),
                ..Default::default()
            })
            .collect();

        let trading_configs: Vec<InstrumentTradingConfig> = refdata
            .iter()
            .map(|r| InstrumentTradingConfig {
                bookkeeping_balance: true,
                ..InstrumentTradingConfig::default_for(&r.instrument_id)
            })
            .collect();

        let oms_config = OmsConfigEntry {
            oms_id: "bt_oms".to_string(),
            managed_account_ids: account_ids.clone(),
            ..Default::default()
        };

        // Build init data before refdata is moved into ConfdataManager.
        let init_exch_balances = init_balances
            .map(|bals| build_init_exch_balances(bals))
            .unwrap_or_default();

        // Derive spot-inventory positions from balances + merge with explicit positions.
        let spot_assets = zk_oms_rs::utils::collect_spot_position_assets(&refdata);
        let mut derived_positions = Vec::new();
        if let Some(bals) = init_balances {
            for (&acc_id, sym_map) in bals {
                derived_positions.extend(zk_oms_rs::utils::derive_spot_positions_from_balances(
                    acc_id,
                    sym_map,
                    &spot_assets,
                ));
            }
        }
        let explicit_positions = init_positions
            .map(|p| build_init_managed_positions(p, &refdata))
            .unwrap_or_default();
        let merged_positions =
            zk_oms_rs::utils::merge_positions_with_override(derived_positions, explicit_positions);

        let confdata = ConfdataManager::new(
            oms_config,
            account_routes,
            gw_configs,
            refdata,
            trading_configs,
        );

        let mut oms = OmsCore::new(confdata, true, false, false, 10_000);
        oms.init_state(vec![], merged_positions, init_exch_balances);

        let simulator = SimulatorCore::new(match_policy, SIM_GW_KEY);

        let sim_balances = init_balances.cloned().unwrap_or_default();

        Self {
            oms,
            simulator,
            sim_balances,
        }
    }

    /// Process a strategy order placement.
    pub fn on_new_order(&mut self, order: &StrategyOrder, ts_ms: i64) -> Vec<BtOmsOutput> {
        let req = to_order_request(order, ts_ms);
        let oms_actions = self.oms.process_message(OmsMessage::PlaceOrder(req));
        self.handle_oms_actions(oms_actions, ts_ms)
    }

    /// Process a tick: feeds the orderbook into the simulator, drives pending fills.
    pub fn on_tick(&mut self, tick: &zk_proto_rs::zk::rtmd::v1::TickData) -> Vec<BtOmsOutput> {
        let ts_ms = tick.original_timestamp;
        let sim_results = self.simulator.on_tick(tick);
        self.process_sim_results(sim_results, ts_ms)
    }

    /// Process a cancel request from a strategy.
    pub fn on_cancel(&mut self, cancel: &StrategyCancel, ts_ms: i64) -> Vec<BtOmsOutput> {
        let req = OrderCancelRequest {
            order_id: cancel.order_id,
            timestamp: ts_ms,
            ..Default::default()
        };
        let oms_actions = self.oms.process_message(OmsMessage::CancelOrder(req));
        self.handle_oms_actions(oms_actions, ts_ms)
    }

    // --- internal ---

    fn handle_oms_actions(&mut self, actions: Vec<OmsAction>, ts_ms: i64) -> Vec<BtOmsOutput> {
        let mut results = Vec::new();
        for action in actions {
            match action {
                OmsAction::SendOrderToGw { request, .. } => {
                    let sim_results = self.simulator.on_new_order(request, ts_ms);
                    results.extend(self.process_sim_results(sim_results, ts_ms));
                }
                OmsAction::SendCancelToGw { request, .. } => {
                    let sim_results = self.simulator.on_cancel(&request, ts_ms);
                    results.extend(self.process_sim_results(sim_results, ts_ms));
                }
                OmsAction::PublishOrderUpdate(oue) => {
                    results.push(BtOmsOutput {
                        ts_ms: ts_ms + 1,
                        order_update: Some(*oue),
                        balance_update: None,
                        position_update: None,
                    });
                }
                OmsAction::PublishBalanceUpdate(bue) => {
                    results.push(BtOmsOutput {
                        ts_ms: ts_ms + 1,
                        order_update: None,
                        balance_update: Some(*bue),
                        position_update: None,
                    });
                }
                OmsAction::PublishPositionUpdate(pue) => {
                    results.push(BtOmsOutput {
                        ts_ms: ts_ms + 1,
                        order_update: None,
                        balance_update: None,
                        position_update: Some(*pue),
                    });
                }
                // Persist and batch actions are no-ops in backtest
                _ => {}
            }
        }
        results
    }

    fn process_sim_results(
        &mut self,
        sim_results: Vec<crate::models::SimResult>,
        _base_ts: i64,
    ) -> Vec<BtOmsOutput> {
        let mut outputs = Vec::new();
        for sr in sim_results {
            let ts = sr.ts_ms;

            // Extract fill info before passing report to OMS.
            let fills = extract_fills(&sr.order_report);

            let oms_actions = self
                .oms
                .process_message(OmsMessage::GatewayOrderReport(sr.order_report));
            for action in oms_actions {
                match action {
                    OmsAction::PublishOrderUpdate(oue) => {
                        outputs.push(BtOmsOutput {
                            ts_ms: ts + 2,
                            order_update: Some(*oue),
                            balance_update: None,
                            position_update: None,
                        });
                    }
                    OmsAction::PublishBalanceUpdate(bue) => {
                        outputs.push(BtOmsOutput {
                            ts_ms: ts + 2,
                            order_update: None,
                            balance_update: Some(*bue),
                            position_update: None,
                        });
                    }
                    OmsAction::PublishPositionUpdate(pue) => {
                        outputs.push(BtOmsOutput {
                            ts_ms: ts + 2,
                            order_update: None,
                            balance_update: None,
                            position_update: Some(*pue),
                        });
                    }
                    _ => {}
                }
            }

            // After processing fills, synthesize a simulated exchange BalanceUpdate
            // and feed it through OMS so BalanceManager gets updated.
            if !fills.is_empty() {
                for fill in &fills {
                    self.apply_sim_fill(
                        fill.account_id,
                        &fill.instrument,
                        fill.side,
                        fill.qty,
                        fill.price,
                    );
                }
                let mut accounts: Vec<i64> = fills.iter().map(|f| f.account_id).collect();
                accounts.sort_unstable();
                accounts.dedup();
                for acc_id in accounts {
                    let balance_update = self.build_sim_balance_update(acc_id);
                    let bal_actions = self
                        .oms
                        .process_message(OmsMessage::BalanceUpdate(balance_update));
                    for action in bal_actions {
                        match action {
                            OmsAction::PublishBalanceUpdate(bue) => {
                                outputs.push(BtOmsOutput {
                                    ts_ms: ts + 2,
                                    order_update: None,
                                    balance_update: Some(*bue),
                                    position_update: None,
                                });
                            }
                            OmsAction::PublishPositionUpdate(pue) => {
                                outputs.push(BtOmsOutput {
                                    ts_ms: ts + 2,
                                    order_update: None,
                                    balance_update: None,
                                    position_update: Some(*pue),
                                });
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        outputs
    }

    /// Apply a fill to the simulated exchange balance ledger.
    fn apply_sim_fill(
        &mut self,
        account_id: i64,
        instrument: &str,
        side: i32,
        qty: f64,
        price: f64,
    ) {
        let (base, quote) = instrument
            .split_once('-')
            .map(|(b, q)| (b.to_string(), Some(q.to_string())))
            .unwrap_or((instrument.to_string(), None));

        let is_buy = side == 1; // BuySellType::BsBuy
        let value = price * qty;
        let acct = self.sim_balances.entry(account_id).or_default();

        if is_buy {
            *acct.entry(base).or_insert(0.0) += qty;
            if let Some(q) = quote {
                *acct.entry(q).or_insert(0.0) -= value;
            }
        } else {
            *acct.entry(base).or_insert(0.0) -= qty;
            if let Some(q) = quote {
                *acct.entry(q).or_insert(0.0) += value;
            }
        }
    }

    /// Build a simulated exchange BalanceUpdate for a given account.
    fn build_sim_balance_update(&self, account_id: i64) -> BalanceUpdate {
        let acct_code = account_id.to_string();
        let entries = self
            .sim_balances
            .get(&account_id)
            .map(|m| {
                m.iter()
                    .map(|(asset, &qty)| PositionReport {
                        instrument_code: asset.clone(),
                        instrument_type: InstrumentType::InstTypeSpot as i32,
                        qty,
                        avail_qty: qty,
                        account_id,
                        exch_account_code: acct_code.clone(),
                        ..Default::default()
                    })
                    .collect()
            })
            .unwrap_or_default();
        BalanceUpdate { balances: entries }
    }
}

fn to_order_request(order: &StrategyOrder, ts_ms: i64) -> OrderRequest {
    OrderRequest {
        order_id: order.order_id,
        account_id: order.account_id,
        instrument_code: order.symbol.clone(),
        buy_sell_type: order.side,
        open_close_type: order.open_close_type,
        order_type: BasicOrderType::OrdertypeLimit as i32,
        time_inforce_type: TimeInForceType::TimeinforceGtc as i32,
        price: order.price,
        qty: order.qty,
        timestamp: ts_ms,
        ..Default::default()
    }
}

/// Build initial exchange-owned balance snapshots from init_balances.
/// Under the domain split, spot assets (USDT, BTC, etc.) are Balances, not Positions.
fn build_init_exch_balances(
    balances: &HashMap<i64, HashMap<String, f64>>,
) -> Vec<ExchBalanceSnapshot> {
    let mut snapshots = Vec::new();
    for (&acc_id, sym_map) in balances {
        for (sym, &qty) in sym_map {
            snapshots.push(ExchBalanceSnapshot {
                account_id: acc_id,
                asset: sym.clone(),
                symbol_exch: None,
                balance_state: Balance {
                    account_id: acc_id,
                    asset: sym.clone(),
                    total_qty: qty,
                    avail_qty: qty,
                    frozen_qty: 0.0,
                    ..Default::default()
                },
                exch_data_raw: String::new(),
                sync_ts: 0,
            });
        }
    }
    snapshots
}

/// Build initial managed positions from init_positions config.
/// Positions are instrument-level exposure (perps, futures, stocks); negative qty = short.
fn build_init_managed_positions(
    positions: &HashMap<i64, HashMap<String, f64>>,
    refdata: &[InstrumentRefData],
) -> Vec<OmsManagedPosition> {
    let mut result = Vec::new();
    for (&acc_id, sym_map) in positions {
        for (instrument_code, &qty) in sym_map {
            let inst_type = refdata
                .iter()
                .find(|r| r.instrument_id == *instrument_code)
                .map(|r| r.instrument_type)
                .unwrap_or(InstrumentType::InstTypePerp as i32);
            let is_short = qty < 0.0;
            let abs_qty = qty.abs();
            let mut pos =
                OmsManagedPosition::new(acc_id, instrument_code.clone(), inst_type, is_short);
            pos.qty_total = abs_qty;
            pos.qty_available = abs_qty;
            result.push(pos);
        }
    }
    result
}

/// Info extracted from a single trade in an OrderReport.
struct FillInfo {
    account_id: i64,
    instrument: String,
    side: i32,
    qty: f64,
    price: f64,
}

/// Extract fill information from an OrderReport's trade entries.
fn extract_fills(report: &OrderReport) -> Vec<FillInfo> {
    let mut fills = Vec::new();
    for entry in &report.order_report_entries {
        if entry.report_type == OrderReportType::OrderRepTypeTrade as i32 {
            if let Some(Report::TradeReport(trade)) = &entry.report {
                let (instrument, side) = trade
                    .order_info
                    .as_ref()
                    .map(|info| (info.instrument.clone(), info.buy_sell_type))
                    .unwrap_or_default();
                if trade.filled_qty > 0.0 {
                    fills.push(FillInfo {
                        account_id: report.account_id,
                        instrument,
                        side,
                        qty: trade.filled_qty,
                        price: trade.filled_price,
                    });
                }
            }
        }
    }
    fills
}
