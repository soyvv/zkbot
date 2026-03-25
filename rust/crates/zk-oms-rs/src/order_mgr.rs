use std::collections::{HashMap, VecDeque};

use tracing::{error, warn};
use zk_proto_rs::zk::{
    common::v1::{InstrumentRefData, Rejection},
    exch_gw::v1::{ExchExecType, ExchangeOrderStatus, OrderReport, OrderReportType},
    gateway::v1::SendOrderRequest as ExchSendOrderRequest,
    oms::v1::{
        ExecMessage, ExecType, Fee, Order, OrderRequest, OrderStatus, OrderUpdateEvent, Trade,
    },
};

use crate::{
    models::{OmsOrder, OrderContext},
    utils::{gen_order_id, gen_timestamp_ms, round_to_precision},
};

/// Manages the in-memory order book.
/// Mirrors Python `OrderManager`.
pub struct OrderManager {
    /// Instruments are looked up by (gw_key → exch_symbol → refdata).
    refdata_lookup: HashMap<String, HashMap<String, InstrumentRefData>>,
    use_time_emulation: bool,
    /// order_id → OmsOrder
    pub(crate) order_dict: HashMap<i64, OmsOrder>,
    /// IDs of orders that are not yet in a terminal state.
    pub(crate) open_order_ids: std::collections::HashSet<i64>,
    /// LRU eviction queue for closed orders.
    order_id_queue: VecDeque<i64>,
    max_cached_orders: usize,
    /// order_id → OrderContext (resolved once per order, re-used for reports and cancels)
    pub context_cache: HashMap<i64, OrderContext>,
    /// gw_key → (exch_order_ref → order_id)
    exch_ref_to_order_id: HashMap<String, HashMap<String, i64>>,
    /// gw_key → (exch_order_ref → dummy_order_id) for external (non-OMS) orders
    external_order_dict: HashMap<String, HashMap<String, i64>>,
}

impl OrderManager {
    pub fn new(
        refdata_lookup: HashMap<String, HashMap<String, InstrumentRefData>>,
        use_time_emulation: bool,
        max_cached_orders: usize,
    ) -> Self {
        crate::utils::init_id_gen();
        Self {
            refdata_lookup,
            use_time_emulation,
            order_dict: HashMap::new(),
            open_order_ids: Default::default(),
            order_id_queue: VecDeque::new(),
            max_cached_orders,
            context_cache: HashMap::new(),
            exch_ref_to_order_id: HashMap::new(),
            external_order_dict: HashMap::new(),
        }
    }

    pub fn reload_refdata_lookup(
        &mut self,
        refdata_lookup: HashMap<String, HashMap<String, InstrumentRefData>>,
    ) {
        self.refdata_lookup = refdata_lookup;
    }

    // ------------------------------------------------------------------
    // Initialisation
    // ------------------------------------------------------------------

    pub fn init_with_orders(&mut self, orders: Vec<OmsOrder>) {
        for order in orders {
            if order.order_state.order_id == 0 {
                error!("invalid order missing order_id; skipping");
                continue;
            }
            let oid = order.order_id;
            if !order.is_in_terminal_state() {
                if order.order_state.filled_qty != order.order_state.qty {
                    self.open_order_ids.insert(oid);
                }
            }
            if !order.order_state.exch_order_ref.is_empty() {
                let gw_key = order.order_state.gw_key.clone();
                let exch_ref = order.order_state.exch_order_ref.clone();
                self.exch_ref_to_order_id
                    .entry(gw_key)
                    .or_default()
                    .insert(exch_ref, oid);
            }
            self.order_dict.insert(oid, order);
        }
    }

    // ------------------------------------------------------------------
    // Order creation
    // ------------------------------------------------------------------

