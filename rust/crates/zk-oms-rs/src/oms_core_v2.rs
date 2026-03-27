use std::collections::{HashMap, HashSet};

use tracing::{debug, error, warn};

use zk_proto_rs::zk::{
    common::v1::{BuySellType, LongShortType, Rejection, RejectionReason, RejectionSource},
    exch_gw::v1::{order_report_entry::Report, ExchangeOrderStatus, OrderReport, OrderReportType},
    gateway::v1::SendOrderRequest as ExchSendOrderRequest,
    oms::v1::{
        Balance, BalanceUpdateEvent, ExecType, Fee, Order, OrderCancelRequest, OrderRequest,
        OrderUpdateEvent, Position, PositionUpdateEvent, Trade,
    },
};

use crate::{
    balance_v2::BalanceStoreV2,
    config::ConfdataManager,
    metadata_v2::{MetadataTables, TradingMeta},
    models::{
        ExchBalanceSnapshot, ExchPositionSnapshot, OmsAction, OmsManagedPosition, OmsMessage,
    },
    models_v2::*,
    order_store_v2::OrderStoreV2,
    position_v2::PositionStoreV2,
    reservation_v2::ReservationStoreV2,
    utils::{gen_timestamp_ms, round_to_precision, SPOT_INSTRUMENT_TYPE},
    validation::{validate_cancel_request, validate_order_request},
};

// ---------------------------------------------------------------------------
// Default trading meta (used when no trading config exists for an instrument)
// ---------------------------------------------------------------------------

fn default_trading_meta() -> TradingMeta {
    TradingMeta {
        instrument_id: 0,
        bookkeeping_balance: false,
        balance_check: false,
        use_margin: false,
        margin_ratio: 1.0,
        max_order_size: None,
        publish_balance_on_trade: true,
        publish_balance_on_book: false,
        publish_balance_on_cancel: false,
    }
}

// ---------------------------------------------------------------------------
// OmsCoreV2
// ---------------------------------------------------------------------------

/// V2 OMS core state machine -- compact integer-ID based.
/// Same process_message interface as v1 OmsCore, but returns OmsActionV2.
pub struct OmsCoreV2 {
    pub metadata: MetadataTables,
    pub orders: OrderStoreV2,
    pub reservations: ReservationStoreV2,
    pub positions: PositionStoreV2,
    pub balances: BalanceStoreV2,
    pending_reports: HashMap<(u32, DynStrId), Vec<OrderReport>>,
    panic_accounts: HashSet<i64>,
    retry_state: RetryState,
    use_time_emulation: bool,
    risk_check_enabled: bool,
    handle_external_orders: bool,
}

impl OmsCoreV2 {
    pub fn new(
        config: &ConfdataManager,
        use_time_emulation: bool,
        risk_check_enabled: bool,
        handle_external_orders: bool,
        max_cached_orders: usize,
    ) -> Self {
        let metadata = MetadataTables::from_confdata(config);
        let orders = OrderStoreV2::new(max_cached_orders, use_time_emulation);
        let reservations = ReservationStoreV2::new();
        let positions = PositionStoreV2::new();
        let mut balances = BalanceStoreV2::new();

        // Build account reverse map from routes
        let mut reverse_map = HashMap::new();
        for (account_id, route) in &metadata.route_by_account {
            reverse_map.insert(route.exch_account_sym, *account_id);
        }
        balances.init_account_reverse_map(reverse_map);

        Self {
            metadata,
            orders,
            reservations,
            positions,
            balances,
            pending_reports: HashMap::new(),
            panic_accounts: HashSet::new(),
            retry_state: RetryState::default(),
            use_time_emulation,
            risk_check_enabled,
            handle_external_orders,
        }
    }

    // ------------------------------------------------------------------
    // Initialisation
    // ------------------------------------------------------------------

    /// Warm-start initialization. Accepts v1 OmsOrder objects for compatibility.
    pub fn init_state(
        &mut self,
        warm_orders: Vec<crate::models::OmsOrder>,
        positions: Vec<OmsManagedPosition>,
        balances: Vec<ExchBalanceSnapshot>,
    ) {
        // Convert v1 OmsOrders to v2 LiveOrder + OrderDetailLog pairs
        for v1_order in warm_orders {
            let (live, detail) = self.convert_v1_order(&v1_order);
            self.orders.init_with_orders(vec![(live, detail)]);
        }
        // Convert positions
        for pos in positions {
            if let Some(inst) = self.metadata.resolve_instrument(&pos.instrument_code) {
                let v2_pos = ManagedPositionV2 {
                    account_id: pos.account_id,
                    instrument_id: inst.instrument_id,
                    instrument_type: pos.instrument_type,
                    is_short: pos.is_short,
                    qty_total: pos.qty_total,
                    qty_frozen: pos.qty_frozen,
                    qty_available: pos.qty_available,
                    last_local_update_ts: pos.last_local_update_ts,
                    last_exch_sync_ts: pos.last_exch_sync_ts,
                    reconcile_status: pos.reconcile_status,
                    last_exch_qty: pos.last_exch_qty,
                    first_diverged_ts: pos.first_diverged_ts,
                    divergence_count: pos.divergence_count,
                };
                self.positions
                    .managed
                    .insert((pos.account_id, inst.instrument_id), v2_pos);
            }
        }
        // Init balances
        for snap in balances {
            let asset_sym = self.metadata.strings.lookup(&snap.asset);
            if let Some(sym) = asset_sym {
                if let Some(&asset_id) = self.metadata.asset_by_symbol.get(&sym) {
                    self.balances.set_balance(snap.account_id, asset_id, snap);
                    continue;
                }
            }
            let asset = snap.asset.clone();
            self.balances
                .set_unknown_balance(snap.account_id, &asset, snap);
        }
    }

    /// Reload config, preserving ID stability via the existing InternTable.
    pub fn reload_config(&mut self, new_config: ConfdataManager) {
        self.metadata.rebuild_from_config(&new_config);
    }

    // ------------------------------------------------------------------
    // Main entry point
    // ------------------------------------------------------------------

    pub fn process_message(&mut self, msg: OmsMessage) -> Vec<OmsActionV2> {
        match msg {
            OmsMessage::PlaceOrder(req) => self.process_order(req),
            OmsMessage::BatchPlaceOrders(reqs) => {
                let mut actions = Vec::new();
                for req in reqs {
                    actions.extend(self.process_order(req));
                }
                actions
            }
            OmsMessage::CancelOrder(req) => self.process_cancel(req),
            OmsMessage::BatchCancelOrders(reqs) => {
                let mut actions = Vec::new();
                for req in reqs {
                    actions.extend(self.process_cancel(req));
                }
                actions
            }
            OmsMessage::GatewayOrderReport(report) => self.process_order_report(report),
            OmsMessage::BalanceUpdate(update) => self.process_balance_update(update),
            OmsMessage::Panic { account_id } => {
                self.panic_accounts.insert(account_id);
                vec![]
            }
            OmsMessage::DontPanic { account_id } => {
                self.panic_accounts.remove(&account_id);
                vec![]
            }
            OmsMessage::Cleanup { ts_ms } => self.process_cleanup(ts_ms),
            OmsMessage::ReloadConfig => vec![], // TODO
            OmsMessage::PositionRecheck => self.process_position_recheck(),
            OmsMessage::RecheckOrder(_) | OmsMessage::RecheckCancel(_) => vec![], // handled by service layer
            OmsMessage::GatewaySendFailed {
                order_id,
                error_msg,
                ..
            } => self.process_gateway_send_failed(order_id, &error_msg),
            OmsMessage::GatewayCancelSendFailed {
                order_id,
                error_msg,
                ..
            } => self.process_gateway_cancel_send_failed(order_id, &error_msg),
        }
    }

    // ------------------------------------------------------------------
    // Gateway send failure handling
    // ------------------------------------------------------------------

