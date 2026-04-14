use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use tokio::sync::Mutex;
use tracing::info;

use zk_proto_rs::zk::exch_gw::v1::{order_report_entry::Report, OrderReport, OrderReportType};
use zk_proto_rs::zk::gateway::v1::CancelOrderRequest as ExchCancelOrderRequest;
use zk_proto_rs::zk::gateway::v1::SendOrderRequest as ExchSendOrderRequest;

use zk_sim_core::match_policy::{
    FirstComeFirstServedMatchPolicy, ImmediateMatchPolicy, MatchPolicy,
};

use crate::venue::simulator::error_injection::{
    evaluate_rules, ErrorEffect, ErrorScope, RequestContext,
};
use crate::venue::simulator::state::SimulatorState;
use crate::venue_adapter::*;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Create a MatchPolicy from a policy name.
pub fn make_match_policy(name: &str) -> Box<dyn MatchPolicy> {
    match name {
        "fcfs" => Box::new(FirstComeFirstServedMatchPolicy),
        _ => Box::new(ImmediateMatchPolicy),
    }
}

/// Simulator venue adapter: implements `VenueAdapter` backed by `zk-sim-core::SimulatorCore`.
pub struct SimulatorVenueAdapter {
    pub state: Arc<Mutex<SimulatorState>>,
    event_tx: tokio::sync::mpsc::Sender<VenueEvent>,
    event_rx: Arc<Mutex<tokio::sync::mpsc::Receiver<VenueEvent>>>,
}

impl SimulatorVenueAdapter {
    pub fn new(state: Arc<Mutex<SimulatorState>>) -> Self {
        let (tx, rx) = tokio::sync::mpsc::channel(1024);
        Self {
            state,
            event_tx: tx,
            event_rx: Arc::new(Mutex::new(rx)),
        }
    }

    /// Queue events internally (used by admin RPCs too).
    pub async fn queue_event(&self, event: VenueEvent) {
        let _ = self.event_tx.send(event).await;
    }

    /// Extract order info from report entries for balance updates.
    pub fn extract_fill_info(report: &OrderReport) -> Option<(String, i32, f64, f64)> {
        let mut instrument = String::new();
        let mut side = 0i32;
        let mut fill_qty = 0.0;
        let mut fill_price = 0.0;

        for entry in &report.order_report_entries {
            if entry.report_type == OrderReportType::OrderRepTypeTrade as i32 {
                if let Some(Report::TradeReport(trade)) = &entry.report {
                    fill_qty = trade.filled_qty;
                    fill_price = trade.filled_price;
                    if let Some(ref info) = trade.order_info {
                        instrument = info.instrument.clone();
                        side = info.buy_sell_type;
                    }
                }
            }
        }

        if fill_qty > 0.0 && !instrument.is_empty() {
            Some((instrument, side, fill_qty, fill_price))
        } else {
            None
        }
    }
}

/// Helper: check error injection rules, returning early on RPC error or reject.
/// Returns `Some(delay_ms)` if a delay effect was found, `None` otherwise.
async fn check_error_injection(
    state: &mut SimulatorState,
    scope: &ErrorScope,
    ctx: &RequestContext,
) -> Result<Option<u64>, VenueCommandAck> {
    if let Some(effect) = evaluate_rules(&mut state.control_state.error_rules, scope, ctx) {
        match effect {
            ErrorEffect::RpcError { error_message, .. } => {
                return Err(VenueCommandAck {
                    success: false,
                    exch_order_ref: None,
                    error_message: Some(error_message),
                });
            }
            ErrorEffect::RejectOrder { error_message } => {
                return Err(VenueCommandAck {
                    success: false,
                    exch_order_ref: None,
                    error_message: Some(error_message),
                });
            }
            ErrorEffect::DelayResponse { delay_ms } => {
                return Ok(Some(delay_ms));
            }
        }
    }
    Ok(None)
}

