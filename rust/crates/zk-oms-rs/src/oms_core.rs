use std::collections::{HashMap, HashSet};

use tracing::{error, info, warn};
use zk_proto_rs::zk::{
    common::v1::{BuySellType, Rejection, RejectionReason, RejectionSource},
    exch_gw::v1::OrderReport,
    gateway::v1::{
        BatchCancelOrdersRequest as ExchBatchCancelOrdersRequest,
        BatchSendOrdersRequest as ExchBatchSendOrdersRequest,
    },
    oms::v1::{ExecType, OrderRequest, OrderStatus},
};

use crate::{
    balance_mgr::BalanceManager,
    config::ConfdataManager,
    models::{
        CancelRecheckRequest, OmsAction, OmsMessage, OmsOrder, OmsPosition, OrderContext,
        OrderRecheckRequest, PositionChange,
    },
    order_mgr::OrderManager,
    utils::gen_timestamp_ms,
};

/// The core OMS state machine.
///
/// Receives messages via `process_message` and returns a list of actions the
/// service layer must execute (send to gateway, persist to Redis, publish to NATS).
///
/// All mutable state lives here. This type is single-writer: only one task/thread
/// drives it. Readers should consume immutable snapshots or a Redis-backed read model.
pub struct OmsCore {
    config: ConfdataManager,
    pub order_mgr: OrderManager,
    pub balance_mgr: BalanceManager,
    use_time_emulation: bool,
    risk_check_enabled: bool,
    handle_external_orders: bool,
    /// Account IDs that are in panic mode (order placement blocked).
    panic_accounts: HashSet<i64>,
    /// gw_key → exch_order_ref → buffered reports that arrived before their TQ order.
    pending_order_reports: HashMap<String, HashMap<String, Vec<OrderReport>>>,
    total_retries_orders: u32,
    total_retries_cancels: u32,
}

impl OmsCore {
    pub fn new(
        config: ConfdataManager,
        use_time_emulation: bool,
        risk_check_enabled: bool,
        handle_external_orders: bool,
        max_cached_orders: usize,
    ) -> Self {
        let order_mgr = OrderManager::new(
            config.refdata_by_gw_key.clone(),
            use_time_emulation,
            max_cached_orders,
        );
        let balance_mgr = BalanceManager::new(
            config.refdata_by_account_id.clone(),
            &config.account_routes.values().cloned().collect::<Vec<_>>(),
            &config.gw_configs.values().cloned().collect::<Vec<_>>(),
        );
        Self {
            config,
            order_mgr,
            balance_mgr,
            use_time_emulation,
            risk_check_enabled,
            handle_external_orders,
            panic_accounts: HashSet::new(),
            pending_order_reports: HashMap::new(),
            total_retries_orders: 0,
            total_retries_cancels: 0,
        }
    }

    // ------------------------------------------------------------------
    // Read replica (live trading only — compiled when feature = "replica")
    // ------------------------------------------------------------------

    /// Build the initial read-replica snapshot from current `OmsCore` state and
    /// return an `OmsSnapshotWriter` primed with that state for subsequent
    /// incremental updates.
    ///
    /// Called **once** after warm-start / gateway reconciliation. The returned
    /// `OmsSnapshotWriter` is then owned by the writer task; subsequent mutations
    /// drive it via `apply_persist_order` / `apply_panic` etc., and each
    /// `writer.publish()` produces a new snapshot at O(1) cost.
    ///
    /// The initial build is O(n log n) for the order map — acceptable as a
    /// one-time startup cost.
    #[cfg(feature = "replica")]
    pub fn take_snapshot(
        &self,
    ) -> (crate::snapshot::OmsSnapshot, crate::snapshot::OmsSnapshotWriter) {
        use crate::snapshot::OmsSnapshotWriter;

        let mut writer = OmsSnapshotWriter::new();

        // Populate from current OrderManager state.
        for (id, order) in &self.order_mgr.order_dict {
            let set_closed = !self.order_mgr.open_order_ids.contains(id);
            writer.apply_persist_order(order, set_closed);
        }

        // Populate panic state.
        for &account_id in &self.panic_accounts {
            writer.apply_panic(account_id);
        }

        let snapshot = writer.publish(
            self.balance_mgr.snapshot_balances(),
            self.balance_mgr.snapshot_exch_balances(),
            crate::utils::gen_timestamp_ms(),
        );

        (snapshot, writer)
    }