    /// Handle a gateway send failure for a place-order. Treats the order as
    /// synthetically rejected: marks terminal, releases reservations, unfreezes
    /// positions, and emits persist + publish actions.
    fn process_gateway_send_failed(&mut self, order_id: i64, error_msg: &str) -> Vec<OmsActionV2> {
        let mut actions = Vec::new();
        let Some(live) = self.orders.get_live(order_id) else {
            warn!(order_id, "GatewaySendFailed: order not found — ignoring");
            return actions;
        };

        // Skip if already in terminal state (report may have arrived first)
        if live.is_in_terminal_state() {
            return actions;
        }

        let account_id = live.account_id;
        let accounting_flags = live.accounting_flags;
        let ts = gen_timestamp_ms();

        let rejection = Rejection {
            source: RejectionSource::OmsReject as i32,
            reason: RejectionReason::RejReasonOmsInternalError as i32,
            ..Default::default()
        };
        let full_msg = format!("gateway send failed: {error_msg}");
        {
            let (live, detail) = self.orders.get_live_and_detail_mut(order_id).unwrap();
            OrderStoreV2::apply_oms_error(
                live,
                detail,
                &full_msg,
                ts,
                ExecType::PlacingOrder as i32,
                rejection,
            );
        }
        self.orders.mark_closed(order_id);

        // Release reservations
        self.reservations.release(order_id);

        // Unfreeze position if sell order
        if let Some(pos_id) = accounting_flags.pos_instrument_id {
            if !accounting_flags.is_buy {
                let remaining = self
                    .orders
                    .get_live(order_id)
                    .map(|l| l.qty - l.filled_qty)
                    .unwrap_or(0.0);
                if remaining > 0.0 {
                    self.positions.unfreeze(account_id, pos_id, remaining);
                }
            }
        }

        actions.push(OmsActionV2::PersistOrder {
            order_id,
            set_expire: true,
            set_closed: true,
        });
        actions.push(OmsActionV2::PublishOrderUpdate {
            order_id,
            include_last_trade: false,
            include_last_fee: false,
            include_exec_message: true,
            include_inferred_trade: false,
        });
        actions
    }

    /// Handle a gateway cancel-send failure. Marks the cancel as failed but
    /// does NOT terminal the order (it may still be live on the exchange).
    fn process_gateway_cancel_send_failed(
        &mut self,
        order_id: i64,
        error_msg: &str,
    ) -> Vec<OmsActionV2> {
        let mut actions = Vec::new();
        let Some(live) = self.orders.get_live(order_id) else {
            warn!(
                order_id,
                "GatewayCancelSendFailed: order not found — ignoring"
            );
            return actions;
        };

        // Skip if already in terminal state
        if live.is_in_terminal_state() {
            return actions;
        }

        let ts = gen_timestamp_ms();
        let rejection = Rejection {
            source: RejectionSource::OmsReject as i32,
            reason: RejectionReason::RejReasonOmsInternalError as i32,
            ..Default::default()
        };
        let full_msg = format!("gateway cancel send failed: {error_msg}");
        {
            let (live, detail) = self.orders.get_live_and_detail_mut(order_id).unwrap();
            OrderStoreV2::apply_oms_error(
                live,
                detail,
                &full_msg,
                ts,
                ExecType::Cancel as i32,
                rejection,
            );
        }

        // Note: we do NOT mark as closed or release reservations here.
        // The order is still live on the exchange — the cancel just failed to send.
        // The order will eventually reach a terminal state via normal GW reports.

        actions.push(OmsActionV2::PersistOrder {
            order_id,
            set_expire: false,
            set_closed: false,
        });
        actions.push(OmsActionV2::PublishOrderUpdate {
            order_id,
            include_last_trade: false,
            include_last_fee: false,
            include_exec_message: true,
            include_inferred_trade: false,
        });
        actions
    }

    // ------------------------------------------------------------------
    // Order placement
    // ------------------------------------------------------------------

    fn process_order(&mut self, req: OrderRequest) -> Vec<OmsActionV2> {
        let mut actions = Vec::new();
        let order_id = req.order_id;
        let account_id = req.account_id;
        let ts = if self.use_time_emulation {
            req.timestamp
        } else {
            gen_timestamp_ms()
        };

        if let Err(msg) = validate_order_request(&req) {
            warn!(
                order_id,
                account_id, "rejecting invalid order request in OmsCoreV2: {msg}"
            );
            return actions;
        }

        // 1. Check panic mode
        if self.panic_accounts.contains(&account_id) {
            return vec![];
        }

        // 2. Resolve context
        let mut meta = match self.resolve_order_meta(&req) {
            Ok(m) => m,
            Err(errors) => {
                let error_msg = errors.join("; ");
                warn!(
                    order_id,
                    account_id,
                    instrument_code = %req.instrument_code,
                    qty = req.qty,
                    price = req.price,
                    source_id = %req.source_id,
                    "resolve_order_meta rejected order request: {error_msg}"
                );
                let rejection = Rejection {
                    source: RejectionSource::OmsReject as i32,
                    reason: RejectionReason::RejReasonOmsInternalError as i32,
                    ..Default::default()
                };
                let flags = OrderAccountingFlags::default();
                let stub_meta = ResolvedOrderMeta {
                    account_id,
                    gw_id: 0,
                    instrument_id: 0,
                    instrument_exch_sym: 0,
                    exch_account_sym: 0,
                    source_sym: 0,
                    fund_asset_id: None,
                    pos_instrument_id: None,
                    bookkeeping_balance: false,
                    balance_check: false,
                    use_margin: false,
                    max_order_size: None,
                    qty_precision: 0,
                    price_precision: 0,
                };
                self.orders
                    .create_order(order_id, &req, &stub_meta, flags, None);
                {
                    let (live, detail) = self.orders.get_live_and_detail_mut(order_id).unwrap();
                    OrderStoreV2::apply_oms_error(
                        live,
                        detail,
                        &error_msg,
                        ts,
                        ExecType::PlacingOrder as i32,
                        rejection,
                    );
                }
                self.orders.mark_closed(order_id);
                actions.push(OmsActionV2::PublishOrderUpdate {
                    order_id,
                    include_last_trade: false,
                    include_last_fee: false,
                    include_exec_message: true,
                    include_inferred_trade: false,
                });
                return actions;
            }
        };

        // Intern source_id from request (not from instrument metadata)
        if !req.source_id.is_empty() {
            meta.source_sym = self.metadata.strings.intern(&req.source_id);
        }

        // 3. Risk check
        if self.risk_check_enabled {
            if let Some(max_size) = meta.max_order_size {
                if req.qty > max_size {
                    let rejection = Rejection {
                        source: RejectionSource::OmsReject as i32,
                        reason: RejectionReason::RejReasonOmsInvalidState as i32,
                        ..Default::default()
                    };
                    let flags = OrderAccountingFlags::default();
                    self.orders.create_order(order_id, &req, &meta, flags, None);
                    let error_msg = format!("order qty {} exceeds max {}", req.qty, max_size);
                    {
                        let (live, detail) = self.orders.get_live_and_detail_mut(order_id).unwrap();
                        OrderStoreV2::apply_oms_error(
                            live,
                            detail,
                            &error_msg,
                            ts,
                            ExecType::PlacingOrder as i32,
                            rejection,
                        );
                    }
                    self.orders.mark_closed(order_id);
                    actions.push(OmsActionV2::PublishOrderUpdate {
                        order_id,
                        include_last_trade: false,
                        include_last_fee: false,
                        include_exec_message: true,
                        include_inferred_trade: false,
                    });
                    return actions;
                }
            }
        }

        // 4. Pre-trade checks (balance/position reservation)
        let is_buy = req.buy_sell_type == BuySellType::BsBuy as i32;
        let flags = OrderAccountingFlags {
            bookkeeping_balance: meta.bookkeeping_balance,
            balance_check: meta.balance_check,
            use_margin: meta.use_margin,
            is_buy,
            fund_asset_id: meta.fund_asset_id,
            pos_instrument_id: meta.pos_instrument_id,
        };

        if meta.bookkeeping_balance && !meta.use_margin {
            if is_buy {
                // Check + reserve cash
                if let Some(fund_id) = meta.fund_asset_id {
                    if meta.balance_check {
                        let cost = req.qty * req.price;
                        let avail = self
                            .balances
                            .get_balance(account_id, fund_id)
                            .map(|b| b.balance_state.avail_qty)
                            .unwrap_or(0.0);
                        if let Err(e) = self
                            .reservations
                            .check_available(account_id, fund_id, cost, avail)
                        {
                            let rejection = Rejection {
                                source: RejectionSource::OmsReject as i32,
                                reason: RejectionReason::RejReasonOmsInvalidState as i32,
                                ..Default::default()
                            };
                            self.orders.create_order(order_id, &req, &meta, flags, None);
                            {
                                let (live, detail) =
                                    self.orders.get_live_and_detail_mut(order_id).unwrap();
                                OrderStoreV2::apply_oms_error(
                                    live,
                                    detail,
                                    &e,
                                    ts,
                                    ExecType::PlacingOrder as i32,
                                    rejection,
                                );
                            }
                            self.orders.mark_closed(order_id);
                            actions.push(OmsActionV2::PublishOrderUpdate {
                                order_id,
                                include_last_trade: false,
                                include_last_fee: false,
                                include_exec_message: true,
                                include_inferred_trade: false,
                            });
                            return actions;
                        }
                        let _ = self.reservations.reserve(
                            order_id,
                            account_id,
                            ReserveKind::CashAsset,
                            fund_id,
                            cost,
                        );
                    }
                }
            } else {
                // Check + freeze position for sell
                if let Some(pos_id) = meta.pos_instrument_id {
                    let is_short = false; // sells from long positions by default
                    if let Err(e) = self
                        .positions
                        .check_and_freeze(account_id, pos_id, req.qty, is_short)
                    {
                        let rejection = Rejection {
                            source: RejectionSource::OmsReject as i32,
                            reason: RejectionReason::RejReasonOmsInvalidState as i32,
                            ..Default::default()
                        };
                        self.orders.create_order(order_id, &req, &meta, flags, None);
                        {
                            let (live, detail) =
                                self.orders.get_live_and_detail_mut(order_id).unwrap();
                            OrderStoreV2::apply_oms_error(
                                live,
                                detail,
                                &e,
                                ts,
                                ExecType::PlacingOrder as i32,
                                rejection,
                            );
                        }
                        self.orders.mark_closed(order_id);
                        actions.push(OmsActionV2::PublishOrderUpdate {
                            order_id,
                            include_last_trade: false,
                            include_last_fee: false,
                            include_exec_message: true,
                            include_inferred_trade: false,
                        });
                        return actions;
                    }
                    let _ = self.reservations.reserve(
                        order_id,
                        account_id,
                        ReserveKind::InventoryInstrument,
                        pos_id,
                        req.qty,
                    );
                }
            }
        }

        // 5. Build gateway request
        let gw_req = self.build_gw_request(&req, &meta);
        let scaled_price = gw_req.scaled_price;
        let scaled_qty = gw_req.scaled_qty;

        // 6. Create order in store
        self.orders
            .create_order(order_id, &req, &meta, flags, Some(Box::new(gw_req)));
        // Update scaled price/qty on the live order
        if let Some(live) = self.orders.get_live_mut(order_id) {
            live.price = scaled_price;
            live.qty = scaled_qty;
        }

        // 7. Emit actions
        actions.push(OmsActionV2::PersistOrder {
            order_id,
            set_expire: false,
            set_closed: false,
        });
        actions.push(OmsActionV2::SendOrderToGw {
            gw_id: meta.gw_id,
            order_id,
            order_created_at: ts,
        });

        actions
    }