fn sim_order_to_venue_fact(o: &zk_sim_core::models::SimOrder) -> VenueOrderFact {
    let status = if o.remaining_qty <= 0.0 {
        VenueOrderStatus::Filled
    } else if o.filled_qty > 0.0 {
        VenueOrderStatus::PartiallyFilled
    } else {
        VenueOrderStatus::Booked
    };
    VenueOrderFact {
        order_id: o.req.correlation_id,
        exch_order_ref: o.exch_order_id.clone(),
        instrument: o.req.instrument.clone(),
        status,
        filled_qty: o.filled_qty,
        unfilled_qty: o.remaining_qty,
        avg_price: 0.0, // simulator does not track avg price on cached orders
        timestamp: o.update_ts,
    }
}

#[async_trait]
impl VenueAdapter for SimulatorVenueAdapter {
    async fn connect(&self) -> anyhow::Result<()> {
        info!("simulator venue adapter connected");
        Ok(())
    }

    async fn place_order(&self, req: VenuePlaceOrder) -> anyhow::Result<VenueCommandAck> {
        let ts = now_ms();

        // Hold a single lock for error injection + order placement + balance update.
        let (results, balance_snapshot) = {
            let mut state = self.state.lock().await;

            let ctx = RequestContext {
                instrument: Some(req.instrument.clone()),
                side: Some(req.buysell_type),
                ..Default::default()
            };
            match check_error_injection(&mut state, &ErrorScope::PlaceOrder, &ctx).await {
                Err(ack) => return Ok(ack),
                Ok(Some(delay_ms)) => {
                    // Release lock during sleep, then re-acquire.
                    drop(state);
                    tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                    state = self.state.lock().await;
                }
                Ok(None) => {}
            }

            let send_req = ExchSendOrderRequest {
                correlation_id: req.correlation_id,
                exch_account_id: req.exch_account_id,
                instrument: req.instrument,
                buysell_type: req.buysell_type,
                openclose_type: req.openclose_type,
                order_type: req.order_type,
                scaled_price: req.price,
                scaled_qty: req.qty,
                leverage: req.leverage,
                timestamp: req.timestamp,
                ..Default::default()
            };

            let results = if state.control_state.matching_paused {
                state.sim_core.on_new_order_book_only(send_req, ts)
            } else {
                state.sim_core.on_new_order(send_req, ts)
            };

            // Update balances for any fills within the same lock.
            let mut has_fills = false;
            for result in &results {
                if let Some((instrument, side, qty, price)) =
                    Self::extract_fill_info(&result.order_report)
                {
                    state
                        .account_state
                        .apply_fill_to_balances(&instrument, side, qty, price);
                    has_fills = true;
                }
            }

            let balance_snapshot = if has_fills {
                Some(
                    state
                        .account_state
                        .balances
                        .iter()
                        .map(|(asset, entry)| VenueBalanceFact {
                            asset: asset.clone(),
                            total_qty: entry.total_qty,
                            avail_qty: entry.avail_qty,
                            frozen_qty: entry.frozen_qty,
                        })
                        .collect::<Vec<_>>(),
                )
            } else {
                None
            };

            (results, balance_snapshot)
        };

        // Queue events outside the state lock.
        if let Some(balance_facts) = balance_snapshot {
            let _ = self.event_tx.send(VenueEvent::Balance(balance_facts)).await;
        }
        for result in &results {
            let _ = self
                .event_tx
                .send(VenueEvent::OrderReport(result.order_report.clone()))
                .await;
        }

        Ok(VenueCommandAck {
            success: true,
            exch_order_ref: results
                .first()
                .map(|r| r.order_report.exch_order_ref.clone()),
            error_message: None,
        })
    }

