use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};

use zk_proto_rs::zk::{
    exch_gw::v1::{
        order_report_entry::Report, ExchangeOrderStatus, OrderIdLinkageReport, OrderInfo,
        OrderReport, OrderReportEntry, OrderReportType, OrderStateReport, TradeReport,
    },
    gateway::v1::{
        CancelOrderRequest as ExchCancelOrderRequest, SendOrderRequest as ExchSendOrderRequest,
    },
    rtmd::v1::TickData,
};

use crate::{
    match_policy::MatchPolicy,
    models::{MatchResult, SimOrder, SimResult},
};

static SIM_COUNTER: AtomicI64 = AtomicI64::new(1);

fn next_id() -> String {
    SIM_COUNTER.fetch_add(1, Ordering::Relaxed).to_string()
}

/// Exchange simulator: maintains pending order state and drives `MatchPolicy`.
pub struct SimulatorCore {
    pub match_policy: Box<dyn MatchPolicy>,
    /// order_id → in-flight order.
    order_cache: HashMap<i64, SimOrder>,
    /// instrument_code → latest tick.
    orderbook_cache: HashMap<String, TickData>,
    pub exch_name: String,
}

impl SimulatorCore {
    pub fn new(match_policy: Box<dyn MatchPolicy>, exch_name: impl Into<String>) -> Self {
        Self {
            match_policy,
            order_cache: HashMap::new(),
            orderbook_cache: HashMap::new(),
            exch_name: exch_name.into(),
        }
    }

    pub fn on_new_order(&mut self, req: ExchSendOrderRequest, ts: i64) -> Vec<SimResult> {
        self.on_new_order_inner(req, ts, false)
    }

    /// Accept an order but only book it — skip matching even if the match
    /// policy would produce immediate fills. Used when matching is paused.
    pub fn on_new_order_book_only(&mut self, req: ExchSendOrderRequest, ts: i64) -> Vec<SimResult> {
        self.on_new_order_inner(req, ts, true)
    }

    fn on_new_order_inner(
        &mut self,
        req: ExchSendOrderRequest,
        ts: i64,
        book_only: bool,
    ) -> Vec<SimResult> {
        if !Self::validate(&req) {
            let report = make_rejected_report(req.correlation_id, next_id(), ts);
            return vec![SimResult {
                ts_ms: ts,
                order_report: report,
            }];
        }

        let exch_id = next_id();
        let remaining = req.scaled_qty;
        let mut sim_order = SimOrder {
            req,
            exch_order_id: exch_id,
            filled_qty: 0.0,
            remaining_qty: remaining,
            update_ts: ts,
        };

        let matches = if book_only {
            vec![]
        } else {
            let tick = self.orderbook_cache.get(&sim_order.req.instrument).cloned();
            self.match_policy.match_order(&sim_order, tick.as_ref())
        };

        let mut results: Vec<SimResult> = Vec::new();

        // Always emit linkage + booked first — the OMS expects the full
        // lifecycle (Pending → Booked → Filled/Cancelled).
        let booked = make_booked_report(&mut sim_order, ts);
        results.push(booked);

        for m in matches {
            let res = apply_match(&mut sim_order, m, true);
            results.push(res);
            if sim_order.remaining_qty <= 0.0 {
                break;
            }
        }

        self.order_cache
            .insert(sim_order.req.correlation_id, sim_order);
        results
    }

    /// Update the orderbook cache without running any matching.
    pub fn update_orderbook(&mut self, tick: &TickData) {
        self.orderbook_cache
            .insert(tick.instrument_code.clone(), tick.clone());
    }

    pub fn on_tick(&mut self, tick: &TickData) -> Vec<SimResult> {
        let sym = tick.instrument_code.clone();
        self.orderbook_cache.insert(sym.clone(), tick.clone());

        let active_ids: Vec<i64> = self
            .order_cache
            .values()
            .filter(|o| o.req.instrument == sym && o.remaining_qty > 0.0)
            .map(|o| o.req.correlation_id)
            .collect();

        let mut results = Vec::new();
        let tick_ref = self.orderbook_cache.get(&sym).unwrap();
        for id in &active_ids {
            let order = self.order_cache.get_mut(id).unwrap();
            let matches = self.match_policy.match_order(order, Some(tick_ref));
            for m in matches {
                let res = apply_match(order, m, false);
                results.push(res);
                if order.remaining_qty <= 0.0 {
                    break;
                }
            }
        }
        results
    }

    pub fn on_cancel(&mut self, req: &ExchCancelOrderRequest, ts: i64) -> Vec<SimResult> {
        let order_id = req.order_id;
        if let Some(order) = self.order_cache.get_mut(&order_id) {
            if order.remaining_qty > 0.0 {
                order.remaining_qty = 0.0;
                order.update_ts = ts + 1;
                let report = make_cancelled_report(order, ts + 1);
                return vec![SimResult {
                    ts_ms: ts + 1,
                    order_report: report,
                }];
            }
        }
        vec![]
    }