    // ------------------------------------------------------------------
    // Context resolution
    // ------------------------------------------------------------------

    fn resolve_order_meta(&self, req: &OrderRequest) -> Result<ResolvedOrderMeta, Vec<String>> {
        let mut errors = Vec::new();

        let route = match self.metadata.route(req.account_id) {
            Some(r) => r,
            None => {
                errors.push(format!("no route for account {}", req.account_id));
                return Err(errors);
            }
        };

        let instrument = match self.metadata.resolve_instrument(&req.instrument_code) {
            Some(i) => i,
            None => {
                errors.push(format!("unknown instrument {}", req.instrument_code));
                return Err(errors);
            }
        };

        let default_tc = default_trading_meta();
        let tc = self
            .metadata
            .trading(instrument.instrument_id)
            .unwrap_or(&default_tc);

        // Determine fund and position symbols based on instrument type.
        // V1 spot: fund=quote_asset, pos=base_asset (inventory tracking).
        // V1 deriv: fund=settlement/quote, pos=instrument_code.
        // V2 uses intern syms as position keys to avoid table-index collisions.
        let (fund_asset_id, pos_instrument_id) =
            if instrument.instrument_type == SPOT_INSTRUMENT_TYPE {
                // Spot: fund=quote_asset, pos=base_asset (inventory)
                (instrument.quote_asset_id, instrument.base_asset_sym)
            } else {
                // Derivatives: fund=settlement/quote, pos=instrument_code_sym
                let fund = instrument.settlement_asset_id.or(instrument.quote_asset_id);
                (fund, Some(instrument.instrument_code_sym))
            };

        Ok(ResolvedOrderMeta {
            account_id: req.account_id,
            gw_id: route.gw_id,
            instrument_id: instrument.instrument_id,
            instrument_exch_sym: instrument.instrument_exch_sym,
            exch_account_sym: route.exch_account_sym,
            source_sym: 0, // Set by caller after intern
            fund_asset_id,
            pos_instrument_id,
            bookkeeping_balance: tc.bookkeeping_balance,
            balance_check: tc.balance_check,
            use_margin: tc.use_margin,
            max_order_size: tc.max_order_size,
            qty_precision: instrument.qty_precision,
            price_precision: instrument.price_precision,
        })
    }

    // ------------------------------------------------------------------
    // Gateway request building
    // ------------------------------------------------------------------

    fn build_gw_request(
        &self,
        req: &OrderRequest,
        meta: &ResolvedOrderMeta,
    ) -> ExchSendOrderRequest {
        let qty = if meta.qty_precision > 0 {
            round_to_precision(req.qty, meta.qty_precision as i64)
        } else {
            req.qty
        };
        let price = if meta.price_precision > 0 {
            round_to_precision(req.price, meta.price_precision as i64)
        } else {
            req.price
        };
        let ts = if self.use_time_emulation {
            req.timestamp
        } else {
            gen_timestamp_ms()
        };

        let mut gw_req = ExchSendOrderRequest::default();
        gw_req.exch_account_id = self.metadata.str_resolve(meta.exch_account_sym).to_string();
        gw_req.order_type = req.order_type;
        gw_req.buysell_type = req.buy_sell_type;
        gw_req.openclose_type = req.open_close_type;
        gw_req.correlation_id = req.order_id;
        gw_req.instrument = self
            .metadata
            .str_resolve(meta.instrument_exch_sym)
            .to_string();
        gw_req.scaled_qty = qty;
        gw_req.scaled_price = price;
        gw_req.timestamp = ts;
        gw_req.time_inforce_type = req.time_inforce_type;
        if let Some(extra) = &req.extra_properties {
            gw_req.exch_specific_params = Some(extra.clone());
        }
        gw_req
    }

    // ------------------------------------------------------------------
    // Cancel
    // ------------------------------------------------------------------