    // ------------------------------------------------------------------
    // Initialisation
    // ------------------------------------------------------------------

    pub fn init_state(&mut self, orders: Vec<OmsOrder>, balances: Vec<OmsPosition>) {
        self.order_mgr.init_with_orders(orders);
        self.balance_mgr.init_balances(balances);
    }

    // ------------------------------------------------------------------
    // Config reload
    // ------------------------------------------------------------------

    pub fn reload_config(&mut self, new_config: ConfdataManager) {
        self.config = new_config;
        self.order_mgr.reload_refdata_lookup(self.config.refdata_by_gw_key.clone());
        self.balance_mgr.reload_symbol_map(self.config.refdata_by_account_id.clone());
        self.balance_mgr.reload_account_config(
            &self.config.account_routes.values().cloned().collect::<Vec<_>>(),
            &self.config.gw_configs.values().cloned().collect::<Vec<_>>(),
        );
    }

    // ------------------------------------------------------------------
    // Main entry point
    // ------------------------------------------------------------------

    pub fn process_message(&mut self, msg: OmsMessage) -> Vec<OmsAction> {
        match msg {
            OmsMessage::PlaceOrder(req) => self.process_order(req),
            OmsMessage::BatchPlaceOrders(reqs) => self.batch_process_orders(reqs),
            OmsMessage::CancelOrder(req) => self.process_cancel(req),
            OmsMessage::BatchCancelOrders(reqs) => self.batch_process_cancels(reqs),
            OmsMessage::GatewayOrderReport(report) => self.process_order_report(report),
            OmsMessage::BalanceUpdate(update) => self.process_balance_update(update),
            OmsMessage::RecheckOrder(req) => self.process_recheck_order(req),
            OmsMessage::RecheckCancel(req) => self.process_recheck_cancel(req),
            OmsMessage::Panic { account_id } => {
                self.panic_accounts.insert(account_id);
                info!(account_id, "panic activated");
                vec![]
            }
            OmsMessage::DontPanic { account_id } => {
                self.panic_accounts.remove(&account_id);
                info!(account_id, "panic cleared");
                vec![]
            }
            OmsMessage::Cleanup { ts_ms } => self.process_cleanup(ts_ms),
            OmsMessage::ReloadConfig => {
                // Caller is responsible for providing the new config to reload_config().
                // This variant just signals a reload was requested.
                vec![]
            }
        }
    }

    // ------------------------------------------------------------------
    // Order placement
    // ------------------------------------------------------------------

    fn process_order(&mut self, req: OrderRequest) -> Vec<OmsAction> {
        match self.process_order_inner(req.clone()) {
            Ok(actions) => actions,
            Err(e) => {
                error!(order_id = req.order_id, error = %e, "unexpected error in process_order");
                let rejection = Rejection {
                    reason: RejectionReason::RejReasonOmsInternalError as i32,
                    recoverable: false,
                    source: RejectionSource::OmsReject as i32,
                    error_message: e.to_string(),
                    ..Default::default()
                };
                let update_event = self.order_mgr.update_with_oms_error(None, &req, &e, rejection);
                vec![OmsAction::PublishOrderUpdate(Box::new(update_event))]
            }
        }
    }