    async fn cancel_order(&self, req: VenueCancelOrder) -> anyhow::Result<VenueCommandAck> {
        let ts = now_ms();

        // Single lock for error injection + cancel.
        let results = {
            let mut state = self.state.lock().await;

            let ctx = RequestContext {
                order_id: Some(req.order_id),
                ..Default::default()
            };
            match check_error_injection(&mut state, &ErrorScope::CancelOrder, &ctx).await {
                Err(ack) => return Ok(ack),
                Ok(Some(delay_ms)) => {
                    drop(state);
                    tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                    state = self.state.lock().await;
                }
                Ok(None) => {}
            }

            let cancel_req = ExchCancelOrderRequest {
                exch_order_ref: req.exch_order_ref,
                order_id: req.order_id,
                timestamp: req.timestamp,
                ..Default::default()
            };

            state.sim_core.on_cancel(&cancel_req, ts)
        };

        for result in &results {
            let _ = self
                .event_tx
                .send(VenueEvent::OrderReport(result.order_report.clone()))
                .await;
        }

        Ok(VenueCommandAck {
            success: !results.is_empty(),
            exch_order_ref: None,
            error_message: if results.is_empty() {
                Some("order not found or already filled".to_string())
            } else {
                None
            },
        })
    }

    async fn query_balance(
        &self,
        _req: VenueBalanceQuery,
    ) -> anyhow::Result<Vec<VenueBalanceFact>> {
        // Single lock for error injection + query.
        let mut state = self.state.lock().await;
        let ctx = RequestContext::default();
        if let Some(effect) = evaluate_rules(
            &mut state.control_state.error_rules,
            &ErrorScope::QueryAccountBalance,
            &ctx,
        ) {
            match effect {
                ErrorEffect::RpcError { error_message, .. } => {
                    return Err(anyhow::anyhow!(error_message));
                }
                ErrorEffect::RejectOrder { error_message } => {
                    return Err(anyhow::anyhow!("query rejected: {}", error_message));
                }
                ErrorEffect::DelayResponse { delay_ms } => {
                    drop(state);
                    tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                    state = self.state.lock().await;
                }
            }
        }

        Ok(state
            .account_state
            .balances
            .iter()
            .map(|(asset, entry)| VenueBalanceFact {
                asset: asset.clone(),
                total_qty: entry.total_qty,
                avail_qty: entry.avail_qty,
                frozen_qty: entry.frozen_qty,
            })
            .collect())
    }

    async fn query_order(&self, req: VenueOrderQuery) -> anyhow::Result<Vec<VenueOrderFact>> {
        let state = self.state.lock().await;
        let mut results = Vec::new();
        for order in state.sim_core.order_cache().values() {
            let matches = req
                .order_id
                .map_or(false, |id| order.req.correlation_id == id)
                || req
                    .exch_order_ref
                    .as_deref()
                    .map_or(false, |r| r == order.exch_order_id)
                || req
                    .instrument
                    .as_deref()
                    .map_or(false, |i| i == order.req.instrument);
            if matches {
                results.push(sim_order_to_venue_fact(order));
            }
        }
        Ok(results)
    }

    async fn query_open_orders(
        &self,
        _req: VenueOpenOrdersQuery,
    ) -> anyhow::Result<Vec<VenueOrderFact>> {
        let state = self.state.lock().await;
        Ok(state
            .sim_core
            .order_cache()
            .values()
            .filter(|o| o.remaining_qty > 0.0)
            .map(sim_order_to_venue_fact)
            .collect())
    }

    async fn query_trades(&self, _req: VenueTradeQuery) -> anyhow::Result<Vec<VenueTradeFact>> {
        Ok(vec![])
    }

    async fn query_positions(
        &self,
        _req: VenuePositionQuery,
    ) -> anyhow::Result<Vec<VenuePositionFact>> {
        let state = self.state.lock().await;
        Ok(state
            .account_state
            .positions
            .values()
            .map(|entry| VenuePositionFact {
                instrument: entry.instrument_code.clone(),
                long_short_type: entry.long_short_type,
                qty: entry.total_qty,
                avail_qty: entry.avail_qty,
                frozen_qty: entry.frozen_qty,
                account_id: state.account_state.account_id,
                instrument_type: 0,
            })
            .collect())
    }

    async fn next_event(&self) -> anyhow::Result<VenueEvent> {
        let mut rx = self.event_rx.lock().await;
        rx.recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("event channel closed"))
    }
}