    pub fn create_order(
        &mut self,
        order_req: &OrderRequest,
        ctx: Option<&OrderContext>,
    ) -> OmsOrder {
        let gw_req = ctx.map(|c| self.build_gw_order_req(order_req, c));

        let ts = if self.use_time_emulation {
            order_req.timestamp
        } else {
            gen_timestamp_ms()
        };

        let mut order_state = Order::default();
        order_state.snapshot_version = 1;
        order_state.order_id = order_req.order_id;
        order_state.account_id = order_req.account_id;
        order_state.gw_key = ctx
            .and_then(|c| c.route.as_ref())
            .map(|r| r.gw_key.clone())
            .unwrap_or_default();
        order_state.order_status = OrderStatus::Pending as i32;
        order_state.instrument = order_req.instrument_code.clone();
        order_state.instrument_exch = ctx
            .and_then(|c| c.symbol_ref.as_ref())
            .map(|r| r.instrument_id_exchange.clone())
            .unwrap_or_default();
        order_state.price = gw_req
            .as_ref()
            .map(|r| r.scaled_price)
            .unwrap_or(order_req.price);
        order_state.qty = gw_req
            .as_ref()
            .map(|r| r.scaled_qty)
            .unwrap_or(order_req.qty);
        order_state.filled_qty = 0.0;
        order_state.source_id = order_req.source_id.clone();
        order_state.buy_sell_type = order_req.buy_sell_type;
        order_state.open_close_type = order_req.open_close_type;
        order_state.created_at = ts;
        order_state.updated_at = ts;

        let oms_order = OmsOrder {
            is_from_external: false,
            order_id: order_req.order_id,
            account_id: order_req.account_id,
            exch_order_ref: None,
            oms_req: Some(order_req.clone()),
            gw_req,
            cancel_req: None,
            order_state,
            trades: Vec::new(),
            acc_trades_filled_qty: 0.0,
            acc_trades_value: 0.0,
            order_inferred_trades: Vec::new(),
            exec_msgs: Vec::new(),
            fees: Vec::new(),
            cancel_attempts: 0,
        };

        self.order_dict
            .insert(oms_order.order_id, oms_order.clone());
        self.open_order_ids.insert(oms_order.order_id);
        oms_order
    }

    fn build_gw_order_req(&self, req: &OrderRequest, ctx: &OrderContext) -> ExchSendOrderRequest {
        let ref_data = ctx
            .symbol_ref
            .as_ref()
            .expect("symbol_ref required for gw req");
        let route = ctx.route.as_ref().expect("route required for gw req");

        let scaled_qty = if ref_data.qty_precision > 0 {
            round_to_precision(req.qty, ref_data.qty_precision)
        } else {
            req.qty
        };
        let scaled_price = if ref_data.price_precision > 0 {
            round_to_precision(req.price, ref_data.price_precision)
        } else {
            req.price
        };

        let ts = if self.use_time_emulation {
            req.timestamp
        } else {
            gen_timestamp_ms()
        };

        let mut gw_req = ExchSendOrderRequest::default();
        gw_req.exch_account_id = route.exch_account_id.clone();
        gw_req.order_type = req.order_type;
        gw_req.buysell_type = req.buy_sell_type;
        gw_req.openclose_type = req.open_close_type;
        gw_req.correlation_id = req.order_id;
        gw_req.instrument = ref_data.instrument_id_exchange.clone();
        gw_req.scaled_qty = scaled_qty;
        gw_req.scaled_price = scaled_price;
        gw_req.timestamp = ts;
        gw_req.time_inforce_type = req.time_inforce_type;
        if let Some(extra) = &req.extra_properties {
            gw_req.exch_specific_params = Some(extra.clone());
        }
        gw_req
    }

    // ------------------------------------------------------------------
    // Lookup helpers
    // ------------------------------------------------------------------

    pub fn get_order_by_id(&self, order_id: i64) -> Option<&OmsOrder> {
        self.order_dict.get(&order_id)
    }

    pub fn get_order_by_id_mut(&mut self, order_id: i64) -> Option<&mut OmsOrder> {
        self.order_dict.get_mut(&order_id)
    }

