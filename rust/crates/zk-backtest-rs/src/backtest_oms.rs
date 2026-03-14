use std::collections::HashMap;

use zk_proto_rs::{
    zk::{
        common::v1::{BasicOrderType, OpenCloseType, TimeInForceType, InstrumentRefData},
        oms::v1::{OrderCancelRequest, OrderRequest},
    },
    ods::{GwConfigEntry, OmsConfigEntry, OmsRouteEntry},
};

use zk_oms_rs::{
    config::{ConfdataManager, InstrumentTradingConfig},
    models::{OmsAction, OmsMessage, OmsPosition},
    oms_core::OmsCore,
};
use zk_strategy_sdk_rs::models::{StrategyCancel, StrategyOrder};

use crate::{
    match_policy::MatchPolicy,
    models::BtOmsOutput,
    simulator::SimulatorCore,
};

const SIM_GW_KEY: &str = "SIM1";

/// Backtester OMS: wraps the Rust `OmsCore` and `SimulatorCore`.
///
/// Converts `StrategyOrder` / `StrategyCancel` into OMS messages,
/// feeds resulting gateway requests into the simulator, and converts
/// simulator order reports back through OMS to produce `BtOmsOutput` events.
pub struct BacktestOms {
    oms: OmsCore,
    simulator: SimulatorCore,
}

impl BacktestOms {
    /// Create a backtester OMS.
    ///
    /// - `refdata`: instruments traded in this backtest session.
    /// - `account_ids`: strategy accounts (all routed to the SIM1 simulator gateway).
    /// - `match_policy`: fill simulation policy.
    /// - `init_balances`: optional initial holdings `account_id → symbol → qty`.
    pub fn new(
        refdata: Vec<InstrumentRefData>,
        account_ids: Vec<i64>,
        match_policy: Box<dyn MatchPolicy>,
        init_balances: Option<&HashMap<i64, HashMap<String, f64>>>,
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

        let confdata = ConfdataManager::new(oms_config, account_routes, gw_configs, refdata, trading_configs);

        let mut oms = OmsCore::new(confdata, true, false, false, 10_000);

        // Seed initial balances
        let init_positions = init_balances
            .map(|bals| build_init_positions(bals))
            .unwrap_or_default();
        oms.init_state(vec![], init_positions);

        let simulator = SimulatorCore::new(match_policy, SIM_GW_KEY);

        Self { oms, simulator }
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
                        position_update: None,
                    });
                }
                OmsAction::PublishBalanceUpdate(pue) => {
                    results.push(BtOmsOutput {
                        ts_ms: ts_ms + 1,
                        order_update: None,
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
            let oms_actions = self
                .oms
                .process_message(OmsMessage::GatewayOrderReport(sr.order_report));
            for action in oms_actions {
                match action {
                    OmsAction::PublishOrderUpdate(oue) => {
                        outputs.push(BtOmsOutput {
                            ts_ms: ts + 2,
                            order_update: Some(*oue),
                            position_update: None,
                        });
                    }
                    OmsAction::PublishBalanceUpdate(pue) => {
                        outputs.push(BtOmsOutput {
                            ts_ms: ts + 2,
                            order_update: None,
                            position_update: Some(*pue),
                        });
                    }
                    _ => {}
                }
            }
        }
        outputs
    }
}

fn to_order_request(order: &StrategyOrder, ts_ms: i64) -> OrderRequest {
    OrderRequest {
        order_id: order.order_id,
        account_id: order.account_id,
        instrument_code: order.symbol.clone(),
        buy_sell_type: order.side,
        open_close_type: OpenCloseType::OcOpen as i32,
        order_type: BasicOrderType::OrdertypeLimit as i32,
        time_inforce_type: TimeInForceType::TimeinforceGtc as i32,
        price: order.price,
        qty: order.qty,
        timestamp: ts_ms,
        ..Default::default()
    }
}

fn build_init_positions(balances: &HashMap<i64, HashMap<String, f64>>) -> Vec<OmsPosition> {
    use zk_proto_rs::zk::common::v1::InstrumentType;
    let mut positions = Vec::new();
    for (&acc_id, sym_map) in balances {
        for (sym, &qty) in sym_map {
            let mut pos = OmsPosition::new(
                acc_id,
                sym.clone(),
                InstrumentType::InstTypeSpot as i32,
                false,
                false,
            );
            pos.position_state.avail_qty = qty;
            pos.position_state.total_qty = qty;
            positions.push(pos);
        }
    }
    positions
}
