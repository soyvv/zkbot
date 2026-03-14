//! OMS parity tests — mirrors the Python test suite in
//! `libs/zk-core/src/zk_oms/tests/`.
//!
//! Scenarios covered:
//!   - Order placement → SendOrderToGw + PersistOrder
//!   - Unknown symbol → rejection update
//!   - Panic mode → order silently blocked
//!   - Linkage report → PENDING promoted to BOOKED
//!   - State report: partial fill (via unfilled_qty), IOC cancel, full fill
//!   - IOC partial fill then cancel — filled_qty preserved across reports
//!   - Inferred trade synthesised from state-only report
//!   - Cancel request on booked order → SendCancelToGw
//!   - Cancel already-terminal order → PublishOrderUpdate (error)
//!   - Gateway balance update → PublishBalanceUpdate

use std::sync::atomic::{AtomicI64, Ordering};

use zk_oms_rs::{
    config::{ConfdataManager, InstrumentTradingConfig},
    models::{OmsAction, OmsMessage},
    oms_core::OmsCore,
};
use zk_proto_rs::{
    zk::{
        common::v1::{BasicOrderType, BuySellType, InstrumentRefData, InstrumentType, LongShortType, OpenCloseType},
        exch_gw::v1::{
            BalanceUpdate, ExchangeOrderStatus, OrderIdLinkageReport, OrderReport, OrderReportEntry,
            OrderReportType, OrderStateReport, PositionReport, TradeReport,
            order_report_entry::Report,
        },
        oms::v1::{OrderCancelRequest, OrderRequest, OrderStatus, OrderUpdateEvent, PositionUpdateEvent},
    },
    ods::{GwConfigEntry, OmsConfigEntry, OmsRouteEntry},
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const ACCOUNT_1: i64 = 100;
const ACCOUNT_2: i64 = 101;
const GW_KEY_1: &str = "GW1";
const GW_KEY_2: &str = "GW2";
const EXCH_ACCOUNT_1: &str = "TEST1";
const EXCH_ACCOUNT_2: &str = "TEST2";
const EXCH_1: &str = "EX1";
const EXCH_2: &str = "EX2";
const ETH_PERP_EX1: &str = "ETH-P/USDC@EX1";
const ETH_SPOT_EX2: &str = "ETH/USD@EX2";

// ---------------------------------------------------------------------------
// ID generation
// ---------------------------------------------------------------------------

static ID_GEN: AtomicI64 = AtomicI64::new(1_000_000);

fn next_id() -> i64 {
    ID_GEN.fetch_add(1, Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Config helpers
// ---------------------------------------------------------------------------

fn test_refdata() -> Vec<InstrumentRefData> {
    vec![
        InstrumentRefData {
            instrument_id: "ETH-P/USDC@EX1".into(),
            instrument_id_exchange: "ETH".into(),
            exchange_name: EXCH_1.into(),
            instrument_type: InstrumentType::InstTypePerp as i32,
            quote_asset: "USDC".into(),
            base_asset: "ETH".into(),
            settlement_asset: "USDC".into(),
            price_precision: 2,
            qty_precision: 2,
            ..Default::default()
        },
        InstrumentRefData {
            instrument_id: "BTC-P/USDC@EX1".into(),
            instrument_id_exchange: "BTC".into(),
            exchange_name: EXCH_1.into(),
            instrument_type: InstrumentType::InstTypePerp as i32,
            quote_asset: "USDC".into(),
            base_asset: "BTC".into(),
            settlement_asset: "USDC".into(),
            price_precision: 1,
            qty_precision: 5,
            ..Default::default()
        },
        InstrumentRefData {
            instrument_id: "ETH/USD@EX2".into(),
            instrument_id_exchange: "ETH".into(),
            exchange_name: EXCH_2.into(),
            instrument_type: InstrumentType::InstTypeSpot as i32,
            quote_asset: "USD".into(),
            base_asset: "ETH".into(),
            price_precision: 1,
            qty_precision: 5,
            ..Default::default()
        },
    ]
}

fn test_routes() -> Vec<OmsRouteEntry> {
    vec![
        OmsRouteEntry {
            account_id: ACCOUNT_1,
            exch_account_id: EXCH_ACCOUNT_1.into(),
            gw_key: GW_KEY_1.into(),
            ..Default::default()
        },
        OmsRouteEntry {
            account_id: ACCOUNT_2,
            exch_account_id: EXCH_ACCOUNT_2.into(),
            gw_key: GW_KEY_2.into(),
            ..Default::default()
        },
    ]
}

fn test_gw_configs() -> Vec<GwConfigEntry> {
    vec![
        GwConfigEntry {
            exch_name: EXCH_1.into(),
            gw_key: GW_KEY_1.into(),
            cancel_required_fields: vec!["order_id".into()],
            ..Default::default()
        },
        GwConfigEntry {
            exch_name: EXCH_2.into(),
            gw_key: GW_KEY_2.into(),
            cancel_required_fields: vec!["order_id".into()],
            ..Default::default()
        },
    ]
}

fn build_config() -> ConfdataManager {
    let refdata = test_refdata();
    let tc: Vec<InstrumentTradingConfig> = refdata
        .iter()
        .map(|r| InstrumentTradingConfig::default_for(&r.instrument_id))
        .collect();
    ConfdataManager::new(
        OmsConfigEntry {
            oms_id: "test_oms".into(),
            managed_account_ids: vec![ACCOUNT_1, ACCOUNT_2],
            ..Default::default()
        },
        test_routes(),
        test_gw_configs(),
        refdata,
        tc,
    )
}

fn build_oms() -> OmsCore {
    let mut oms = OmsCore::new(build_config(), true, true, false, 1000);
    oms.init_state(vec![], vec![]);
    oms
}

// ---------------------------------------------------------------------------
// Message builders  (mirrors Python GwMessageHelper)
// ---------------------------------------------------------------------------

fn place_order(account_id: i64, symbol: &str, buy: bool, qty: f64, price: f64, ts: i64) -> OmsMessage {
    OmsMessage::PlaceOrder(OrderRequest {
        order_id: next_id(),
        account_id,
        instrument_code: symbol.into(),
        buy_sell_type: if buy { BuySellType::BsBuy as i32 } else { BuySellType::BsSell as i32 },
        open_close_type: OpenCloseType::OcOpen as i32,
        order_type: BasicOrderType::OrdertypeLimit as i32,
        price,
        qty,
        source_id: "TEST".into(),
        timestamp: ts,
        ..Default::default()
    })
}

/// Place order and return the assigned order_id.
fn place_and_get_id(oms: &mut OmsCore, account_id: i64, symbol: &str, buy: bool, qty: f64, price: f64, ts: i64) -> i64 {
    let id = ID_GEN.fetch_add(1, Ordering::Relaxed);
    oms.process_message(OmsMessage::PlaceOrder(OrderRequest {
        order_id: id,
        account_id,
        instrument_code: symbol.into(),
        buy_sell_type: if buy { BuySellType::BsBuy as i32 } else { BuySellType::BsSell as i32 },
        open_close_type: OpenCloseType::OcOpen as i32,
        order_type: BasicOrderType::OrdertypeLimit as i32,
        price,
        qty,
        source_id: "TEST".into(),
        timestamp: ts,
        ..Default::default()
    }));
    id
}

fn linkage_msg(gw_key: &str, order_id: i64, exch_ref: &str, ts: i64) -> OmsMessage {
    OmsMessage::GatewayOrderReport(OrderReport {
        exchange: gw_key.into(),
        order_id,
        exch_order_ref: exch_ref.into(),
        update_timestamp: ts,
        order_report_entries: vec![OrderReportEntry {
            report_type: OrderReportType::OrderRepTypeLinkage as i32,
            report: Some(Report::OrderIdLinkageReport(OrderIdLinkageReport {
                exch_order_ref: exch_ref.into(),
                order_id,
            })),
        }],
        ..Default::default()
    })
}

fn state_msg(
    gw_key: &str,
    order_id: i64,
    exch_ref: &str,
    status: ExchangeOrderStatus,
    filled_qty: f64,
    unfilled_qty: f64,
    avg_price: f64,
    ts: i64,
) -> OmsMessage {
    OmsMessage::GatewayOrderReport(OrderReport {
        exchange: gw_key.into(),
        order_id,
        exch_order_ref: exch_ref.into(),
        update_timestamp: ts,
        order_report_entries: vec![OrderReportEntry {
            report_type: OrderReportType::OrderRepTypeState as i32,
            report: Some(Report::OrderStateReport(OrderStateReport {
                exch_order_status: status as i32,
                filled_qty,
                unfilled_qty,
                avg_price,
                ..Default::default()
            })),
        }],
        ..Default::default()
    })
}

fn trade_msg(gw_key: &str, order_id: i64, exch_ref: &str, price: f64, qty: f64, ts: i64) -> OmsMessage {
    OmsMessage::GatewayOrderReport(OrderReport {
        exchange: gw_key.into(),
        order_id,
        exch_order_ref: exch_ref.into(),
        update_timestamp: ts,
        order_report_entries: vec![OrderReportEntry {
            report_type: OrderReportType::OrderRepTypeTrade as i32,
            report: Some(Report::TradeReport(TradeReport {
                exch_trade_id: format!("T{}", next_id()),
                filled_price: price,
                filled_qty: qty,
                ..Default::default()
            })),
        }],
        ..Default::default()
    })
}

fn cancel_msg(order_id: i64) -> OmsMessage {
    OmsMessage::CancelOrder(OrderCancelRequest { order_id, ..Default::default() })
}

// ---------------------------------------------------------------------------
// Action extractors
// ---------------------------------------------------------------------------

fn find_order_update(actions: &[OmsAction]) -> Option<&OrderUpdateEvent> {
    actions.iter().find_map(|a| match a {
        OmsAction::PublishOrderUpdate(e) => Some(e.as_ref()),
        _ => None,
    })
}

fn find_balance_update(actions: &[OmsAction]) -> Option<&PositionUpdateEvent> {
    actions.iter().find_map(|a| match a {
        OmsAction::PublishBalanceUpdate(e) => Some(e.as_ref()),
        _ => None,
    })
}

fn has_send_order(actions: &[OmsAction]) -> bool {
    actions.iter().any(|a| matches!(a, OmsAction::SendOrderToGw { .. }))
}

fn has_send_cancel(actions: &[OmsAction]) -> bool {
    actions.iter().any(|a| matches!(a, OmsAction::SendCancelToGw { .. }))
}

fn has_persist(actions: &[OmsAction]) -> bool {
    actions.iter().any(|a| matches!(a, OmsAction::PersistOrder { .. }))
}

fn snap_status(event: &OrderUpdateEvent) -> OrderStatus {
    let s = event.order_snapshot.as_ref().expect("order_snapshot missing");
    OrderStatus::try_from(s.order_status).expect("bad order_status")
}

fn snap_filled(event: &OrderUpdateEvent) -> f64 {
    event.order_snapshot.as_ref().unwrap().filled_qty
}

fn snap_avg_price(event: &OrderUpdateEvent) -> f64 {
    event.order_snapshot.as_ref().unwrap().filled_avg_price
}

// ===========================================================================
// Tests
// ===========================================================================

/// Happy-path order placement produces SendOrderToGw + PersistOrder.
#[test]
fn test_place_order_emits_send_and_persist() {
    let mut oms = build_oms();
    let actions = oms.process_message(place_order(ACCOUNT_1, ETH_PERP_EX1, true, 10.0, 2000.0, 1_000));
    assert_eq!(actions.len(), 2, "expected SendOrderToGw + PersistOrder, got {}", actions.len());
    assert!(has_send_order(&actions), "missing SendOrderToGw");
    assert!(has_persist(&actions), "missing PersistOrder");
}

/// Order for an unknown instrument is rejected immediately with a PublishOrderUpdate.
#[test]
fn test_place_order_unknown_symbol_rejected() {
    let mut oms = build_oms();
    let actions = oms.process_message(place_order(ACCOUNT_1, "UNKNOWN@EX1", true, 10.0, 2000.0, 1_001));
    let event = find_order_update(&actions).expect("expected rejection update");
    assert_eq!(snap_status(event), OrderStatus::Rejected);
}

/// Panic mode silently drops orders for the panicked account; other accounts unaffected.
#[test]
fn test_panic_mode_blocks_order() {
    let mut oms = build_oms();
    oms.process_message(OmsMessage::Panic { account_id: ACCOUNT_1 });

    let actions = oms.process_message(place_order(ACCOUNT_1, ETH_PERP_EX1, true, 10.0, 2000.0, 1_002));
    assert!(actions.is_empty(), "panic account should produce no actions");

    // Other account must still work
    let actions2 = oms.process_message(place_order(ACCOUNT_2, ETH_SPOT_EX2, true, 1.0, 2000.0, 1_002));
    assert!(has_send_order(&actions2), "non-panic account should still send");
}

/// Linkage-only report promotes order from PENDING to BOOKED.
#[test]
fn test_linkage_promotes_to_booked() {
    let mut oms = build_oms();
    let oid = place_and_get_id(&mut oms, ACCOUNT_1, ETH_PERP_EX1, true, 20.0, 1620.0, 1_003);

    let actions = oms.process_message(linkage_msg(GW_KEY_1, oid, "ref001", 1_004));
    let event = find_order_update(&actions).expect("linkage must produce order update");
    assert_eq!(snap_status(event), OrderStatus::Booked);
}

/// IOC order cancelled immediately with 0 fills.
#[test]
fn test_ioc_immediate_cancel() {
    let mut oms = build_oms();
    let oid = place_and_get_id(&mut oms, ACCOUNT_1, ETH_PERP_EX1, true, 20.0, 1620.0, 1_010);
    oms.process_message(linkage_msg(GW_KEY_1, oid, "ref010", 1_011));

    let actions = oms.process_message(state_msg(
        GW_KEY_1, oid, "ref010",
        ExchangeOrderStatus::ExchOrderStatusCancelled,
        0.0, 0.0, 0.0, 1_012,
    ));
    assert_eq!(actions.len(), 2, "cancel state: PublishOrderUpdate + PersistOrder");
    let event = find_order_update(&actions).expect("expected order update");
    assert_eq!(snap_status(event), OrderStatus::Cancelled);
    assert_eq!(snap_filled(event), 0.0);
}

/// State report with partial fill (via unfilled_qty only).
#[test]
fn test_state_report_partial_fill_from_unfilled_qty() {
    let mut oms = build_oms();
    let oid = place_and_get_id(&mut oms, ACCOUNT_1, ETH_PERP_EX1, true, 20.0, 1620.0, 1_020);
    oms.process_message(linkage_msg(GW_KEY_1, oid, "ref020", 1_021));

    // filled_qty=0, unfilled_qty=5 → inferred filled = qty - unfilled = 15
    let actions = oms.process_message(state_msg(
        GW_KEY_1, oid, "ref020",
        ExchangeOrderStatus::ExchOrderStatusPartialFilled,
        0.0, 5.0, 0.0, 1_022,
    ));
    let event = find_order_update(&actions).expect("expected order update");
    assert_eq!(snap_status(event), OrderStatus::PartiallyFilled);
    assert_eq!(snap_filled(event), 15.0);
}

/// IOC partial fill followed by cancel — filled_qty preserved across both reports.
#[test]
fn test_ioc_partial_fill_then_cancel() {
    let mut oms = build_oms();
    let oid = place_and_get_id(&mut oms, ACCOUNT_1, ETH_PERP_EX1, true, 20.0, 1620.0, 1_030);
    oms.process_message(linkage_msg(GW_KEY_1, oid, "ref030", 1_031));

    let a1 = oms.process_message(state_msg(
        GW_KEY_1, oid, "ref030",
        ExchangeOrderStatus::ExchOrderStatusPartialFilled,
        0.0, 5.0, 0.0, 1_032,
    ));
    let e1 = find_order_update(&a1).unwrap();
    assert_eq!(snap_status(e1), OrderStatus::PartiallyFilled);
    assert_eq!(snap_filled(e1), 15.0);

    // cancel with zeros — filled must be preserved from prior partial fill
    let a2 = oms.process_message(state_msg(
        GW_KEY_1, oid, "ref030",
        ExchangeOrderStatus::ExchOrderStatusCancelled,
        0.0, 0.0, 0.0, 1_033,
    ));
    let e2 = find_order_update(&a2).unwrap();
    assert_eq!(snap_status(e2), OrderStatus::Cancelled);
    assert_eq!(snap_filled(e2), 15.0, "filled_qty must survive cancel");
}

/// Partial fill state → trade report → full fill state.
#[test]
fn test_partial_fill_then_full_fill() {
    let mut oms = build_oms();
    let oid = place_and_get_id(&mut oms, ACCOUNT_1, ETH_PERP_EX1, true, 20.0, 1620.0, 1_040);
    oms.process_message(linkage_msg(GW_KEY_1, oid, "ref040", 1_041));

    let a1 = oms.process_message(state_msg(
        GW_KEY_1, oid, "ref040",
        ExchangeOrderStatus::ExchOrderStatusPartialFilled,
        0.0, 5.0, 0.0, 1_042,
    ));
    assert_eq!(snap_status(find_order_update(&a1).unwrap()), OrderStatus::PartiallyFilled);
    assert_eq!(snap_filled(find_order_update(&a1).unwrap()), 15.0);

    oms.process_message(trade_msg(GW_KEY_1, oid, "ref040", 1620.0, 15.0, 1_042));

    let a3 = oms.process_message(state_msg(
        GW_KEY_1, oid, "ref040",
        ExchangeOrderStatus::ExchOrderStatusFilled,
        20.0, 0.0, 1620.0, 1_043,
    ));
    let e3 = find_order_update(&a3).unwrap();
    assert_eq!(snap_status(e3), OrderStatus::Filled);
    assert_eq!(snap_filled(e3), 20.0);
    assert!(snap_avg_price(e3) > 0.0, "avg price must be set on fill");
}

/// State-only report (no explicit trade) must synthesise an inferred trade.
#[test]
fn test_inferred_trade_from_state_report() {
    let mut oms = build_oms();
    let oid = place_and_get_id(&mut oms, ACCOUNT_1, ETH_PERP_EX1, true, 10.0, 2000.0, 1_050);
    oms.process_message(linkage_msg(GW_KEY_1, oid, "ref050", 1_051));

    let actions = oms.process_message(state_msg(
        GW_KEY_1, oid, "ref050",
        ExchangeOrderStatus::ExchOrderStatusPartialFilled,
        5.0, 5.0, 2000.0, 1_052,
    ));
    let event = find_order_update(&actions).unwrap();
    let inferred = event.order_inferred_trade.as_ref()
        .expect("state-report fill must generate an inferred trade");
    assert_eq!(inferred.filled_qty, 5.0);
}

/// Cancel request on a booked (non-terminal) order → SendCancelToGw.
#[test]
fn test_cancel_booked_order() {
    let mut oms = build_oms();
    let oid = place_and_get_id(&mut oms, ACCOUNT_1, ETH_PERP_EX1, true, 0.2, 1620.0, 1_060);
    oms.process_message(linkage_msg(GW_KEY_1, oid, "ref060", 1_061));

    let actions = oms.process_message(cancel_msg(oid));
    assert_eq!(actions.len(), 1, "expected SendCancelToGw only");
    assert!(has_send_cancel(&actions), "missing SendCancelToGw");
}

/// Cancel request on an already-terminal order → PublishOrderUpdate (error) only.
#[test]
fn test_cancel_terminal_order_returns_error() {
    let mut oms = build_oms();
    let oid = place_and_get_id(&mut oms, ACCOUNT_1, ETH_PERP_EX1, true, 0.2, 1620.0, 1_070);
    oms.process_message(linkage_msg(GW_KEY_1, oid, "ref070", 1_071));
    oms.process_message(state_msg(
        GW_KEY_1, oid, "ref070",
        ExchangeOrderStatus::ExchOrderStatusCancelled,
        0.0, 0.2, 0.0, 1_072,
    ));

    let actions = oms.process_message(cancel_msg(oid));
    assert_eq!(actions.len(), 1, "should get exactly one action");
    assert!(
        actions.iter().any(|a| matches!(a, OmsAction::PublishOrderUpdate(_))),
        "expected PublishOrderUpdate for terminal-order cancel"
    );
}

/// Gateway balance update is merged and published as a PositionUpdateEvent.
#[test]
fn test_gateway_balance_update_publishes_position() {
    let mut oms = build_oms();

    let actions = oms.process_message(OmsMessage::BalanceUpdate(BalanceUpdate {
        balances: vec![PositionReport {
            instrument_code: "ETH".into(),         // exchange symbol for ETH-P/USDC@EX1
            instrument_type: InstrumentType::InstTypePerp as i32,
            long_short_type: LongShortType::LsLong as i32,
            exch_account_code: EXCH_ACCOUNT_1.into(),
            qty: 5.0,
            avail_qty: 5.0,
            update_timestamp: 1_080,
            ..Default::default()
        }],
    }));

    let pue = find_balance_update(&actions).expect("expected PublishBalanceUpdate");
    assert_eq!(pue.account_id, ACCOUNT_1);
    assert!(!pue.position_snapshots.is_empty(), "position snapshot must not be empty");
}