    fn process_order_inner(&mut self, req: OrderRequest) -> Result<Vec<OmsAction>, String> {
        let ts = if self.use_time_emulation { req.timestamp } else { gen_timestamp_ms() };
        info!(order_id = req.order_id, "processing order request");

        // Panic check
        if self.panic_accounts.contains(&req.account_id) {
            info!(account_id = req.account_id, "order rejected: account in panic");
            return Ok(vec![]);
        }

        // Resolve context
        let mut ctx = self.resolve_context(&req.instrument_code, req.account_id);
        if ctx.symbol_ref.is_none() {
            ctx.errors.push("Instrument not found".into());
        } else if ctx.gw_config.is_none() {
            ctx.errors.push("Exchange gw not found".into());
        }

        // Risk check
        if self.risk_check_enabled {
            let tc = ctx.trading_config.as_ref();
            if let Some(max_size) = tc.and_then(|t| t.max_order_size) {
                if req.qty > max_size {
                    ctx.errors.push(format!("order qty {} exceeds max {}", req.qty, max_size));
                }
            }
        }

        // Balance bookkeeping (pre-trade)
        let mut fund_change: Option<PositionChange> = None;
        let mut pos_change: Option<PositionChange> = None;

        let bookkeeping = ctx.trading_config.as_ref().map(|t| t.bookkeeping_balance).unwrap_or(false);
        if !ctx.has_error() && bookkeeping {
            let tc = ctx.trading_config.as_ref().unwrap();
            let fund_sym = ctx.fund_symbol.clone().unwrap_or_default();
            let pos_sym = ctx.pos_symbol.clone().unwrap_or_default();

            let mut fc = self.balance_mgr.create_balance_change(req.account_id, &fund_sym, false);
            let mut pc = self.balance_mgr.create_balance_change(req.account_id, &pos_sym, false);

            if !tc.use_margin {
                match BuySellType::try_from(req.buy_sell_type) {
                    Ok(BuySellType::BsBuy) => {
                        fc.avail_change = -(req.qty * req.price);
                        fc.frozen_change = -fc.avail_change;
                    }
                    Ok(BuySellType::BsSell) => {
                        pc.avail_change = -req.qty;
                        pc.frozen_change = -pc.avail_change;
                    }
                    _ => return Err("unsupported buy_sell_type".into()),
                }
            }

            let balance_check = tc.balance_check;
            if balance_check {
                self.balance_mgr
                    .check_changes(&[fc.clone(), pc.clone()])
                    .map_err(|e| e.to_string())
                    .map_err(|e| {
                        ctx.errors.push(e.clone());
                        e
                    })
                    .ok();
            }

            fund_change = Some(fc);
            pos_change = Some(pc);
        }

        // Create internal order record — skip building gw_req when context has errors
        // (build_gw_order_req requires symbol_ref which may be None on error paths)
        let order_ctx = if ctx.has_error() { None } else { Some(&ctx) };
        let oms_order = self.order_mgr.create_order(&req, order_ctx);
        self.order_mgr.context_cache.insert(req.order_id, ctx.clone());

        let mut actions = Vec::new();

        if ctx.has_error() {
            let rejection = Rejection {
                reason: RejectionReason::RejReasonOmsInvalidState as i32,
                recoverable: false,
                source: RejectionSource::OmsReject as i32,
                ..Default::default()
            };
            let update_event = self.order_mgr.update_with_oms_error(
                Some(oms_order.clone()),
                &req,
                &ctx.errors.join("; "),
                rejection,
            );
            actions.push(OmsAction::PublishOrderUpdate(Box::new(update_event)));
        } else {
            // Send to gateway
            let gw_req = oms_order.gw_req.clone().expect("gw_req always set on non-error order");
            let gw_key = ctx.gw_config.as_ref().map(|g| g.gw_key.clone()).unwrap_or_default();
            actions.push(OmsAction::SendOrderToGw {
                gw_key,
                request: gw_req,
                order_id: oms_order.order_id,
                order_created_at: oms_order.order_state.created_at,
            });

            // Apply balance changes if bookkeeping is on
            if let (Some(fc), Some(pc)) = (fund_change, pos_change) {
                let updated_symbols = self.balance_mgr.apply_changes(&[fc, pc], ts);
                let tc = ctx.trading_config.as_ref().unwrap();
                if tc.publish_balance_on_book {
                    let pue = self.balance_mgr.build_position_snapshot_for_symbols(
                        req.account_id, &updated_symbols, false, ts,
                    );
                    actions.push(OmsAction::PublishBalanceUpdate(Box::new(pue)));
                }
            }
        }

        // Persist order
        let terminal = self.order_mgr.get_order_by_id(req.order_id)
            .map(|o| o.is_in_terminal_state())
            .unwrap_or(false);
        actions.push(OmsAction::PersistOrder {
            order: Box::new(oms_order),
            set_expire: terminal,
            set_closed: terminal,
        });

        Ok(actions)
    }

