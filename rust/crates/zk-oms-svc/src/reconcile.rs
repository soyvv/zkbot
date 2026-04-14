//! Order reconciliation logic for startup and periodic recheck.
//!
//! Compares OMS open-order state against gateway query results and produces
//! synthetic `OrderReport` messages to drive convergence through `OmsCoreV2`.
//!
//! # Two-phase reconciliation
//!
//! Phase 1 (`reconcile_orders_against_gateway`): compare OMS open orders against
//! gateway `QueryOpenOrders` result. Orders present in both are checked for state
//! divergence. Orders absent from the gateway set are returned as
//! `needs_detail_query` — they require a follow-up `QueryOrderDetails` call to
//! determine the actual terminal state (Filled, Cancelled, Rejected, etc.).
//!
//! Phase 2 (`resolve_missing_order`): called by the service layer per order after
//! `QueryOrderDetails`. If the gateway returns detail, the actual terminal state is
//! used. If the gateway returns nothing, a fallback synthetic cancel is emitted.

use std::collections::HashMap;

use tracing::info;

use zk_proto_rs::zk::common::v1::{Rejection, RejectionReason, RejectionSource};
use zk_proto_rs::zk::exch_gw::v1::{
    order_report_entry, ExchExecType, ExchangeOrderStatus, ExecReport, OrderReport,
    OrderReportEntry, OrderReportType, OrderSourceType, OrderStateReport,
};
use zk_proto_rs::zk::gateway::v1::ExchOrder;

use zk_oms_rs::oms_core_v2::OmsCoreV2;
use zk_oms_rs::order_mgr::map_order_status;

// ── Snapshot ────────────────────────────────────────────────────────────────

/// Lightweight snapshot of an OMS open order for reconciliation.
#[derive(Debug, Clone)]
pub struct OmsOrderSnapshot {
    pub order_id: i64,
    pub account_id: i64,
    pub gw_key: String,
    pub exch_order_ref: Option<String>,
    pub instrument: String,
    pub order_status: i32,
    pub filled_qty: f64,
}

/// Extract snapshots of all non-terminal orders from OmsCoreV2.
pub fn extract_open_order_snapshots(core: &OmsCoreV2) -> Vec<OmsOrderSnapshot> {
    let mut snapshots = Vec::new();
    for (oid, live) in &core.orders.live {
        if live.is_in_terminal_state() {
            continue;
        }
        let gw_key = if let Some(gw_meta) = core.metadata.gws.get(live.gw_id as usize) {
            core.metadata.strings.resolve(gw_meta.gw_key_sym).to_owned()
        } else {
            continue; // unknown gateway, skip
        };
        let exch_order_ref = live
            .exch_order_ref_id
            .and_then(|id| core.orders.dyn_strings.try_resolve(id))
            .map(|s| s.to_owned());
        let instrument =
            if let Some(inst) = core.metadata.instruments.get(live.instrument_id as usize) {
                core.metadata
                    .strings
                    .resolve(inst.instrument_code_sym)
                    .to_owned()
            } else {
                continue;
            };
        snapshots.push(OmsOrderSnapshot {
            order_id: *oid,
            account_id: live.account_id,
            gw_key,
            exch_order_ref,
            instrument,
            order_status: live.order_status,
            filled_qty: live.filled_qty,
        });
    }
    snapshots
}

// ── Reconciliation result ───────────────────────────────────────────────────

/// Per-gateway reconciliation statistics.
#[derive(Debug, Default)]
pub struct GwReconcileResult {
    pub gw_key: String,
    pub orders_matched: usize,
    pub orders_updated: usize,
    pub orders_need_detail: usize,
    pub orders_rejected: usize,
}

// ── Core convergence (phase 1) ──────────────────────────────────────────────