    fn process_cancel(&mut self, req: OrderCancelRequest) -> Vec<OmsActionV2> {
        let mut actions = Vec::new();
        let order_id = req.order_id;

        if let Err(msg) = validate_cancel_request(&req) {
            warn!(
                order_id,
                "rejecting invalid cancel request in OmsCoreV2: {msg}"
            );
            return actions;
        }

        // Extract needed data from immutable borrow first
        let (is_external, is_terminal, gw_id, _account_id, _instrument_id, exch_ref_str) = {
            let live = match self.orders.get_live(order_id) {
                Some(o) => o,
                None => {
                    warn!(order_id, "cancel: order not found");
                    return vec![];
                }
            };
            let exch_ref = live
                .exch_order_ref_id
                .map(|id| self.orders.dyn_strings.resolve(id).to_string())
                .unwrap_or_default();
            (
                live.is_external,
                live.is_in_terminal_state(),
                live.gw_id,
                live.account_id,
                live.instrument_id,
                exch_ref,
            )
        };

        if is_external {
            return vec![];
        }

        if is_terminal {
            let ts = if self.use_time_emulation {
                req.timestamp
            } else {
                gen_timestamp_ms()
            };
            let rejection = Rejection {
                source: RejectionSource::OmsReject as i32,
                reason: RejectionReason::RejReasonOmsInternalError as i32,
                ..Default::default()
            };
            let (live, detail) = self.orders.get_live_and_detail_mut(order_id).unwrap();
            OrderStoreV2::apply_oms_error(
                live,
                detail,
                "cannot cancel terminal order",
                ts,
                ExecType::Cancel as i32,
                rejection,
            );
            actions.push(OmsActionV2::PublishOrderUpdate {
                order_id,
                include_last_trade: false,
                include_last_fee: false,
                include_exec_message: true,
                include_inferred_trade: false,
            });
            return actions;
        }

        // Build cancel request
        let mut cancel_req = zk_proto_rs::zk::gateway::v1::CancelOrderRequest::default();
        cancel_req.order_id = order_id;
        cancel_req.exch_order_ref = exch_ref_str;
        cancel_req.timestamp = if self.use_time_emulation {
            req.timestamp
        } else {
            gen_timestamp_ms()
        };

        if let Some(detail) = self.orders.get_detail_mut(order_id) {
            detail.cancel_req = Some(Box::new(cancel_req));
        }

        let live = self.orders.get_live_mut(order_id).unwrap();
        live.cancel_attempts += 1;

        actions.push(OmsActionV2::SendCancelToGw { gw_id, order_id });
        actions
    }

    // ------------------------------------------------------------------
    // Order report processing
    // ------------------------------------------------------------------

    fn process_order_report(&mut self, report: OrderReport) -> Vec<OmsActionV2> {
        let order_id = report.order_id;
        let exch_order_ref = report.exch_order_ref.clone();
        let gw_key = report.exchange.clone();

        // Resolve gateway
        let gw_id = match self
            .metadata
            .strings
            .lookup(&gw_key)
            .and_then(|sym| self.metadata.gw_by_key.get(&sym).copied())
        {
            Some(id) => id,
            None => {
                error!(gw_key = %gw_key, "unknown gateway in report");
                return vec![];
            }
        };

        // Resolve order
        let resolved_id = self
            .orders
            .resolve_order_id(order_id, gw_id, &exch_order_ref);

        // Handle external orders
        if resolved_id.is_none() {
            if self.handle_external_orders && !exch_order_ref.is_empty() {
                // Check if already registered as external
                let existing = self
                    .orders
                    .dyn_strings
                    .lookup(&exch_order_ref)
                    .and_then(|ref_id| {
                        if self
                            .orders
                            .external_order_ids
                            .contains_key(&(gw_id, ref_id))
                        {
                            Some(ref_id)
                        } else {
                            None
                        }
                    });

                let ext_order_id = if existing.is_none() {
                    let source_label = format!("External@{}", gw_key);
                    let source_sym = self.metadata.strings.intern(&source_label);
                    self.orders.create_external_order(
                        gw_id,
                        &exch_order_ref,
                        report.account_id,
                        report.update_timestamp,
                        source_sym,
                    )
                } else {
                    let ref_id = self.orders.dyn_strings.lookup(&exch_order_ref).unwrap();
                    *self
                        .orders
                        .external_order_ids
                        .get(&(gw_id, ref_id))
                        .unwrap()
                };

                return self.process_report_for_order(ext_order_id, gw_id, &report);
            }

            // Buffer for later (pending report)
            if !exch_order_ref.is_empty() {
                let ref_id = self.orders.dyn_strings.intern(&exch_order_ref);
                self.pending_reports
                    .entry((gw_id, ref_id))
                    .or_default()
                    .push(report);
            }
            return vec![];
        }

        let resolved_id = resolved_id.unwrap();
        self.process_report_for_order(resolved_id, gw_id, &report)
    }