    fn batch_process_orders(&mut self, reqs: Vec<OrderRequest>) -> Vec<OmsAction> {
        info!(n = reqs.len(), "batch processing orders");
        let mut gw_orders: HashMap<String, Vec<OmsAction>> = HashMap::new();
        let mut balance_actions: Vec<OmsAction> = Vec::new();
        let mut other_actions: Vec<OmsAction> = Vec::new();

        for req in reqs {
            for action in self.process_order(req) {
                match &action {
                    OmsAction::SendOrderToGw { gw_key, .. } => {
                        gw_orders.entry(gw_key.clone()).or_default().push(action);
                    }
                    OmsAction::PublishBalanceUpdate(_) => balance_actions.push(action),
                    _ => other_actions.push(action),
                }
            }
        }

        // Batch per exchange if supported
        for (gw_key, oa_list) in gw_orders {
            let supports_batch = self.config.gw_configs.get(&gw_key)
                .map(|g| g.support_batch_order)
                .unwrap_or(false);
            if oa_list.len() > 1 && supports_batch {
                let requests: Vec<_> = oa_list
                    .into_iter()
                    .filter_map(|a| match a {
                        OmsAction::SendOrderToGw { request, .. } => Some(request),
                        _ => None,
                    })
                    .collect();
                other_actions.push(OmsAction::BatchSendOrdersToGw {
                    gw_key,
                    request: ExchBatchSendOrdersRequest { order_requests: requests },
                });
            } else {
                other_actions.extend(oa_list);
            }
        }

        // Deduplicate balance updates (keep last per account+symbol)
        let mut last_by_account: HashMap<i64, zk_proto_rs::zk::oms::v1::PositionUpdateEvent> = HashMap::new();
        for action in balance_actions {
            if let OmsAction::PublishBalanceUpdate(pue) = action {
                let account_id = pue.account_id;
                last_by_account.insert(account_id, *pue);
            }
        }
        for pue in last_by_account.into_values() {
            other_actions.push(OmsAction::PublishBalanceUpdate(Box::new(pue)));
        }

        other_actions
    }

    // ------------------------------------------------------------------
    // Cancel
    // ------------------------------------------------------------------

    fn process_cancel(&mut self, req: zk_proto_rs::zk::oms::v1::OrderCancelRequest) -> Vec<OmsAction> {
        let order_id = req.order_id;
        info!(order_id, "processing cancel request");

        // Reconstruct context if missing
        if !self.order_mgr.context_cache.contains_key(&order_id) {
            if let Some(order) = self.order_mgr.get_order_by_id(order_id) {
                let instrument = order.order_state.instrument.clone();
                let account_id = order.account_id;
                let ctx = self.resolve_context(&instrument, account_id);
                self.order_mgr.context_cache.insert(order_id, ctx);
            } else {
                error!(order_id, "cancel for unknown order");
                return vec![];
            }
        }

        let ctx = match self.order_mgr.context_cache.get(&order_id).cloned() {
            Some(c) => c,
            None => {
                error!(order_id, "could not reconstruct context for cancel");
                return vec![];
            }
        };

        // Increment attempt counter
        if let Some(order) = self.order_mgr.get_order_by_id_mut(order_id) {
            order.cancel_attempts += 1;
        }

        let order = match self.order_mgr.get_order_by_id(order_id) {
            Some(o) => o,
            None => {
                error!(order_id, "order not found for cancel");
                return vec![];
            }
        };

        // Check state
        let mut rejection: Option<Rejection> = None;
        if order.is_in_terminal_state() {
            rejection = Some(Rejection {
                reason: RejectionReason::RejReasonOmsInvalidState as i32,
                source: RejectionSource::OmsReject as i32,
                recoverable: true,
                error_message: "order already in terminal state".into(),
                ..Default::default()
            });
        }
        let exch_order_ref = order.order_state.exch_order_ref.clone();
        if exch_order_ref.is_empty() {
            rejection = Some(Rejection {
                reason: RejectionReason::RejReasonOmsInvalidState as i32,
                source: RejectionSource::OmsReject as i32,
                recoverable: true,
                ..Default::default()
            });
        }

        if let Some(rej) = rejection {
            let msg = rej.error_message.clone();
            if let Some(oue) = self.order_mgr.update_with_gw_error(
                order_id, req.timestamp, &msg, ExecType::Cancel as i32, rej,
            ) {
                return vec![OmsAction::PublishOrderUpdate(Box::new(oue))];
            }
            return vec![];
        }

        // Build extra info required by exchange
        let mut extra = zk_proto_rs::zk::common::v1::ExtraData::default();
        if let Some(gw_cfg) = &ctx.gw_config {
            for field in &gw_cfg.cancel_required_fields {
                let order = self.order_mgr.get_order_by_id(order_id).unwrap();
                let val = match field.as_str() {
                    "order_id" => order.order_id.to_string(),
                    "exch_order_ref" => order.order_state.exch_order_ref.clone(),
                    "instrument" => order.order_state.instrument.clone(),
                    other => {
                        warn!(field = other, "unknown cancel_required_field");
                        String::new()
                    }
                };
                extra.data_map.insert(field.clone(), val);
            }
        }

        let gw_cancel = zk_proto_rs::zk::gateway::v1::CancelOrderRequest {
            exch_order_ref: exch_order_ref.clone(),
            order_id,
            exch_specific_params: Some(extra),
            timestamp: req.timestamp,
            ..Default::default()
        };

        let gw_key = ctx.route.map(|r| r.gw_key).unwrap_or_default();
        vec![OmsAction::SendCancelToGw { gw_key, request: gw_cancel }]
    }