/// Compare OMS open orders against a gateway's open-order query result.
///
/// Returns:
/// - `reports`: synthetic `OrderReport`s for immediately resolvable cases
///   (state divergence on live orders, no-exch-ref rejects).
/// - `needs_detail_query`: orders that are absent from gateway open orders and
///   require a follow-up `QueryOrderDetails` to determine actual terminal state.
pub fn reconcile_orders_against_gateway(
    oms_open_orders: &[OmsOrderSnapshot],
    gw_open_orders: &[ExchOrder],
    gw_key: &str,
    now_ms: i64,
) -> (Vec<OrderReport>, Vec<OmsOrderSnapshot>, GwReconcileResult) {
    let mut result = GwReconcileResult {
        gw_key: gw_key.to_string(),
        ..Default::default()
    };

    // Index gateway open orders by exch_order_ref.
    let gw_index: HashMap<&str, &ExchOrder> = gw_open_orders
        .iter()
        .map(|o| (o.order_ref.as_str(), o))
        .collect();

    let mut reports = Vec::new();
    let mut needs_detail = Vec::new();

    for snap in oms_open_orders {
        if snap.gw_key != gw_key {
            continue;
        }

        match &snap.exch_order_ref {
            Some(ref exch_ref) if !exch_ref.is_empty() => {
                if let Some(gw_order) = gw_index.get(exch_ref.as_str()) {
                    // Order is live on exchange — check for state divergence.
                    result.orders_matched += 1;
                    let gw_state = extract_gw_order_state(gw_order);
                    if let Some((filled_qty, unfilled_qty, avg_price, gw_exch_status)) = gw_state {
                        let fill_diverged = (filled_qty - snap.filled_qty).abs() > 1e-12;
                        // Map gateway ExchangeOrderStatus → OMS OrderStatus before
                        // comparing. These are different enum domains with different
                        // integer values (e.g. ExchBooked=1 vs OmsBooked=2).
                        let mapped_oms_status = map_order_status(
                            ExchangeOrderStatus::try_from(gw_exch_status)
                                .unwrap_or(ExchangeOrderStatus::ExchOrderStatusUnspecified),
                        ) as i32;
                        let status_diverged = mapped_oms_status != snap.order_status;
                        if fill_diverged || status_diverged {
                            reports.push(make_synthetic_state_report(
                                snap.order_id,
                                exch_ref,
                                snap.account_id,
                                gw_exch_status,
                                filled_qty,
                                unfilled_qty,
                                avg_price,
                                now_ms,
                            ));
                            result.orders_updated += 1;
                        }
                    }
                } else {
                    // Had exch_order_ref but gone from open orders — needs detail
                    // query to determine actual terminal state.
                    info!(
                        order_id = snap.order_id,
                        exch_ref, "reconcile: order absent from open orders, querying details"
                    );
                    needs_detail.push(snap.clone());
                    result.orders_need_detail += 1;
                }
            }
            _ => {
                // No exch_order_ref — never reached venue. Synthetically reject.
                info!(
                    order_id = snap.order_id,
                    "reconcile: pending order has no exch_order_ref, rejecting"
                );
                reports.push(make_synthetic_reject_report(
                    snap.order_id,
                    snap.account_id,
                    "reconcile: order never reached exchange (no exch_order_ref after restart)",
                    now_ms,
                ));
                result.orders_rejected += 1;
            }
        }
    }

    (reports, needs_detail, result)
}

// ── Phase 2: resolve missing orders ─────────────────────────────────────────