    fn process_report_for_order(
        &mut self,
        order_id: i64,
        _gw_id: u32,
        report: &OrderReport,
    ) -> Vec<OmsActionV2> {
        let mut actions = Vec::new();
        let ts = report.update_timestamp;

        // Infer state entry if needed (before mutable borrow)
        let extra_entry = {
            let live = match self.orders.get_live(order_id) {
                Some(l) => l,
                None => return vec![],
            };
            OrderStoreV2::infer_state_from_trades(&report.order_report_entries, live)
        };

        let mut original_order_updated = false;
        let mut new_trade = false;
        let mut new_oms_trade = false;
        let mut new_fee = false;
        let mut new_exec_msg = false;

        // Track trade counts before entry processing for per-trade fill loop
        let trades_before = self
            .orders
            .get_detail(order_id)
            .map(|d| d.trades.len())
            .unwrap_or(0);
        let inferred_before = self
            .orders
            .get_detail(order_id)
            .map(|d| d.inferred_trades.len())
            .unwrap_or(0);

        // Update timestamp
        if let Some(live) = self.orders.get_live_mut(order_id) {
            live.updated_at = ts;
        }

        // Collect entries to process (owned + extra)
        let entries_len = report.order_report_entries.len();
        let total_entries = entries_len + if extra_entry.is_some() { 1 } else { 0 };

        for idx in 0..total_entries {
            let entry = if idx < entries_len {
                &report.order_report_entries[idx]
            } else {
                extra_entry.as_ref().unwrap()
            };

            let report_type = OrderReportType::try_from(entry.report_type)
                .unwrap_or(OrderReportType::OrderRepTypeUnspecified);

            match report_type {
                OrderReportType::OrderRepTypeLinkage => {
                    if let Some(Report::OrderIdLinkageReport(linkage)) = &entry.report {
                        self.orders
                            .register_exch_ref(order_id, _gw_id, &linkage.exch_order_ref);

                        // If linkage-only, promote PENDING -> BOOKED
                        if entries_len == 1 && extra_entry.is_none() {
                            self.orders.apply_linkage_promotion(order_id);
                            original_order_updated = true;
                        }
                    }
                }

                OrderReportType::OrderRepTypeState => {
                    if let Some(Report::OrderStateReport(state_report)) = &entry.report {
                        let (live, detail) = self.orders.get_live_and_detail_mut(order_id).unwrap();

                        let new_filled = if state_report.filled_qty != 0.0 {
                            Some(state_report.filled_qty)
                        } else if state_report.unfilled_qty != 0.0 {
                            Some(live.qty - state_report.unfilled_qty)
                        } else {
                            None
                        };

                        let inferred_before = detail.inferred_trades.len();

                        let updated = OrderStoreV2::apply_state_report(
                            live,
                            detail,
                            new_filled,
                            state_report.avg_price,
                            ExchangeOrderStatus::try_from(state_report.exch_order_status)
                                .unwrap_or(ExchangeOrderStatus::ExchOrderStatusUnspecified),
                            ts,
                        );
                        if updated {
                            original_order_updated = true;
                            if detail.inferred_trades.len() > inferred_before {
                                new_oms_trade = true;
                            }
                        }
                    }
                }

                OrderReportType::OrderRepTypeTrade => {
                    if let Some(Report::TradeReport(trade_report)) = &entry.report {
                        // Extract data from immutable borrow first
                        let (
                            live_order_id,
                            ext_order_ref,
                            live_account_id,
                            live_buy_sell_type,
                            instrument_code,
                        ) = {
                            let live = self.orders.get_live(order_id).unwrap();
                            let ext_ref = live
                                .exch_order_ref_id
                                .map(|id| self.orders.dyn_strings.resolve(id).to_string())
                                .unwrap_or_default();
                            let inst_code = self
                                .metadata
                                .instrument(live.instrument_id)
                                .map(|i| {
                                    self.metadata.str_resolve(i.instrument_code_sym).to_string()
                                })
                                .unwrap_or_default();
                            (
                                live.order_id,
                                ext_ref,
                                live.account_id,
                                live.buy_sell_type,
                                inst_code,
                            )
                        };

                        let trade = Trade {
                            order_id: live_order_id,
                            ext_order_ref: ext_order_ref,
                            ext_trade_id: trade_report.exch_trade_id.clone(),
                            filled_qty: trade_report.filled_qty,
                            filled_price: trade_report.filled_price,
                            filled_ts: ts,
                            account_id: live_account_id,
                            buy_sell_type: live_buy_sell_type,
                            instrument: instrument_code,
                            fill_type: trade_report.fill_type,
                            exch_pnl: trade_report.exch_pnl,
                            ..Default::default()
                        };
                        let (live, detail) = self.orders.get_live_and_detail_mut(order_id).unwrap();
                        OrderStoreV2::apply_trade_report(live, detail, trade);
                        new_trade = true;
                        original_order_updated = true;
                    }
                }

                OrderReportType::OrderRepTypeExec => {
                    if let Some(Report::ExecReport(exec_report)) = &entry.report {
                        use zk_proto_rs::zk::exch_gw::v1::ExchExecType;
                        let exec_type = ExchExecType::try_from(exec_report.exec_type)
                            .unwrap_or(ExchExecType::Unspecified);
                        let oms_exec_type = match exec_type {
                            ExchExecType::Rejected => ExecType::PlacingOrder,
                            ExchExecType::CancelReject => ExecType::Cancel,
                            _ => ExecType::Unspecified,
                        };
                        if oms_exec_type != ExecType::Unspecified {
                            let rejection =
                                exec_report.rejection_info.clone().unwrap_or_default();
                            let msg = if let Some(err_info) = &exec_report.error_info {
                                format!(
                                    "err from gw: code={}, msg={}",
                                    err_info.error_code, err_info.error_message
                                )
                            } else if !rejection.error_message.is_empty() {
                                rejection.error_message.clone()
                            } else if !exec_report.exec_message.is_empty() {
                                exec_report.exec_message.clone()
                            } else {
                                "gateway execution rejected request".to_string()
                            };
                            let (live, detail) =
                                self.orders.get_live_and_detail_mut(order_id).unwrap();
                            OrderStoreV2::apply_oms_error(
                                live,
                                detail,
                                &msg,
                                ts,
                                oms_exec_type as i32,
                                rejection,
                            );
                            new_exec_msg = true;
                            original_order_updated = true;
                        }
                    }
                }

                OrderReportType::OrderRepTypeFee => {
                    if let Some(Report::FeeReport(fee_report)) = &entry.report {
                        let live_order_id = self.orders.get_live(order_id).unwrap().order_id;
                        let fee = Fee {
                            order_id: live_order_id,
                            fee_amount: fee_report.fee_qty,
                            fee_symbol: fee_report.fee_symbol.clone(),
                            fee_ts: ts,
                            fee_type: fee_report.fee_type,
                        };
                        let detail = self.orders.get_detail_mut(order_id).unwrap();
                        OrderStoreV2::apply_fee_report(detail, fee);
                        new_fee = true;
                        original_order_updated = true;
                    }
                }

                _ => {}
            }
        }

        if !original_order_updated {
            return vec![];
        }

        // Finalize: extract needed data, then mutate
        let (is_terminal, account_id, buy_sell_type, accounting_flags) = {
            let live = self.orders.get_live_mut(order_id).unwrap();
            live.snapshot_version += 1;
            // Finalize avg price
            if live.is_in_terminal_state()
                && live.filled_avg_price == 0.0
                && live.acc_trades_filled_qty != 0.0
            {
                live.filled_avg_price = live.acc_trades_value / live.acc_trades_filled_qty;
            }
            (
                live.is_in_terminal_state(),
                live.account_id,
                live.buy_sell_type,
                live.accounting_flags,
            )
        };

        // Publish order update
        actions.push(OmsActionV2::PublishOrderUpdate {
            order_id,
            include_last_trade: new_trade,
            include_last_fee: new_fee,
            include_exec_message: new_exec_msg,
            include_inferred_trade: new_oms_trade,
        });

        // Position delta + reservation release for fills — per-trade loop (matches v1)
        if new_trade || new_oms_trade {
            let is_buy = buy_sell_type == BuySellType::BsBuy as i32;
            let sell_was_frozen = !is_buy && accounting_flags.balance_check;
            let order_price = self
                .orders
                .get_live(order_id)
                .map(|l| l.price)
                .unwrap_or(0.0);

            // Collect per-trade fill quantities from all newly-added trades
            let new_fills: Vec<f64> = {
                let detail = self.orders.get_detail(order_id).unwrap();
                let mut fills = Vec::new();
                for t in &detail.trades[trades_before..] {
                    fills.push(t.filled_qty);
                }
                for t in &detail.inferred_trades[inferred_before..] {
                    fills.push(t.filled_qty);
                }
                fills
            };

            let mut position_changed = false;
            for fill_qty in &new_fills {
                if *fill_qty <= 0.0 {
                    continue;
                }

                // Apply position delta per trade
                if let Some(pos_id) = accounting_flags.pos_instrument_id {
                    let delta = PositionDeltaV2 {
                        account_id,
                        instrument_id: pos_id,
                        is_short: !is_buy,
                        avail_change: if is_buy {
                            *fill_qty
                        } else if sell_was_frozen {
                            0.0
                        } else {
                            0.0
                        },
                        frozen_change: if sell_was_frozen { -*fill_qty } else { 0.0 },
                        total_change: if is_buy { *fill_qty } else { -*fill_qty },
                    };
                    self.positions.apply_delta(&delta, ts);
                    position_changed = true;
                }

                // Release reservation proportional to this trade's fill.
                // Buy: release cash using order price (not fill price).
                // Sell: release inventory by filled qty.
                let release_amount = if accounting_flags.is_buy {
                    *fill_qty * order_price
                } else {
                    *fill_qty
                };
                self.reservations.release_partial(order_id, release_amount);
            }

            if position_changed {
                if let Some(pos_id) = accounting_flags.pos_instrument_id {
                    actions.push(OmsActionV2::PersistPosition {
                        account_id,
                        instrument_id: pos_id,
                    });
                    actions.push(OmsActionV2::PublishPositionUpdate { account_id });
                }
            }
        }

        // Terminal state handling
        if is_terminal {
            self.reservations.release(order_id);
            // Unfreeze remaining position if sell was cancelled
            if let Some(pos_id) = accounting_flags.pos_instrument_id {
                if !accounting_flags.is_buy {
                    let remaining = self
                        .orders
                        .get_live(order_id)
                        .map(|l| l.qty - l.filled_qty)
                        .unwrap_or(0.0);
                    if remaining > 0.0 {
                        self.positions.unfreeze(account_id, pos_id, remaining);
                    }
                }
            }
            self.orders.mark_closed(order_id);
            actions.push(OmsActionV2::PersistOrder {
                order_id,
                set_expire: true,
                set_closed: true,
            });
        } else {
            actions.push(OmsActionV2::PersistOrder {
                order_id,
                set_expire: false,
                set_closed: false,
            });
        }

        actions
    }

    // ------------------------------------------------------------------
    // Balance update
    // ------------------------------------------------------------------