    pub fn get_order(
        &self,
        order_id: i64,
        gw_key: &str,
        exch_order_ref: &str,
    ) -> Option<&OmsOrder> {
        if order_id != 0 {
            return self.order_dict.get(&order_id);
        }
        let oid = self
            .exch_ref_to_order_id
            .get(gw_key)
            .and_then(|m| m.get(exch_order_ref))
            .copied();
        oid.and_then(|id| self.order_dict.get(&id))
    }

    pub fn get_order_mut(
        &mut self,
        order_id: i64,
        gw_key: &str,
        exch_order_ref: &str,
    ) -> Option<&mut OmsOrder> {
        let resolved_id = if order_id != 0 {
            order_id
        } else {
            match self
                .exch_ref_to_order_id
                .get(gw_key)
                .and_then(|m| m.get(exch_order_ref))
                .copied()
            {
                Some(id) => id,
                None => return None,
            }
        };
        self.order_dict.get_mut(&resolved_id)
    }

    pub fn get_order_by_exch_ref(&self, gw_key: &str, exch_order_ref: &str) -> Option<&OmsOrder> {
        let oid = self
            .exch_ref_to_order_id
            .get(gw_key)
            .and_then(|m| m.get(exch_order_ref))
            .copied()?;
        self.order_dict.get(&oid)
    }

    pub fn get_open_orders_for_account(&self, account_id: i64) -> Vec<&OmsOrder> {
        self.open_order_ids
            .iter()
            .filter_map(|id| self.order_dict.get(id))
            .filter(|o| o.account_id == account_id)
            .collect()
    }

    pub fn is_marked_as_external(&self, gw_key: &str, exch_order_ref: &str) -> bool {
        self.external_order_dict
            .get(gw_key)
            .map(|m| m.contains_key(exch_order_ref))
            .unwrap_or(false)
    }

    pub fn get_external_order(&self, gw_key: &str, exch_order_ref: &str) -> Option<&OmsOrder> {
        let oid = self
            .external_order_dict
            .get(gw_key)
            .and_then(|m| m.get(exch_order_ref))
            .copied()?;
        self.order_dict.get(&oid)
    }

    pub fn get_external_order_mut(
        &mut self,
        gw_key: &str,
        exch_order_ref: &str,
    ) -> Option<&mut OmsOrder> {
        let oid = self
            .external_order_dict
            .get(gw_key)
            .and_then(|m| m.get(exch_order_ref))
            .copied()?;
        self.order_dict.get_mut(&oid)
    }

    // ------------------------------------------------------------------
    // State management helpers
    // ------------------------------------------------------------------

    pub fn remove_open_order(&mut self, order_id: i64) {
        self.open_order_ids.remove(&order_id);
        self.order_id_queue.push_back(order_id);
        if self.order_id_queue.len() > self.max_cached_orders {
            if let Some(oldest) = self.order_id_queue.pop_front() {
                self.cleanup_order(oldest);
            }
        }
    }

    fn cleanup_order(&mut self, order_id: i64) {
        if let Some(order) = self.order_dict.remove(&order_id) {
            let gw_key = &order.order_state.gw_key;
            let exch_ref = &order.order_state.exch_order_ref;
            if let Some(gw_map) = self.exch_ref_to_order_id.get_mut(gw_key) {
                gw_map.remove(exch_ref);
            }
        }
        self.context_cache.remove(&order_id);
    }

    // ------------------------------------------------------------------
    // Error helpers
    // ------------------------------------------------------------------