    fn batch_process_cancels(&mut self, reqs: Vec<zk_proto_rs::zk::oms::v1::OrderCancelRequest>) -> Vec<OmsAction> {
        info!(n = reqs.len(), "batch processing cancels");
        let mut cancel_by_gw: HashMap<String, Vec<OmsAction>> = HashMap::new();
        let mut other_actions: Vec<OmsAction> = Vec::new();

        for req in reqs {
            for action in self.process_cancel(req) {
                match &action {
                    OmsAction::SendCancelToGw { gw_key, .. } => {
                        cancel_by_gw.entry(gw_key.clone()).or_default().push(action);
                    }
                    _ => other_actions.push(action),
                }
            }
        }

        for (gw_key, ca_list) in cancel_by_gw {
            let supports_batch = self.config.gw_configs.get(&gw_key)
                .map(|g| g.support_batch_cancel)
                .unwrap_or(false);
            if ca_list.len() > 1 && supports_batch {
                let requests: Vec<_> = ca_list
                    .into_iter()
                    .filter_map(|a| match a {
                        OmsAction::SendCancelToGw { request, .. } => Some(request),
                        _ => None,
                    })
                    .collect();
                other_actions.push(OmsAction::BatchCancelToGw {
                    gw_key,
                    request: ExchBatchCancelOrdersRequest { cancel_requests: requests },
                });
            } else {
                other_actions.extend(ca_list);
            }
        }

        other_actions
    }

    // ------------------------------------------------------------------
    // Gateway order report
    // ------------------------------------------------------------------