    fn process_balance_update(
        &mut self,
        update: zk_proto_rs::zk::exch_gw::v1::BalanceUpdate,
    ) -> Vec<OmsActionV2> {
        let mut actions = Vec::new();
        let ts = gen_timestamp_ms();
        let total_entries = update.balances.len();

        if total_entries == 0 {
            return vec![];
        }

        // Resolve account from the first entry's exch_account_code
        let exch_account_code = &update.balances[0].exch_account_code;
        let account_id = self
            .metadata
            .strings
            .lookup(exch_account_code)
            .and_then(|sym| self.balances.resolve_account_id(sym))
            .or_else(|| {
                // Try direct lookup from route reverse map
                self.metadata
                    .route_by_account
                    .iter()
                    .find(|(_, r)| {
                        self.metadata.str_resolve(r.exch_account_sym) == exch_account_code.as_str()
                    })
                    .map(|(id, _)| *id)
            });

        let account_id = match account_id {
            Some(id) => id,
            None => {
                warn!(
                    total_entries,
                    exch_account_code,
                    "unknown exchange account in balance update"
                );
                return vec![];
            }
        };

        // Classify entries as spot (balance) vs non-spot (position)
        let mut has_spot = false;
        let mut has_nonspot = false;
        let mut spot_entries = 0usize;
        let mut nonspot_entries = 0usize;

        // Process spot entries -> balance store
        for entry in &update.balances {
            if entry.instrument_type == SPOT_INSTRUMENT_TYPE {
                has_spot = true;
                spot_entries += 1;
                let asset = &entry.instrument_code;
                let asset_id = self
                    .metadata
                    .strings
                    .lookup(asset)
                    .and_then(|s| self.metadata.asset_by_symbol.get(&s).copied());

                if let Some(asset_id) = asset_id {
                    self.balances.remove_unknown_balance(account_id, asset);
                    let snap = self.balances.get_or_create_balance(account_id, asset_id);
                    snap.symbol_exch = Some(entry.instrument_code.clone());
                    snap.balance_state.account_id = account_id;
                    snap.balance_state.asset = asset.clone();
                    snap.balance_state.total_qty = entry.qty;
                    snap.balance_state.avail_qty = entry.avail_qty;
                    snap.balance_state.frozen_qty = entry.qty - entry.avail_qty;
                    snap.balance_state.update_timestamp = entry.update_timestamp;
                    snap.balance_state.sync_timestamp = ts;
                    snap.balance_state.is_from_exch = true;
                    snap.balance_state.exch_data_raw = entry.message_raw.clone();
                    snap.exch_data_raw = entry.message_raw.clone();
                    snap.sync_ts = ts;

                    actions.push(OmsActionV2::PersistBalance {
                        account_id,
                        asset_id,
                    });
                } else {
                    let snap = ExchBalanceSnapshot {
                        account_id,
                        asset: asset.clone(),
                        symbol_exch: Some(entry.instrument_code.clone()),
                        balance_state: Balance {
                            account_id,
                            asset: asset.clone(),
                            total_qty: entry.qty,
                            frozen_qty: entry.qty - entry.avail_qty,
                            avail_qty: entry.avail_qty,
                            sync_timestamp: ts,
                            update_timestamp: entry.update_timestamp,
                            is_from_exch: true,
                            exch_data_raw: entry.message_raw.clone(),
                        },
                        exch_data_raw: entry.message_raw.clone(),
                        sync_ts: ts,
                    };
                    self.balances.set_unknown_balance(account_id, asset, snap);
                }
            }
        }
        if has_spot {
            actions.push(OmsActionV2::PublishBalanceUpdate { account_id });
        }

        // Process non-spot entries -> position store
        for entry in &update.balances {
            if entry.instrument_type != SPOT_INSTRUMENT_TYPE {
                has_nonspot = true;
                nonspot_entries += 1;

                if let Some(route) = self.metadata.route(account_id) {
                    let gw_id = route.gw_id;
                    if let Some(inst) = self
                        .metadata
                        .resolve_instrument_by_gw(gw_id, &entry.instrument_code)
                    {
                        let instrument_id = inst.instrument_id;
                        let instrument_code_str = self
                            .metadata
                            .str_resolve(inst.instrument_code_sym)
                            .to_string();
                        let is_short = entry.long_short_type == LongShortType::LsShort as i32;

                        // Update exchange position snapshot
                        let exch_snap = self
                            .positions
                            .exch
                            .entry((account_id, instrument_id))
                            .or_insert_with(|| ExchPositionSnapshot {
                                account_id,
                                instrument_code: instrument_code_str.clone(),
                                symbol_exch: Some(entry.instrument_code.clone()),
                                position_state: Position::default(),
                                exch_data_raw: String::new(),
                                sync_ts: 0,
                            });
                        exch_snap.symbol_exch = Some(entry.instrument_code.clone());
                        exch_snap.position_state.account_id = account_id;
                        exch_snap.position_state.instrument_code = instrument_code_str;
                        exch_snap.position_state.total_qty = entry.qty;
                        exch_snap.position_state.avail_qty = entry.avail_qty;
                        exch_snap.position_state.frozen_qty = entry.qty - entry.avail_qty;
                        exch_snap.position_state.long_short_type = entry.long_short_type;
                        exch_snap.position_state.instrument_type = entry.instrument_type;
                        exch_snap.position_state.update_timestamp = entry.update_timestamp;
                        exch_snap.position_state.sync_timestamp = ts;
                        exch_snap.exch_data_raw = entry.message_raw.clone();
                        exch_snap.sync_ts = ts;

                        // Seed managed position on first sync
                        let should_seed = self
                            .positions
                            .managed
                            .get(&(account_id, instrument_id))
                            .map(|p| p.last_exch_sync_ts == 0)
                            .unwrap_or(true);
                        if should_seed {
                            let pos = self.positions.get_or_create_position(
                                account_id,
                                instrument_id,
                                entry.instrument_type,
                                is_short,
                            );
                            pos.qty_total = entry.qty;
                            pos.qty_available = entry.avail_qty;
                            pos.qty_frozen = entry.qty - entry.avail_qty;
                            pos.is_short = is_short;
                            pos.last_exch_sync_ts = ts;
                        }

                        self.positions
                            .update_reconcile_status(account_id, instrument_id, ts);
                        actions.push(OmsActionV2::PersistPosition {
                            account_id,
                            instrument_id,
                        });
                    }
                }
            }
        }
        debug!(
            account_id,
            total_entries,
            spot_entries,
            position_entries = nonspot_entries,
            "processed exchange balance/position update"
        );
        if has_nonspot {
            actions.push(OmsActionV2::PublishPositionUpdate { account_id });
        }

        actions
    }

    // ------------------------------------------------------------------
    // Cleanup / recheck stubs
    // ------------------------------------------------------------------

    fn process_cleanup(&mut self, _ts_ms: i64) -> Vec<OmsActionV2> {
        // TODO: port cleanup logic (scan pending reports for stale orders)
        vec![]
    }

    fn process_position_recheck(&mut self) -> Vec<OmsActionV2> {
        // TODO: port position recheck logic
        vec![]
    }

    // ------------------------------------------------------------------
    // Public convenience builders (service layer consumes these directly)
    // ------------------------------------------------------------------

    /// Build snapshot metadata name tables from current `MetadataTables`.
    /// The result is `Arc`-shared across snapshots — rebuilt on config reload.
    #[cfg(feature = "replica")]
    pub fn build_snapshot_metadata(&self) -> crate::snapshot_v2::SnapshotMetadata {
        let meta = &self.metadata;
        let instrument_names: Vec<Box<str>> = meta
            .instruments
            .iter()
            .map(|i| meta.str_resolve(i.instrument_code_sym).into())
            .collect();
        let instrument_exch_names: Vec<Box<str>> = meta
            .instruments
            .iter()
            .map(|i| meta.str_resolve(i.instrument_exch_sym).into())
            .collect();
        let asset_names: Vec<Box<str>> = meta
            .assets
            .iter()
            .map(|a| meta.str_resolve(a.asset_sym).into())
            .collect();
        let gw_names: Vec<Box<str>> = meta
            .gws
            .iter()
            .map(|g| meta.str_resolve(g.gw_key_sym).into())
            .collect();
        // source_names covers the entire InternTable — any string ID can be a source_sym
        let source_names: Vec<Box<str>> = (0..meta.strings.len() as u32)
            .map(|id| meta.str_resolve(id).into())
            .collect();

        crate::snapshot_v2::SnapshotMetadata {
            instrument_names,
            instrument_exch_names,
            asset_names,
            gw_names,
            source_names,
        }
    }