/// Resolve a single order that was absent from gateway open orders.
///
/// `detail_orders` is the result of `QueryOrderDetails` for this order's
/// `exch_order_ref`. If the gateway returned order detail, we use the actual
/// terminal state. If empty (gateway has no record), fall back to synthetic
/// cancel with the OMS's last known fill state.
pub fn resolve_missing_order(
    snap: &OmsOrderSnapshot,
    detail_orders: &[ExchOrder],
    now_ms: i64,
) -> OrderReport {
    let exch_ref = snap.exch_order_ref.as_deref().unwrap_or("");

    // Try to find the order in the detail response.
    for detail in detail_orders {
        if detail.order_ref == exch_ref {
            if let Some((filled_qty, unfilled_qty, avg_price, status)) =
                extract_gw_order_state(&detail)
            {
                info!(
                    order_id = snap.order_id,
                    exch_ref,
                    status,
                    filled_qty,
                    "reconcile: resolved missing order from detail query"
                );
                return make_synthetic_state_report(
                    snap.order_id,
                    exch_ref,
                    snap.account_id,
                    status,
                    filled_qty,
                    unfilled_qty,
                    avg_price,
                    now_ms,
                );
            }
        }
    }

    // Fallback: gateway has no record either. Use synthetic cancel with OMS's
    // last known fill state. This is the least-information path.
    info!(
        order_id = snap.order_id,
        exch_ref, "reconcile: no detail found, falling back to synthetic cancel"
    );
    make_synthetic_state_report(
        snap.order_id,
        exch_ref,
        snap.account_id,
        ExchangeOrderStatus::ExchOrderStatusCancelled as i32,
        snap.filled_qty,
        0.0,
        0.0,
        now_ms,
    )
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Extract filled_qty, unfilled_qty, avg_price, status from a gateway ExchOrder.
fn extract_gw_order_state(order: &ExchOrder) -> Option<(f64, f64, f64, i32)> {
    let report = order.order_report.as_ref()?;
    for entry in &report.order_report_entries {
        if entry.report_type == OrderReportType::OrderRepTypeState as i32 {
            if let Some(order_report_entry::Report::OrderStateReport(s)) = &entry.report {
                return Some((
                    s.filled_qty,
                    s.unfilled_qty,
                    s.avg_price,
                    s.exch_order_status,
                ));
            }
        }
    }
    None
}

pub fn make_synthetic_state_report(
    order_id: i64,
    exch_order_ref: &str,
    account_id: i64,
    status: i32,
    filled_qty: f64,
    unfilled_qty: f64,
    avg_price: f64,
    ts_ms: i64,
) -> OrderReport {
    let state_report = OrderStateReport {
        exch_order_status: status,
        filled_qty,
        unfilled_qty,
        avg_price,
        ..Default::default()
    };
    OrderReport {
        exch_order_ref: exch_order_ref.to_string(),
        order_id,
        account_id,
        update_timestamp: ts_ms,
        order_source_type: OrderSourceType::OrderSourceTq as i32,
        order_report_entries: vec![OrderReportEntry {
            report_type: OrderReportType::OrderRepTypeState as i32,
            report: Some(order_report_entry::Report::OrderStateReport(state_report)),
        }],
        ..Default::default()
    }
}

fn make_synthetic_reject_report(
    order_id: i64,
    account_id: i64,
    reason: &str,
    ts_ms: i64,
) -> OrderReport {
    let exec_report = ExecReport {
        exec_id: format!("reconcile-reject-{order_id}"),
        exec_type: ExchExecType::Rejected as i32,
        exec_message: reason.to_string(),
        rejection_info: Some(Rejection {
            reason: RejectionReason::RejReasonExchReject as i32,
            source: RejectionSource::ExchReject as i32,
            error_message: reason.to_string(),
            ..Default::default()
        }),
        ..Default::default()
    };
    OrderReport {
        order_id,
        account_id,
        update_timestamp: ts_ms,
        order_source_type: OrderSourceType::OrderSourceTq as i32,
        order_report_entries: vec![OrderReportEntry {
            report_type: OrderReportType::OrderRepTypeExec as i32,
            report: Some(order_report_entry::Report::ExecReport(exec_report)),
        }],
        ..Default::default()
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_snapshot(
        order_id: i64,
        gw_key: &str,
        exch_ref: Option<&str>,
        filled_qty: f64,
    ) -> OmsOrderSnapshot {
        OmsOrderSnapshot {
            order_id,
            account_id: 1000,
            gw_key: gw_key.to_string(),
            exch_order_ref: exch_ref.map(String::from),
            instrument: "BTC/USDT@SIM".to_string(),
            order_status: 1, // Pending
            filled_qty,
        }
    }

    fn make_snapshot_with_status(
        order_id: i64,
        gw_key: &str,
        exch_ref: Option<&str>,
        filled_qty: f64,
        order_status: i32,
    ) -> OmsOrderSnapshot {
        OmsOrderSnapshot {
            order_id,
            account_id: 1000,
            gw_key: gw_key.to_string(),
            exch_order_ref: exch_ref.map(String::from),
            instrument: "BTC/USDT@SIM".to_string(),
            order_status,
            filled_qty,
        }
    }

    fn make_gw_open_order(
        exch_ref: &str,
        filled_qty: f64,
        unfilled_qty: f64,
        status: i32,
    ) -> ExchOrder {
        ExchOrder {
            order_ref: exch_ref.to_string(),
            order_report: Some(OrderReport {
                exch_order_ref: exch_ref.to_string(),
                order_report_entries: vec![OrderReportEntry {
                    report_type: OrderReportType::OrderRepTypeState as i32,
                    report: Some(order_report_entry::Report::OrderStateReport(
                        OrderStateReport {
                            exch_order_status: status,
                            filled_qty,
                            unfilled_qty,
                            avg_price: 50000.0,
                            ..Default::default()
                        },
                    )),
                }],
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn test_order_live_no_change() {
        use zk_proto_rs::zk::oms::v1::OrderStatus;
        // OMS and GW agree on both status and fill qty — no report emitted.
        // OMS stores OrderStatus::PartiallyFilled (=3), GW reports
        // ExchOrderStatusPartialFilled (=2). After mapping, they match.
        let snapshots = vec![make_snapshot_with_status(
            1,
            "gw1",
            Some("exch-1"),
            0.5,
            OrderStatus::PartiallyFilled as i32,
        )];
        let gw_orders = vec![make_gw_open_order(
            "exch-1",
            0.5,
            0.5,
            ExchangeOrderStatus::ExchOrderStatusPartialFilled as i32,
        )];
        let (reports, needs_detail, result) =
            reconcile_orders_against_gateway(&snapshots, &gw_orders, "gw1", 100);
        assert!(reports.is_empty(), "no reports when state matches");
        assert!(needs_detail.is_empty());
        assert_eq!(result.orders_matched, 1);
        assert_eq!(result.orders_updated, 0);
    }

    #[test]
    fn test_order_live_fill_update() {
        let snapshots = vec![make_snapshot(1, "gw1", Some("exch-1"), 0.3)];
        let gw_orders = vec![make_gw_open_order(
            "exch-1",
            0.7,
            0.3,
            ExchangeOrderStatus::ExchOrderStatusPartialFilled as i32,
        )];
        let (reports, _, result) =
            reconcile_orders_against_gateway(&snapshots, &gw_orders, "gw1", 100);
        assert_eq!(reports.len(), 1, "should emit state update");
        assert_eq!(result.orders_matched, 1);
        assert_eq!(result.orders_updated, 1);

        // Verify the report carries the updated fill qty.
        let entry = &reports[0].order_report_entries[0];
        if let Some(order_report_entry::Report::OrderStateReport(s)) = &entry.report {
            assert!((s.filled_qty - 0.7).abs() < 1e-12);
        } else {
            panic!("expected OrderStateReport");
        }
    }

    #[test]
    fn test_pending_to_booked_status_divergence() {
        // OMS has Pending (OrderStatus::Pending=1), GW has Booked
        // (ExchOrderStatusBooked=1). These have the SAME integer value but
        // represent DIFFERENT states. After mapping through map_order_status,
        // GW Booked → OMS Booked (=2) ≠ OMS Pending (=1), so we MUST emit.
        use zk_proto_rs::zk::oms::v1::OrderStatus;

        let snapshots = vec![make_snapshot_with_status(
            1,
            "gw1",
            Some("exch-1"),
            0.0,
            OrderStatus::Pending as i32, // = 1
        )];
        let gw_orders = vec![make_gw_open_order(
            "exch-1",
            0.0,
            1.0,
            ExchangeOrderStatus::ExchOrderStatusBooked as i32, // = 1
        )];
        let (reports, _, result) =
            reconcile_orders_against_gateway(&snapshots, &gw_orders, "gw1", 100);
        assert_eq!(result.orders_matched, 1);
        assert_eq!(
            reports.len(),
            1,
            "Pending→Booked must emit update even though raw ints are both 1"
        );
        assert_eq!(result.orders_updated, 1);
    }

    #[test]
    fn test_no_false_positive_across_enum_domains() {
        // OMS has Booked (OrderStatus::Booked=2), GW has Booked
        // (ExchOrderStatusBooked=1). Raw ints differ (2 vs 1), but after
        // mapping they are the same. Must NOT emit a spurious update.
        use zk_proto_rs::zk::oms::v1::OrderStatus;

        let snapshots = vec![make_snapshot_with_status(
            1,
            "gw1",
            Some("exch-1"),
            0.5,
            OrderStatus::Booked as i32, // = 2
        )];
        let gw_orders = vec![make_gw_open_order(
            "exch-1",
            0.5,
            0.5,
            ExchangeOrderStatus::ExchOrderStatusBooked as i32, // = 1
        )];
        let (reports, _, result) =
            reconcile_orders_against_gateway(&snapshots, &gw_orders, "gw1", 100);
        assert!(
            reports.is_empty(),
            "same semantic status (Booked) must NOT emit update despite different raw ints"
        );
        assert_eq!(result.orders_matched, 1);
        assert_eq!(result.orders_updated, 0);
    }

    #[test]
    fn test_no_false_positive_partial_filled() {
        // Same test for PartiallyFilled: OMS=3, GW ExchPartialFilled=2.
        use zk_proto_rs::zk::oms::v1::OrderStatus;

        let snapshots = vec![make_snapshot_with_status(
            1,
            "gw1",
            Some("exch-1"),
            0.5,
            OrderStatus::PartiallyFilled as i32, // = 3
        )];
        let gw_orders = vec![make_gw_open_order(
            "exch-1",
            0.5,
            0.5,
            ExchangeOrderStatus::ExchOrderStatusPartialFilled as i32, // = 2
        )];
        let (reports, _, result) =
            reconcile_orders_against_gateway(&snapshots, &gw_orders, "gw1", 100);
        assert!(
            reports.is_empty(),
            "same semantic status (PartiallyFilled) must NOT emit update"
        );
        assert_eq!(result.orders_matched, 1);
        assert_eq!(result.orders_updated, 0);
    }

    #[test]
    fn test_order_missing_returns_needs_detail() {
        // Order has exch_order_ref but is absent from open orders —
        // should NOT immediately cancel, but return as needs_detail_query.
        let snapshots = vec![make_snapshot(1, "gw1", Some("exch-1"), 0.0)];
        let gw_orders: Vec<ExchOrder> = vec![]; // order gone from exchange
        let (reports, needs_detail, result) =
            reconcile_orders_against_gateway(&snapshots, &gw_orders, "gw1", 100);
        assert!(reports.is_empty(), "should NOT immediately terminate");
        assert_eq!(needs_detail.len(), 1);
        assert_eq!(needs_detail[0].order_id, 1);
        assert_eq!(result.orders_need_detail, 1);
    }

    #[test]
    fn test_resolve_missing_with_detail() {
        // Gateway detail query returns the order with actual terminal state (Filled).
        let snap = make_snapshot(1, "gw1", Some("exch-1"), 0.3);
        let detail = vec![make_gw_open_order(
            "exch-1",
            1.0,
            0.0,
            ExchangeOrderStatus::ExchOrderStatusFilled as i32,
        )];
        let report = resolve_missing_order(&snap, &detail, 100);
        let entry = &report.order_report_entries[0];
        if let Some(order_report_entry::Report::OrderStateReport(s)) = &entry.report {
            assert_eq!(
                s.exch_order_status,
                ExchangeOrderStatus::ExchOrderStatusFilled as i32,
                "should use actual Filled status, not Cancelled"
            );
            assert!(
                (s.filled_qty - 1.0).abs() < 1e-12,
                "should use actual fill qty"
            );
        } else {
            panic!("expected OrderStateReport");
        }
    }

    #[test]
    fn test_resolve_missing_no_detail_fallback() {
        // Gateway detail query returned nothing — fallback to synthetic cancel.
        let snap = make_snapshot(1, "gw1", Some("exch-1"), 0.5);
        let detail: Vec<ExchOrder> = vec![];
        let report = resolve_missing_order(&snap, &detail, 100);
        let entry = &report.order_report_entries[0];
        if let Some(order_report_entry::Report::OrderStateReport(s)) = &entry.report {
            assert_eq!(
                s.exch_order_status,
                ExchangeOrderStatus::ExchOrderStatusCancelled as i32,
            );
            // Uses OMS's last known fill qty since we have no better data.
            assert!((s.filled_qty - 0.5).abs() < 1e-12);
        } else {
            panic!("expected OrderStateReport");
        }
    }

    #[test]
    fn test_order_missing_no_exch_ref() {
        let snapshots = vec![make_snapshot(1, "gw1", None, 0.0)];
        let gw_orders: Vec<ExchOrder> = vec![];
        let (reports, needs_detail, result) =
            reconcile_orders_against_gateway(&snapshots, &gw_orders, "gw1", 100);
        assert_eq!(reports.len(), 1);
        assert!(needs_detail.is_empty());
        assert_eq!(result.orders_rejected, 1);

        // Should be an EXEC report with Rejected type.
        let entry = &reports[0].order_report_entries[0];
        assert_eq!(entry.report_type, OrderReportType::OrderRepTypeExec as i32);
        if let Some(order_report_entry::Report::ExecReport(e)) = &entry.report {
            assert_eq!(e.exec_type, ExchExecType::Rejected as i32);
        } else {
            panic!("expected ExecReport");
        }
    }

    #[test]
    fn test_filters_by_gateway() {
        let snapshots = vec![
            make_snapshot(1, "gw1", Some("exch-1"), 0.0),
            make_snapshot(2, "gw2", Some("exch-2"), 0.0),
        ];
        let gw_orders: Vec<ExchOrder> = vec![]; // nothing on gw1
        let (reports, needs_detail, _result) =
            reconcile_orders_against_gateway(&snapshots, &gw_orders, "gw1", 100);
        // Only order 1 (gw1) should be reconciled, order 2 (gw2) skipped.
        assert!(reports.is_empty());
        assert_eq!(needs_detail.len(), 1);
        assert_eq!(needs_detail[0].order_id, 1);
    }

    #[test]
    fn test_snapshot_instrument_populated() {
        // Verify that OmsOrderSnapshot.instrument is populated and can be used
        // for SingleOrderQuery.symbol in the service layer.
        let snap = make_snapshot(1, "gw1", Some("exch-1"), 0.0);
        assert_eq!(snap.instrument, "BTC/USDT@SIM");
        assert!(
            !snap.instrument.is_empty(),
            "instrument must be non-empty for venue queries"
        );
    }

    #[test]
    fn test_empty_exch_ref_treated_as_missing() {
        // Some("") should be treated the same as None — synthetic reject.
        let snapshots = vec![make_snapshot(1, "gw1", Some(""), 0.0)];
        let gw_orders: Vec<ExchOrder> = vec![];
        let (reports, needs_detail, result) =
            reconcile_orders_against_gateway(&snapshots, &gw_orders, "gw1", 100);
        assert_eq!(reports.len(), 1);
        assert!(needs_detail.is_empty());
        assert_eq!(result.orders_rejected, 1);
        let entry = &reports[0].order_report_entries[0];
        assert_eq!(entry.report_type, OrderReportType::OrderRepTypeExec as i32);
    }
}