    fn process_order_report(&mut self, mut report: OrderReport) -> Vec<OmsAction> {
        let ts = report.update_timestamp;
        let gw_key = report.exchange.clone();
        let exch_order_ref = report.exch_order_ref.clone();
        info!(exch_order_ref, gw_key, "processing order report");

        // Handle external / non-TQ orders
        if self.handle_external_orders {
            use zk_proto_rs::zk::exch_gw::v1::OrderSourceType;
            let account_id = if report.account_id != 0 {
                report.account_id
            } else {
                // Guess account_id if there's only one account on this gw
                let ids = self.config.gw_key_to_account_ids.get(&gw_key);
                if let Some(set) = ids {
                    if set.len() == 1 {
                        *set.iter().next().unwrap()
                    } else {
                        error!(gw_key, "cannot determine account_id for non-TQ report; discarding");
                        return vec![];
                    }
                } else {
                    error!(gw_key, "no account mapping for gw_key; discarding");
                    return vec![];
                }
            };
            report.account_id = account_id;

            let src_type = OrderSourceType::try_from(report.order_source_type)
                .unwrap_or(OrderSourceType::OrderSourceUnspecified);
            let is_external = src_type == OrderSourceType::OrderSourceNonTq;
            let is_known_external = src_type == OrderSourceType::OrderSourceUnknown
                && self.order_mgr.is_marked_as_external(&gw_key, &exch_order_ref);

            if is_external || is_known_external {
                let mut actions = Vec::new();
                let oue = self.order_mgr.handle_external_order_report(&mut report);

                // Replay any pending reports that were buffered for this exch_order_ref
                let pending = self.pop_pending_reports(&gw_key, &exch_order_ref);

                let mut events = Vec::new();
                if let Some(e) = oue {
                    events.push(e);
                }
                for mut pending_report in pending {
                    if let Some(e) = self.order_mgr.handle_external_order_report(&mut pending_report) {
                        events.push(e);
                    }
                }

                for event in events {
                    let order_id = event.order_id;
                    let terminal = self.order_mgr.get_order_by_id(order_id)
                        .map(|o| o.is_in_terminal_state())
                        .unwrap_or(false);
                    if let Some(order) = self.order_mgr.get_order_by_id(order_id).cloned() {
                        actions.push(OmsAction::PersistOrder {
                            order: Box::new(order),
                            set_expire: terminal,
                            set_closed: terminal,
                        });
                    }
                    actions.push(OmsAction::PublishOrderUpdate(Box::new(event)));
                }
                return actions;
            }
        }

        // Normal (TQ-originated) order report
        let order_id = report.order_id;
        let ctx = if order_id != 0 {
            self.order_mgr.context_cache.get(&order_id).cloned()
                .or_else(|| self.resolve_context_for_order(order_id))
        } else {
            let order = self.order_mgr.get_order_by_exch_ref(&gw_key, &exch_order_ref);
            match order {
                Some(o) => self.resolve_context_for_order(o.order_id),
                None => {
                    warn!(exch_order_ref, gw_key, "unknown order; buffering report");
                    self.add_to_pending_reports(gw_key, report);
                    return vec![];
                }
            }
        };

        let mut actions = Vec::new();

        // Balance update on trade (for bookkeeping instruments)
        if let Some(ctx) = &ctx {
            let bookkeeping = ctx.trading_config.as_ref().map(|t| t.bookkeeping_balance).unwrap_or(false);
            if bookkeeping {
                if let Some(balance_changes) = self.calc_balance_changes_for_report(ctx, &report) {
                    let updated = self.balance_mgr.apply_changes(&balance_changes, ts);
                    let account_id = ctx.account_id;
                    let pue = self.balance_mgr.build_position_snapshot_for_symbols(
                        account_id, &updated, false, ts,
                    );
                    actions.push(OmsAction::PublishBalanceUpdate(Box::new(pue)));
                }
            }
        }

        // Update order state
        let update_events = self.order_mgr.update_with_report(&report);

        // Replay any buffered reports
        let ctx_order_exch_ref = ctx.as_ref()
            .and_then(|c| c.order.as_ref())
            .map(|o| o.order_state.exch_order_ref.clone());
        if let Some(exch_ref) = ctx_order_exch_ref {
            let pending = self.pop_pending_reports(&gw_key, &exch_ref);
            for pending_report in pending {
                let more = self.order_mgr.update_with_report(&pending_report);
                for event in more {
                    let oid = event.order_id;
                    let terminal = self.order_mgr.get_order_by_id(oid)
                        .map(|o| o.is_in_terminal_state())
                        .unwrap_or(false);
                    if let Some(order) = self.order_mgr.get_order_by_id(oid).cloned() {
                        actions.push(OmsAction::PersistOrder {
                            order: Box::new(order),
                            set_expire: terminal,
                            set_closed: terminal,
                        });
                    }
                    actions.push(OmsAction::PublishOrderUpdate(Box::new(event)));
                }
            }
        }

        for event in update_events {
            let oid = event.order_id;
            let terminal = self.order_mgr.get_order_by_id(oid)
                .map(|o| o.is_in_terminal_state())
                .unwrap_or(false);
            if let Some(order) = self.order_mgr.get_order_by_id(oid).cloned() {
                actions.push(OmsAction::PersistOrder {
                    order: Box::new(order),
                    set_expire: terminal,
                    set_closed: terminal,
                });
            }
            actions.push(OmsAction::PublishOrderUpdate(Box::new(event)));
        }

        actions
    }

    // ------------------------------------------------------------------
    // Balance update from gateway
    // ------------------------------------------------------------------