    /// Force-fill an open order deterministically without waiting for market data.
    /// Used by the admin `ForceMatch` RPC.
    ///
    /// `fill_qty`: if `None`, fills the entire remaining quantity.
    /// `fill_price`: if `None`, uses the order's limit price.
    pub fn force_match_order(
        &mut self,
        order_id: i64,
        fill_qty: Option<f64>,
        fill_price: Option<f64>,
        ts: i64,
    ) -> Result<Vec<SimResult>, String> {
        let order = self
            .order_cache
            .get_mut(&order_id)
            .ok_or_else(|| format!("order {order_id} not found"))?;

        if order.remaining_qty <= 0.0 {
            return Err(format!("order {order_id} has no remaining quantity"));
        }

        let qty = fill_qty.unwrap_or(order.remaining_qty);
        if qty <= 0.0 || qty > order.remaining_qty {
            return Err(format!(
                "invalid fill_qty {qty}: remaining is {}",
                order.remaining_qty
            ));
        }

        let price = fill_price.unwrap_or(order.req.scaled_price);

        let m = MatchResult {
            trigger_ts: ts,
            qty,
            price,
            delay_ms: 0,
            ioc_cancel: false,
        };

        let is_first = order.filled_qty == 0.0;
        let res = apply_match(order, m, is_first);
        Ok(vec![res])
    }

    /// Read-only access to the order cache for admin queries.
    pub fn order_cache(&self) -> &HashMap<i64, SimOrder> {
        &self.order_cache
    }

    /// Clear all orders (used by ResetSimulator).
    pub fn clear_orders(&mut self) {
        self.order_cache.clear();
        self.orderbook_cache.clear();
    }

    fn validate(req: &ExchSendOrderRequest) -> bool {
        req.scaled_qty > 0.0 && req.scaled_price >= 0.0
    }
}

// --- report builders ---

fn make_rejected_report(order_id: i64, exch_id: String, ts: i64) -> OrderReport {
    let exec_report = zk_proto_rs::zk::exch_gw::v1::ExecReport {
        exec_type: zk_proto_rs::zk::exch_gw::v1::ExchExecType::Rejected as i32,
        ..Default::default()
    };
    let entry = OrderReportEntry {
        report_type: OrderReportType::OrderRepTypeExec as i32,
        report: Some(Report::ExecReport(exec_report)),
    };
    OrderReport {
        order_id,
        exch_order_ref: exch_id,
        update_timestamp: ts,
        order_report_entries: vec![entry],
        ..Default::default()
    }
}

fn make_booked_report(order: &mut SimOrder, ts: i64) -> SimResult {
    let linkage_entry = OrderReportEntry {
        report_type: OrderReportType::OrderRepTypeLinkage as i32,
        report: Some(Report::OrderIdLinkageReport(OrderIdLinkageReport {
            order_id: order.req.correlation_id,
            exch_order_ref: order.exch_order_id.clone(),
        })),
    };
    let state_entry = make_state_entry(order);
    let report = OrderReport {
        order_id: order.req.correlation_id,
        exch_order_ref: order.exch_order_id.clone(),
        update_timestamp: ts,
        order_report_entries: vec![linkage_entry, state_entry],
        ..Default::default()
    };
    SimResult {
        ts_ms: ts,
        order_report: report,
    }
}

fn make_cancelled_report(order: &SimOrder, ts: i64) -> OrderReport {
    OrderReport {
        order_id: order.req.correlation_id,
        exch_order_ref: order.exch_order_id.clone(),
        update_timestamp: ts,
        order_report_entries: vec![make_state_entry_cancelled(order)],
        ..Default::default()
    }
}

fn apply_match(order: &mut SimOrder, m: MatchResult, add_linkage: bool) -> SimResult {
    order.filled_qty += m.qty;
    order.remaining_qty -= m.qty;
    let ts = (m.trigger_ts + m.delay_ms).max(order.update_ts + 1);
    order.update_ts = ts;

    let mut entries: Vec<OrderReportEntry> = Vec::new();

    if add_linkage {
        entries.push(OrderReportEntry {
            report_type: OrderReportType::OrderRepTypeLinkage as i32,
            report: Some(Report::OrderIdLinkageReport(OrderIdLinkageReport {
                order_id: order.req.correlation_id,
                exch_order_ref: order.exch_order_id.clone(),
            })),
        });
    }

    // Trade entry
    entries.push(OrderReportEntry {
        report_type: OrderReportType::OrderRepTypeTrade as i32,
        report: Some(Report::TradeReport(TradeReport {
            exch_trade_id: next_id(),
            filled_qty: m.qty,
            filled_price: m.price,
            filled_ts: ts,
            order_info: Some(OrderInfo {
                instrument: order.req.instrument.clone(),
                buy_sell_type: order.req.buysell_type,
                ..Default::default()
            }),
            ..Default::default()
        })),
    });

    // State entry
    entries.push(make_state_entry(order));

    let report = OrderReport {
        order_id: order.req.correlation_id,
        exch_order_ref: order.exch_order_id.clone(),
        update_timestamp: ts,
        order_report_entries: entries,
        ..Default::default()
    };
    SimResult {
        ts_ms: ts,
        order_report: report,
    }
}

fn make_state_entry(order: &SimOrder) -> OrderReportEntry {
    let status = if order.remaining_qty <= 0.0 {
        ExchangeOrderStatus::ExchOrderStatusFilled
    } else {
        ExchangeOrderStatus::ExchOrderStatusBooked
    };
    make_state_entry_with_status(order, status)
}

fn make_state_entry_cancelled(order: &SimOrder) -> OrderReportEntry {
    make_state_entry_with_status(order, ExchangeOrderStatus::ExchOrderStatusCancelled)
}

fn make_state_entry_with_status(order: &SimOrder, status: ExchangeOrderStatus) -> OrderReportEntry {
    OrderReportEntry {
        report_type: OrderReportType::OrderRepTypeState as i32,
        report: Some(Report::OrderStateReport(OrderStateReport {
            exch_order_status: status as i32,
            filled_qty: order.filled_qty,
            unfilled_qty: order.remaining_qty,
            ..Default::default()
        })),
    }
}