    pub fn update_with_oms_error(
        &mut self,
        oms_order: Option<OmsOrder>,
        order_req: &OrderRequest,
        error_msg: &str,
        rejection: Rejection,
    ) -> OrderUpdateEvent {
        let ts = if self.use_time_emulation {
            order_req.timestamp
        } else {
            gen_timestamp_ms()
        };

        let order = match oms_order {
            Some(o) => {
                self.order_dict.insert(o.order_id, o);
                self.order_dict.get_mut(&order_req.order_id).unwrap()
            }
            None => {
                // create a rejected stub
                let stub = self.create_order(order_req, None);
                self.order_dict.insert(stub.order_id, stub);
                self.order_dict.get_mut(&order_req.order_id).unwrap()
            }
        };

        apply_order_error(
            order,
            error_msg,
            ts,
            ExecType::PlacingOrder as i32,
            rejection,
        );
        if order.is_in_terminal_state() {
            let oid = order.order_id;
            self.open_order_ids.remove(&oid);
        }
        build_error_event(self.order_dict.get(&order_req.order_id).unwrap())
    }

    pub fn update_with_gw_error(
        &mut self,
        order_id: i64,
        ts: i64,
        error_msg: &str,
        exec_type: i32,
        rejection: Rejection,
    ) -> Option<OrderUpdateEvent> {
        let order = self.order_dict.get_mut(&order_id)?;
        apply_order_error(order, error_msg, ts, exec_type, rejection);
        let event = build_error_event(self.order_dict.get(&order_id).unwrap());
        Some(event)
    }

    // ------------------------------------------------------------------
    // Core report processing
    // ------------------------------------------------------------------