    /// Resolve a gateway key string from a gw_id.
    pub fn resolve_gw_key(&self, gw_id: u32) -> Option<&str> {
        self.metadata
            .gw(gw_id)
            .map(|g| self.metadata.str_resolve(g.gw_key_sym))
    }

    /// Resolve an instrument code string from an instrument_id.
    pub fn resolve_instrument_code(&self, instrument_id: u32) -> &str {
        self.metadata
            .instrument(instrument_id)
            .map(|i| self.metadata.str_resolve(i.instrument_code_sym))
            .unwrap_or("")
    }

    /// Resolve an asset name string from an asset_id.
    pub fn resolve_asset_name(&self, asset_id: u32) -> &str {
        self.metadata
            .asset(asset_id)
            .map(|a| self.metadata.str_resolve(a.asset_sym))
            .unwrap_or("")
    }

    /// Build an `OrderUpdateEvent` proto from current core state.
    pub fn build_order_update_event(
        &self,
        order_id: i64,
        include_last_trade: bool,
        include_last_fee: bool,
        include_exec_message: bool,
        include_inferred_trade: bool,
    ) -> Option<OrderUpdateEvent> {
        let live = self.orders.get_live(order_id)?;
        let detail = self.orders.get_detail(order_id);
        let ts = if self.use_time_emulation {
            live.updated_at
        } else {
            gen_timestamp_ms()
        };

        let order_state = self.build_proto_order(live);
        let mut event = OrderUpdateEvent::default();
        event.order_id = order_id;
        event.account_id = live.account_id;
        event.timestamp = ts;
        event.order_snapshot = Some(order_state);
        event.order_source_id = self
            .metadata
            .strings
            .try_resolve(live.source_sym)
            .unwrap_or("")
            .to_string();

        if let Some(detail) = detail {
            if include_inferred_trade {
                event.order_inferred_trade = detail.inferred_trades.last().cloned();
            }
            if include_last_trade {
                event.last_trade = detail.trades.last().cloned();
            }
            if include_exec_message {
                event.exec_message = detail.exec_msgs.last().cloned();
            }
            if include_last_fee {
                event.last_fee = detail.fees.last().cloned();
            }
        }

        Some(event)
    }

    /// Build a `BalanceUpdateEvent` proto from current core state.
    pub fn build_balance_update_event(&self, account_id: i64) -> Option<BalanceUpdateEvent> {
        let ts = if self.use_time_emulation {
            0
        } else {
            gen_timestamp_ms()
        };
        let mut balances: Vec<Balance> = self
            .balances
            .get_balances_for_account(account_id)
            .iter()
            .map(|snap| snap.balance_state.clone())
            .collect();
        balances.extend(
            self.balances
                .get_unknown_balances_for_account(account_id)
                .iter()
                .map(|snap| snap.balance_state.clone()),
        );
        Some(BalanceUpdateEvent {
            account_id,
            balance_snapshots: balances,
            timestamp: ts,
        })
    }

    /// Build a `PositionUpdateEvent` proto from current core state.
    pub fn build_position_update_event(&self, account_id: i64) -> Option<PositionUpdateEvent> {
        let ts = if self.use_time_emulation {
            0
        } else {
            gen_timestamp_ms()
        };
        let positions: Vec<Position> = self
            .positions
            .get_positions_for_account(account_id)
            .iter()
            .map(|p| {
                let inst_code = self
                    .metadata
                    .instrument(p.instrument_id)
                    .map(|i| self.metadata.str_resolve(i.instrument_code_sym).to_string())
                    .unwrap_or_default();
                Position {
                    account_id: p.account_id,
                    instrument_code: inst_code,
                    instrument_type: p.instrument_type,
                    long_short_type: if p.is_short {
                        LongShortType::LsShort as i32
                    } else {
                        LongShortType::LsLong as i32
                    },
                    total_qty: p.qty_total,
                    frozen_qty: p.qty_frozen,
                    avail_qty: p.qty_available,
                    update_timestamp: p.last_local_update_ts,
                    sync_timestamp: p.last_exch_sync_ts,
                    ..Default::default()
                }
            })
            .collect();
        Some(PositionUpdateEvent {
            account_id,
            position_snapshots: positions,
            timestamp: ts,
        })
    }

    // ------------------------------------------------------------------
    // Materialize: v2 action -> v1 OmsAction (used by parity tests)
    // ------------------------------------------------------------------

    /// Reconstruct a full v1 OmsAction from a compact v2 action.
    /// Used by parity tests. Service layer should use the builders above directly.
    pub fn materialize_action(&self, action: &OmsActionV2) -> OmsAction {
        match action {
            OmsActionV2::SendOrderToGw {
                gw_id,
                order_id,
                order_created_at,
            } => {
                let gw_key = self
                    .metadata
                    .gw(*gw_id)
                    .map(|g| self.metadata.str_resolve(g.gw_key_sym).to_string())
                    .unwrap_or_default();
                let detail = self.orders.get_detail(*order_id);
                let request = detail
                    .and_then(|d| d.last_gw_req.as_ref())
                    .map(|r| (**r).clone())
                    .unwrap_or_default();
                OmsAction::SendOrderToGw {
                    gw_key,
                    request,
                    order_id: *order_id,
                    order_created_at: *order_created_at,
                }
            }
            OmsActionV2::SendCancelToGw { gw_id, order_id } => {
                let gw_key = self
                    .metadata
                    .gw(*gw_id)
                    .map(|g| self.metadata.str_resolve(g.gw_key_sym).to_string())
                    .unwrap_or_default();
                let detail = self.orders.get_detail(*order_id);
                let request = detail
                    .and_then(|d| d.cancel_req.as_ref())
                    .map(|r| (**r).clone())
                    .unwrap_or_default();
                OmsAction::SendCancelToGw { gw_key, request }
            }
            OmsActionV2::PersistOrder {
                order_id,
                set_expire,
                set_closed,
            } => {
                let oms_order = self.build_v1_order(*order_id);
                OmsAction::PersistOrder {
                    order: Box::new(oms_order),
                    set_expire: *set_expire,
                    set_closed: *set_closed,
                }
            }
            OmsActionV2::PublishOrderUpdate {
                order_id,
                include_last_trade,
                include_last_fee,
                include_exec_message,
                include_inferred_trade,
            } => {
                let event = self
                    .build_order_update_event(
                        *order_id,
                        *include_last_trade,
                        *include_last_fee,
                        *include_exec_message,
                        *include_inferred_trade,
                    )
                    .unwrap();
                OmsAction::PublishOrderUpdate(Box::new(event))
            }
            OmsActionV2::PublishBalanceUpdate { account_id } => {
                let event = self.build_balance_update_event(*account_id).unwrap();
                OmsAction::PublishBalanceUpdate(Box::new(event))
            }
            OmsActionV2::PublishPositionUpdate { account_id } => {
                let event = self.build_position_update_event(*account_id).unwrap();
                OmsAction::PublishPositionUpdate(Box::new(event))
            }
            OmsActionV2::PersistBalance {
                account_id,
                asset_id,
            } => {
                let snap = self
                    .balances
                    .get_balance(*account_id, *asset_id)
                    .cloned()
                    .unwrap_or_else(|| ExchBalanceSnapshot {
                        account_id: *account_id,
                        asset: String::new(),
                        symbol_exch: None,
                        balance_state: Default::default(),
                        exch_data_raw: String::new(),
                        sync_ts: 0,
                    });
                OmsAction::PersistBalance {
                    account_id: *account_id,
                    asset: snap.asset.clone(),
                    snapshot: snap,
                }
            }
            OmsActionV2::PersistPosition {
                account_id,
                instrument_id,
            } => {
                let pos = self.positions.get_position(*account_id, *instrument_id);
                let inst_code = self
                    .metadata
                    .instrument(*instrument_id)
                    .map(|i| self.metadata.str_resolve(i.instrument_code_sym).to_string())
                    .unwrap_or_default();
                let managed_pos = pos
                    .map(|p| OmsManagedPosition {
                        account_id: p.account_id,
                        instrument_code: inst_code.clone(),
                        symbol_exch: None,
                        instrument_type: p.instrument_type,
                        is_short: p.is_short,
                        qty_total: p.qty_total,
                        qty_frozen: p.qty_frozen,
                        qty_available: p.qty_available,
                        last_local_update_ts: p.last_local_update_ts,
                        last_exch_sync_ts: p.last_exch_sync_ts,
                        reconcile_status: p.reconcile_status,
                        last_exch_qty: p.last_exch_qty,
                        first_diverged_ts: p.first_diverged_ts,
                        divergence_count: p.divergence_count,
                    })
                    .unwrap_or_else(|| OmsManagedPosition::new(0, "", 0, false));
                let side = if managed_pos.is_short {
                    "SHORT"
                } else {
                    "LONG"
                };
                OmsAction::PersistPosition {
                    account_id: *account_id,
                    instrument_code: inst_code,
                    side: side.to_string(),
                    position: managed_pos,
                }
            }
            OmsActionV2::BatchSendOrdersToGw { .. } | OmsActionV2::BatchCancelToGw { .. } => {
                unimplemented!("batch materialize not yet implemented")
            }
        }
    }