    fn process_balance_update(&mut self, update: zk_proto_rs::zk::exch_gw::v1::BalanceUpdate) -> Vec<OmsAction> {
        let ts = gen_timestamp_ms();
        if let Some(account_id) = self.balance_mgr.merge_gw_balance_update(&update, ts) {
            let exch_ts = update.balances.first().map(|b| b.update_timestamp).unwrap_or(ts);
            let pue = self.balance_mgr.build_position_snapshot(account_id, true, exch_ts);
            return vec![OmsAction::PublishBalanceUpdate(Box::new(pue))];
        }
        vec![]
    }

    // ------------------------------------------------------------------
    // Recheck (timeout / retry paths)
    // ------------------------------------------------------------------

    fn process_recheck_order(&mut self, req: OrderRecheckRequest) -> Vec<OmsAction> {
        self.total_retries_orders += 1;
        let ts = req.timestamp + req.check_delay_secs * 1000;
        let is_pending = self.order_mgr.get_order_by_id(req.order_id)
            .map(|o| OrderStatus::try_from(o.order_state.order_status) == Ok(OrderStatus::Pending))
            .unwrap_or(false);

        if is_pending {
            let can_retry = self.total_retries_orders < 20;
            let rejection = Rejection {
                reason: RejectionReason::RejReasonNetworkError as i32,
                source: RejectionSource::ExchReject as i32,
                recoverable: can_retry,
                ..Default::default()
            };
            if let Some(oue) = self.order_mgr.update_with_gw_error(
                req.order_id, ts, "Order still pending after recheck",
                ExecType::PlacingOrder as i32, rejection,
            ) {
                let terminal = self.order_mgr.get_order_by_id(req.order_id)
                    .map(|o| o.is_in_terminal_state())
                    .unwrap_or(false);
                if let Some(order) = self.order_mgr.get_order_by_id(req.order_id).cloned() {
                    return vec![
                        OmsAction::PublishOrderUpdate(Box::new(oue)),
                        OmsAction::PersistOrder { order: Box::new(order), set_expire: terminal, set_closed: terminal },
                    ];
                }
            }
        }
        vec![]
    }

    fn process_recheck_cancel(&mut self, req: CancelRecheckRequest) -> Vec<OmsAction> {
        self.total_retries_cancels += 1;
        let order_id = req.orig_cancel_request.order_id;
        let ts = req.timestamp + req.check_delay_secs * 1000;

        let not_terminal = self.order_mgr.get_order_by_id(order_id)
            .map(|o| !o.is_in_terminal_state())
            .unwrap_or(false);

        if not_terminal {
            let should_retry = self.total_retries_cancels < 20;
            let rejection = Rejection {
                reason: RejectionReason::RejReasonNetworkError as i32,
                source: RejectionSource::ExchReject as i32,
                recoverable: should_retry,
                ..Default::default()
            };
            if let Some(oue) = self.order_mgr.update_with_gw_error(
                order_id, ts, "cancel did not go through after recheck",
                ExecType::Cancel as i32, rejection,
            ) {
                return vec![OmsAction::PublishOrderUpdate(Box::new(oue))];
            }
        }
        vec![]
    }

    // ------------------------------------------------------------------
    // Periodic cleanup
    // ------------------------------------------------------------------

    fn process_cleanup(&mut self, ts_ms: i64) -> Vec<OmsAction> {
        if !self.handle_external_orders {
            return vec![];
        }

        let timeout_ms = 10 * 60 * 1000; // 10 minutes
        let mut to_replay: Vec<(String, OrderReport)> = Vec::new();
        let mut to_delete: Vec<(String, String)> = Vec::new();

        for (gw_key, exch_map) in &self.pending_order_reports {
            for (exch_order_id, reports) in exch_map {
                if let Some(last) = reports.last() {
                    if ts_ms - last.update_timestamp > timeout_ms {
                        to_delete.push((gw_key.clone(), exch_order_id.clone()));
                        for r in reports {
                            let mut r = r.clone();
                            r.order_source_type =
                                zk_proto_rs::zk::exch_gw::v1::OrderSourceType::OrderSourceNonTq as i32;
                            to_replay.push((gw_key.clone(), r));
                        }
                    }
                }
            }
        }

        for (gw_key, exch_order_id) in to_delete {
            if let Some(m) = self.pending_order_reports.get_mut(&gw_key) {
                m.remove(&exch_order_id);
            }
        }

        let mut actions = Vec::new();
        for (_, report) in to_replay {
            actions.extend(self.process_order_report(report));
        }
        actions
    }