    /// Process a gateway `OrderReport` and return 0 or 1 `OrderUpdateEvent`.
    pub fn update_with_report(&mut self, report: &OrderReport) -> Vec<OrderUpdateEvent> {
        let order_id = if report.order_id != 0 {
            report.order_id
        } else {
            0
        };
        let exch_order_ref = &report.exch_order_ref;
        let gw_key = &report.exchange;
        let ts = report.update_timestamp;

        let resolved_order_id = if order_id != 0 {
            if self.order_dict.contains_key(&order_id) {
                order_id
            } else {
                error!(order_id, "order not found in update_with_report");
                return vec![];
            }
        } else {
            match self
                .exch_ref_to_order_id
                .get(gw_key)
                .and_then(|m| m.get(exch_order_ref))
                .copied()
            {
                Some(id) => id,
                None => {
                    error!(exch_order_ref, gw_key, "order not found by exch_order_ref");
                    return vec![];
                }
            }
        };

        // Compute any inferred state entry without cloning the full entries Vec.
        let extra_entry = self
            .order_dict
            .get(&resolved_order_id)
            .and_then(|order| infer_state_entry(&report.order_report_entries, order));

        let order = match self.order_dict.get_mut(&resolved_order_id) {
            Some(o) => o,
            None => return vec![],
        };

        order.order_state.updated_at = ts;
        if order.order_state.exch_order_ref.is_empty() {
            order.order_state.exch_order_ref = exch_order_ref.clone();
        }

        let mut original_order_updated = false;
        let mut new_trade = false;
        let mut new_oms_trade = false;
        let mut new_fee = false;
        let mut new_exec_msg = false;

        for entry in report.order_report_entries.iter().chain(extra_entry.iter()) {
            let report_type = OrderReportType::try_from(entry.report_type)
                .unwrap_or(OrderReportType::OrderRepTypeUnspecified);

            match report_type {
                OrderReportType::OrderRepTypeLinkage => {
                    if let Some(linkage) =
                        entry.report.as_ref().and_then(|r| match r {
                            zk_proto_rs::zk::exch_gw::v1::order_report_entry::Report::OrderIdLinkageReport(l) => Some(l),
                            _ => None,
                        })
                    {
                        order.exch_order_ref = Some(linkage.exch_order_ref.clone());
                        order.order_state.exch_order_ref = linkage.exch_order_ref.clone();
                        self.exch_ref_to_order_id
                            .entry(gw_key.clone())
                            .or_default()
                            .insert(linkage.exch_order_ref.clone(), resolved_order_id);

                        // if linkage is the only entry, promote PENDING → BOOKED
                        if report.order_report_entries.len() == 1 && extra_entry.is_none() {
                            let status = OrderStatus::try_from(order.order_state.order_status)
                                .unwrap_or(OrderStatus::Unspecified);
                            if matches!(
                                status,
                                OrderStatus::Pending | OrderStatus::Rejected
                            ) {
                                order.order_state.order_status =
                                    OrderStatus::Booked as i32;
                                original_order_updated = true;
                            }
                        }
                    }
                }

                OrderReportType::OrderRepTypeState => {
                    if let Some(state_report) =
                        entry.report.as_ref().and_then(|r| match r {
                            zk_proto_rs::zk::exch_gw::v1::order_report_entry::Report::OrderStateReport(s) => Some(s),
                            _ => None,
                        })
                    {
                        let orig_filled = order.order_state.filled_qty;
                        let orig_avg = order.order_state.filled_avg_price;

                        let new_filled = if state_report.filled_qty != 0.0 {
                            Some(state_report.filled_qty)
                        } else if state_report.unfilled_qty != 0.0 {
                            Some(order.order_state.qty - state_report.unfilled_qty)
                        } else {
                            None
                        };

                        if let Some(nf) = new_filled {
                            order.order_state.filled_qty = f64::max(nf, orig_filled);
                        }
                        if state_report.avg_price != 0.0 {
                            order.order_state.filled_avg_price = state_report.avg_price;
                        }

                        // synthesise an inferred trade for the fill increment
                        let fill_increment = order.order_state.filled_qty - orig_filled;
                        if fill_increment > 1e-8 {
                            let pseudo_id = format!(
                                "{}_{}",
                                resolved_order_id,
                                order.order_inferred_trades.len()
                            );
                            let inferred_price = infer_trade_price(
                                orig_filled,
                                orig_avg,
                                order.order_state.filled_qty,
                                order.order_state.filled_avg_price,
                            );
                            let inferred_trade = Trade {
                                order_id: order.order_state.order_id,
                                ext_order_ref: order.order_state.exch_order_ref.clone(),
                                ext_trade_id: pseudo_id,
                                filled_ts: ts,
                                filled_qty: fill_increment,
                                filled_price: inferred_price.unwrap_or(0.0),
                                ..Default::default()
                            };
                            order.order_inferred_trades.push(inferred_trade);
                            new_oms_trade = true;
                        }

                        let new_status = map_order_status(
                            ExchangeOrderStatus::try_from(state_report.exch_order_status)
                                .unwrap_or(ExchangeOrderStatus::ExchOrderStatusUnspecified),
                        );
                        if can_update_status(
                            OrderStatus::try_from(order.order_state.order_status)
                                .unwrap_or(OrderStatus::Unspecified),
                            new_status,
                        ) {
                            order.order_state.order_status = new_status as i32;
                        }
                        original_order_updated = true;
                    }
                }

                OrderReportType::OrderRepTypeTrade => {
                    if let Some(trade_report) = entry.report.as_ref().and_then(|r| match r {
                        zk_proto_rs::zk::exch_gw::v1::order_report_entry::Report::TradeReport(
                            t,
                        ) => Some(t),
                        _ => None,
                    }) {
                        let trade = Trade {
                            order_id: order.order_state.order_id,
                            ext_order_ref: order.order_state.exch_order_ref.clone(),
                            ext_trade_id: trade_report.exch_trade_id.clone(),
                            filled_qty: trade_report.filled_qty,
                            filled_price: trade_report.filled_price,
                            filled_ts: ts,
                            account_id: order.order_state.account_id,
                            buy_sell_type: order.order_state.buy_sell_type,
                            instrument: order.order_state.instrument.clone(),
                            fill_type: trade_report.fill_type,
                            exch_pnl: trade_report.exch_pnl,
                            ..Default::default()
                        };
                        order.acc_trades_filled_qty += trade.filled_qty;
                        order.acc_trades_value += trade.filled_qty * trade.filled_price;
                        order.trades.push(trade);
                        new_trade = true;
                        original_order_updated = true;
                    }
                }

                OrderReportType::OrderRepTypeExec => {
                    if let Some(exec_report) = entry.report.as_ref().and_then(|r| match r {
                        zk_proto_rs::zk::exch_gw::v1::order_report_entry::Report::ExecReport(e) => {
                            Some(e)
                        }
                        _ => None,
                    }) {
                        let exec_type = ExchExecType::try_from(exec_report.exec_type)
                            .unwrap_or(ExchExecType::Unspecified);
                        let oms_exec_type = match exec_type {
                            ExchExecType::Rejected => ExecType::PlacingOrder,
                            ExchExecType::CancelReject => ExecType::Cancel,
                            _ => ExecType::Unspecified,
                        };
                        if oms_exec_type != ExecType::Unspecified {
                            if let Some(err_info) = &exec_report.error_info {
                                let msg = format!(
                                    "err from gw: code={}, msg={}",
                                    err_info.error_code, err_info.error_message
                                );
                                apply_order_error(
                                    order,
                                    &msg,
                                    ts,
                                    oms_exec_type as i32,
                                    exec_report.rejection_info.clone().unwrap_or_default(),
                                );
                                new_exec_msg = true;
                                original_order_updated = true;
                            }
                        }
                    }
                }

                OrderReportType::OrderRepTypeFee => {
                    if let Some(fee_report) = entry.report.as_ref().and_then(|r| match r {
                        zk_proto_rs::zk::exch_gw::v1::order_report_entry::Report::FeeReport(f) => {
                            Some(f)
                        }
                        _ => None,
                    }) {
                        let fee = Fee {
                            order_id: order.order_state.order_id,
                            fee_amount: fee_report.fee_qty,
                            fee_symbol: fee_report.fee_symbol.clone(),
                            fee_ts: ts,
                            fee_type: fee_report.fee_type,
                        };
                        order.fees.push(fee);
                        new_fee = true;
                        original_order_updated = true;
                    }
                }

                _ => {
                    warn!("unknown order report type: {:?}", entry.report_type);
                }
            }
        }

        if original_order_updated {
            finalize_avg_price(order);
            order.order_state.snapshot_version += 1;

            let ts_event = if self.use_time_emulation {
                ts
            } else {
                gen_timestamp_ms()
            };
            let mut event = OrderUpdateEvent::default();
            event.order_id = resolved_order_id;
            event.account_id = order.order_state.account_id;
            event.timestamp = ts_event;
            event.order_snapshot = Some(order.order_state.clone());
            event.order_source_id = order.order_state.source_id.clone();
            // Propagate GW timestamps for latency tracking (t5 and t4).
            event.gw_report_timestamp_ns = report.update_timestamp * 1_000_000;
            event.gw_received_at_ns = report.gw_received_at_ns;
            if new_oms_trade {
                event.order_inferred_trade = order.order_inferred_trades.last().cloned();
            }
            if new_trade {
                event.last_trade = order.trades.last().cloned();
            }
            if new_exec_msg {
                event.exec_message = order.exec_msgs.last().cloned();
            }
            if new_fee {
                event.last_fee = order.fees.last().cloned();
            }

            if order.is_in_terminal_state() {
                self.remove_open_order(resolved_order_id);
            }

            return vec![event];
        }

        vec![]
    }

