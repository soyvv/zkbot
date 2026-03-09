use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};

use zk_proto_rs::{
    exch_gw::{
        order_report_entry::Report, ExchangeOrderStatus, OrderIdLinkageReport, OrderInfo,
        OrderReport, OrderReportEntry, OrderReportType, OrderStateReport, TradeReport,
    },
    rtmd::TickData,
    tqrpc_exch_gw::{ExchCancelOrderRequest, ExchSendOrderRequest},
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
        if !Self::validate(&req) {
            let report = make_rejected_report(req.correlation_id, next_id(), ts);
            return vec![SimResult { ts_ms: ts, order_report: report }];
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

        let tick = self.orderbook_cache.get(&sim_order.req.instrument).cloned();
        let matches = self.match_policy.match_order(&sim_order, tick.as_ref());

        let mut results: Vec<SimResult> = Vec::new();

        if matches.is_empty() {
            // No immediate fill — emit linkage + booked.
            let res = make_booked_report(&mut sim_order, ts);
            results.push(res);
        } else {
            for m in matches {
                let res = apply_match(&mut sim_order, m, true);
                results.push(res);
                if sim_order.remaining_qty <= 0.0 {
                    break;
                }
            }
        }

        self.order_cache.insert(sim_order.req.correlation_id, sim_order);
        results
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
        // Borrow from cache once — `orderbook_cache`, `order_cache`, and `match_policy`
        // are distinct struct fields, so NLL permits simultaneous borrows.
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
                return vec![SimResult { ts_ms: ts + 1, order_report: report }];
            }
        }
        vec![]
    }

    fn validate(req: &ExchSendOrderRequest) -> bool {
        req.scaled_qty > 0.0 && req.scaled_price >= 0.0
    }
}

// --- report builders ---

fn make_rejected_report(order_id: i64, exch_id: String, ts: i64) -> OrderReport {
    let exec_report = zk_proto_rs::exch_gw::ExecReport {
        exec_type: zk_proto_rs::exch_gw::ExchExecType::Rejected as i32,
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
    SimResult { ts_ms: ts, order_report: report }
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
    SimResult { ts_ms: ts, order_report: report }
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