    // ------------------------------------------------------------------
    // Context resolution
    // ------------------------------------------------------------------

    fn resolve_context(&self, instrument_code: &str, account_id: i64) -> OrderContext {
        let symbol_ref = self.config.refdata.get(instrument_code).cloned();
        let route = self.config.account_routes.get(&account_id).cloned();
        let gw_config = route
            .as_ref()
            .and_then(|r| self.config.gw_configs.get(&r.gw_key))
            .cloned();
        let trading_config = self.config.trading_configs.get(instrument_code).cloned();

        // Spot: fund = quote asset, position = base asset
        // Non-spot: fund = settlement/quote, position = instrument_id
        const SPOT_LIKE: &[i32] = &[1, 6, 7]; // SPOT, ETF, STOCK
        let (fund_symbol, pos_symbol) = if let Some(ref rd) = symbol_ref {
            let is_spot_like = SPOT_LIKE.contains(&(rd.instrument_type));
            if is_spot_like {
                (Some(rd.quote_asset.clone()), Some(rd.base_asset.clone()))
            } else {
                let fund = if !rd.settlement_asset.is_empty() {
                    rd.settlement_asset.clone()
                } else {
                    rd.quote_asset.clone()
                };
                (Some(fund), Some(rd.instrument_id.clone()))
            }
        } else {
            (None, None)
        };

        // Ensure balance entries exist
        OrderContext {
            account_id,
            fund_symbol,
            pos_symbol,
            route,
            trading_config,
            symbol_ref,
            gw_config,
            order: None,
            errors: Vec::new(),
        }
    }

    fn resolve_context_for_order(&self, order_id: i64) -> Option<OrderContext> {
        let order = self.order_mgr.get_order_by_id(order_id)?;
        let instrument = order
            .oms_req
            .as_ref()
            .map(|r| r.instrument_code.clone())
            .unwrap_or_else(|| order.order_state.instrument.clone());
        let account_id = order.account_id;
        let mut ctx = self.resolve_context(&instrument, account_id);
        ctx.order = Some(order.clone());
        Some(ctx)
    }

    // ------------------------------------------------------------------
    // Balance change calculation for reports
    // ------------------------------------------------------------------

    fn calc_balance_changes_for_report(
        &self,
        _ctx: &OrderContext,
        _report: &OrderReport,
    ) -> Option<Vec<PositionChange>> {
        // Full spot/margin balance bookkeeping from trade and state reports.
        // The Python implementation is elaborate and asset-class specific.
        // For Phase 2 we return None (no bookkeeping from reports) and implement
        // the full logic incrementally in Phase 2b after parity tests are in place.
        None
    }

    // ------------------------------------------------------------------
    // Pending report cache
    // ------------------------------------------------------------------

    fn add_to_pending_reports(&mut self, gw_key: String, report: OrderReport) {
        let exch_order_id = report.exch_order_ref.clone();
        if exch_order_id.is_empty() {
            return;
        }
        self.pending_order_reports
            .entry(gw_key)
            .or_default()
            .entry(exch_order_id)
            .or_default()
            .push(report);
    }

    fn pop_pending_reports(&mut self, gw_key: &str, exch_order_ref: &str) -> Vec<OrderReport> {
        let Some(gw_map) = self.pending_order_reports.get_mut(gw_key) else {
            return vec![];
        };
        let reports = gw_map.remove(exch_order_ref).unwrap_or_default();
        if gw_map.is_empty() {
            self.pending_order_reports.remove(gw_key);
        }
        reports
    }

    // ------------------------------------------------------------------
    // Read helpers (for service layer queries)
    // ------------------------------------------------------------------

    pub fn get_open_orders(&self, account_id: i64) -> Vec<&OmsOrder> {
        self.order_mgr.get_open_orders_for_account(account_id)
    }

    pub fn get_account_balance(&self, account_id: i64) -> Vec<&OmsPosition> {
        let use_exch = self.config.account_id_to_gw_config.get(&account_id)
            .map(|g| !g.calc_balance_needed)
            .unwrap_or(true);
        self.balance_mgr.get_balances_for_account(account_id, use_exch)
    }
}