    // ------------------------------------------------------------------
    // External (non-OMS) order handling
    // ------------------------------------------------------------------

    pub fn handle_external_order_report(
        &mut self,
        report: &mut OrderReport,
    ) -> Option<OrderUpdateEvent> {
        let gw_key = report.exchange.clone();
        let exch_order_ref = report.exch_order_ref.clone();

        if !self.is_marked_as_external(&gw_key, &exch_order_ref) {
            let dummy_id = gen_order_id();
            let mut order_state = Order::default();
            order_state.order_id = dummy_id;
            order_state.exch_order_ref = exch_order_ref.clone();
            order_state.account_id = report.account_id;
            order_state.gw_key = gw_key.clone();
            order_state.order_status = OrderStatus::Pending as i32;
            order_state.created_at = report.update_timestamp;
            order_state.updated_at = report.update_timestamp;
            order_state.source_id = format!("External@{}", gw_key);

            let oms_order = OmsOrder {
                is_from_external: true,
                order_id: dummy_id,
                account_id: report.account_id,
                exch_order_ref: Some(exch_order_ref.clone()),
                oms_req: None,
                gw_req: None,
                cancel_req: None,
                order_state,
                trades: Vec::new(),
                acc_trades_filled_qty: 0.0,
                acc_trades_value: 0.0,
                order_inferred_trades: Vec::new(),
                exec_msgs: Vec::new(),
                fees: Vec::new(),
                cancel_attempts: 0,
            };

            self.external_order_dict
                .entry(gw_key.clone())
                .or_default()
                .insert(exch_order_ref.clone(), dummy_id);
            self.exch_ref_to_order_id
                .entry(gw_key.clone())
                .or_default()
                .insert(exch_order_ref.clone(), dummy_id);
            self.order_dict.insert(dummy_id, oms_order);
            self.open_order_ids.insert(dummy_id);
        }

        // override order_id in the report so update_with_report finds it
        let resolved_id = *self
            .external_order_dict
            .get(&gw_key)
            .and_then(|m| m.get(&exch_order_ref))
            .unwrap();
        report.order_id = resolved_id;

        let events = self.update_with_report(report);
        events.into_iter().next()
    }