    // ------------------------------------------------------------------
    // Proto/v1 building helpers
    // ------------------------------------------------------------------

    /// Build a proto Order from LiveOrder + MetadataTables.
    pub fn build_proto_order(&self, live: &LiveOrder) -> Order {
        let inst = self.metadata.instrument(live.instrument_id);
        let gw = self.metadata.gw(live.gw_id);

        Order {
            order_id: live.order_id,
            account_id: live.account_id,
            instrument: inst
                .map(|i| self.metadata.str_resolve(i.instrument_code_sym).to_string())
                .unwrap_or_default(),
            instrument_exch: inst
                .map(|i| self.metadata.str_resolve(i.instrument_exch_sym).to_string())
                .unwrap_or_default(),
            gw_key: gw
                .map(|g| self.metadata.str_resolve(g.gw_key_sym).to_string())
                .unwrap_or_default(),
            order_status: live.order_status,
            buy_sell_type: live.buy_sell_type,
            open_close_type: live.open_close_type,
            price: live.price,
            qty: live.qty,
            filled_qty: live.filled_qty,
            filled_avg_price: live.filled_avg_price,
            source_id: self
                .metadata
                .strings
                .try_resolve(live.source_sym)
                .unwrap_or("")
                .to_string(),
            exch_order_ref: live
                .exch_order_ref_id
                .map(|id| self.orders.dyn_strings.resolve(id).to_string())
                .unwrap_or_default(),
            snapshot_version: live.snapshot_version as i32,
            created_at: live.created_at,
            updated_at: live.updated_at,
            error_msg: live.error_msg.clone(),
            ..Default::default()
        }
    }

    /// Build a full v1 OmsOrder from v2 state (for PersistOrder materialization).
    fn build_v1_order(&self, order_id: i64) -> crate::models::OmsOrder {
        let live = self
            .orders
            .get_live(order_id)
            .expect("order must exist for materialize");
        let detail = self.orders.get_detail(order_id);

        crate::models::OmsOrder {
            is_from_external: live.is_external,
            order_id: live.order_id,
            account_id: live.account_id,
            exch_order_ref: live
                .exch_order_ref_id
                .map(|id| self.orders.dyn_strings.resolve(id).to_string()),
            oms_req: detail.and_then(|d| d.original_req.as_ref().map(|r| (**r).clone())),
            gw_req: detail.and_then(|d| d.last_gw_req.as_ref().map(|r| (**r).clone())),
            cancel_req: detail.and_then(|d| d.cancel_req.as_ref().map(|r| (**r).clone())),
            order_state: self.build_proto_order(live),
            trades: detail.map(|d| d.trades.clone()).unwrap_or_default(),
            acc_trades_filled_qty: live.acc_trades_filled_qty,
            acc_trades_value: live.acc_trades_value,
            order_inferred_trades: detail
                .map(|d| d.inferred_trades.clone())
                .unwrap_or_default(),
            exec_msgs: detail.map(|d| d.exec_msgs.clone()).unwrap_or_default(),
            fees: detail.map(|d| d.fees.clone()).unwrap_or_default(),
            cancel_attempts: live.cancel_attempts,
        }
    }

    // ------------------------------------------------------------------
    // V1 order conversion (warm-start)
    // ------------------------------------------------------------------

    fn convert_v1_order(&mut self, v1: &crate::models::OmsOrder) -> (LiveOrder, OrderDetailLog) {
        let instrument_id = self
            .metadata
            .resolve_instrument(&v1.order_state.instrument)
            .map(|i| i.instrument_id)
            .unwrap_or(0);
        let gw_id = self
            .metadata
            .strings
            .lookup(&v1.order_state.gw_key)
            .and_then(|sym| self.metadata.gw_by_key.get(&sym).copied())
            .unwrap_or(0);
        let route_exch_account_sym = self
            .metadata
            .route(v1.account_id)
            .map(|r| r.exch_account_sym)
            .unwrap_or(0);
        let source_sym = if v1.order_state.source_id.is_empty() {
            0
        } else {
            self.metadata.strings.intern(&v1.order_state.source_id)
        };

        let exch_ref_id = v1
            .exch_order_ref
            .as_ref()
            .map(|r| self.orders.dyn_strings.intern(r));

        let live = LiveOrder {
            order_id: v1.order_id,
            account_id: v1.account_id,
            instrument_id,
            gw_id,
            route_exch_account_sym: route_exch_account_sym,
            source_sym,
            order_status: v1.order_state.order_status,
            buy_sell_type: v1.order_state.buy_sell_type,
            open_close_type: v1.order_state.open_close_type,
            order_type: 0,
            tif_type: 0,
            price: v1.order_state.price,
            qty: v1.order_state.qty,
            filled_qty: v1.order_state.filled_qty,
            filled_avg_price: v1.order_state.filled_avg_price,
            acc_trades_filled_qty: v1.acc_trades_filled_qty,
            acc_trades_value: v1.acc_trades_value,
            exch_order_ref_id: exch_ref_id,
            snapshot_version: v1.order_state.snapshot_version as u32,
            created_at: v1.order_state.created_at,
            updated_at: v1.order_state.updated_at,
            is_external: v1.is_from_external,
            cancel_attempts: v1.cancel_attempts,
            accounting_flags: OrderAccountingFlags::default(),
            error_msg: v1.order_state.error_msg.clone(),
        };

        let detail = OrderDetailLog {
            order_id: v1.order_id,
            original_req: v1.oms_req.as_ref().map(|r| Box::new(r.clone())),
            last_gw_req: v1.gw_req.as_ref().map(|r| Box::new(r.clone())),
            cancel_req: v1.cancel_req.as_ref().map(|r| Box::new(r.clone())),
            trades: v1.trades.clone(),
            inferred_trades: v1.order_inferred_trades.clone(),
            exec_msgs: v1.exec_msgs.clone(),
            fees: v1.fees.clone(),
        };

        (live, detail)
    }
}

// ---------------------------------------------------------------------------
// DynStringTable lookup helper (mirrors order_store_v2 private helper)
// ---------------------------------------------------------------------------