    pub fn generate_order_snapshot(&self, order_id: i64, ts: i64) -> Option<OrderUpdateEvent> {
        let order = self.order_dict.get(&order_id)?;
        let mut event = OrderUpdateEvent::default();
        event.order_id = order_id;
        event.account_id = order.account_id;
        event.timestamp = ts;
        event.order_snapshot = Some(order.order_state.clone());
        event.order_source_id = order.order_state.source_id.clone();
        Some(event)
    }
}

// ---------------------------------------------------------------------------
// Pure functions (no &mut self) used by OrderManager
// ---------------------------------------------------------------------------

fn apply_order_error(
    order: &mut OmsOrder,
    error_msg: &str,
    ts: i64,
    exec_type: i32,
    rejection: Rejection,
) {
    let mut exec_msg = ExecMessage::default();
    exec_msg.order_id = order.order_id;
    exec_msg.exec_type = exec_type;
    exec_msg.exec_success = false;
    exec_msg.timestamp = ts;
    exec_msg.error_msg = error_msg.to_string();
    exec_msg.rejection_info = Some(rejection);
    order.exec_msgs.push(exec_msg);

    if exec_type == ExecType::PlacingOrder as i32 {
        order.order_state.snapshot_version += 1;
        order.order_state.order_status = OrderStatus::Rejected as i32;
        order.order_state.updated_at = ts;
        order.order_state.error_msg = error_msg.to_string();
    }
}

fn build_error_event(order: &OmsOrder) -> OrderUpdateEvent {
    let mut event = OrderUpdateEvent::default();
    event.order_id = order.order_id;
    event.account_id = order.account_id;
    event.timestamp = order.order_state.updated_at;
    event.order_snapshot = Some(order.order_state.clone());
    event.exec_message = order.exec_msgs.last().cloned();
    event.order_source_id = order.order_state.source_id.clone();
    event
}

pub fn map_order_status(gw_status: ExchangeOrderStatus) -> OrderStatus {
    match gw_status {
        ExchangeOrderStatus::ExchOrderStatusBooked => OrderStatus::Booked,
        ExchangeOrderStatus::ExchOrderStatusPartialFilled => OrderStatus::PartiallyFilled,
        ExchangeOrderStatus::ExchOrderStatusFilled => OrderStatus::Filled,
        ExchangeOrderStatus::ExchOrderStatusCancelled => OrderStatus::Cancelled,
        ExchangeOrderStatus::ExchOrderStatusExchRejected => OrderStatus::Rejected,
        ExchangeOrderStatus::ExchOrderStatusExpired => OrderStatus::Cancelled,
        _ => OrderStatus::Unspecified,
    }
}

fn can_update_status(old: OrderStatus, new: OrderStatus) -> bool {
    !matches!(old, OrderStatus::Cancelled | OrderStatus::Filled) && new != OrderStatus::Unspecified
}

fn infer_trade_price(orig_qty: f64, orig_avg: f64, new_qty: f64, new_avg: f64) -> Option<f64> {
    if new_qty == 0.0 {
        return None;
    }
    if orig_avg == 0.0 {
        return Some(new_avg);
    }
    if new_avg == 0.0 {
        return None;
    }
    let increment = new_qty - orig_qty;
    if increment == 0.0 {
        return None;
    }
    let orig_value = orig_avg * orig_qty;
    let new_value = new_avg * new_qty;
    Some((new_value - orig_value) / increment)
}

/// Infers a synthetic `OrderStateReport` entry from trade entries when no explicit state
/// is present. Returns `Some(entry)` if inference applies, `None` otherwise.
/// Borrows the entry slice rather than cloning it — caller chains the result if Some.
fn infer_state_entry(
    entries: &[zk_proto_rs::zk::exch_gw::v1::OrderReportEntry],
    order: &OmsOrder,
) -> Option<zk_proto_rs::zk::exch_gw::v1::OrderReportEntry> {
    if order.is_in_terminal_state() {
        return None;
    }
    let has_state = entries.iter().any(|e| {
        matches!(
            &e.report,
            Some(zk_proto_rs::zk::exch_gw::v1::order_report_entry::Report::OrderStateReport(_))
        )
    });
    if has_state {
        return None;
    }

    // Accumulate fill data from trade entries by borrowing (no clone).
    let mut new_qty = 0f64;
    let mut new_value = 0f64;
    let mut has_trade = false;
    for e in entries {
        if let Some(zk_proto_rs::zk::exch_gw::v1::order_report_entry::Report::TradeReport(t)) =
            &e.report
        {
            new_qty += t.filled_qty;
            new_value += t.filled_qty * t.filled_price;
            has_trade = true;
        }
    }
    if !has_trade {
        return None;
    }

    let total_qty = new_qty + order.acc_trades_filled_qty;
    if total_qty > order.order_state.qty {
        warn!(
            total_qty,
            order_qty = order.order_state.qty,
            "trades exceed order qty"
        );
        return None;
    }
    let avg_price = if total_qty != 0.0 {
        (order.acc_trades_value + new_value) / total_qty
    } else {
        0.0
    };

    let exch_status = if (total_qty - order.order_state.qty).abs() < 1e-8 {
        ExchangeOrderStatus::ExchOrderStatusFilled
    } else {
        ExchangeOrderStatus::ExchOrderStatusPartialFilled
    };

    use zk_proto_rs::zk::exch_gw::v1::{
        order_report_entry::Report, OrderReportEntry, OrderStateReport,
    };
    Some(OrderReportEntry {
        report_type: OrderReportType::OrderRepTypeState as i32,
        report: Some(Report::OrderStateReport(OrderStateReport {
            exch_order_status: exch_status as i32,
            filled_qty: total_qty,
            unfilled_qty: order.order_state.qty - total_qty,
            avg_price,
            ..Default::default()
        })),
    })
}

fn finalize_avg_price(order: &mut OmsOrder) {
    if order.trades.is_empty() {
        return;
    }
    if order.is_in_terminal_state()
        && order.order_state.filled_avg_price == 0.0
        && order.acc_trades_filled_qty != 0.0
    {
        order.order_state.filled_avg_price = order.acc_trades_value / order.acc_trades_filled_qty;
    }
}

// OrderContext is defined in crate::models and re-exported from crate::order_mgr
// to avoid a circular import.
