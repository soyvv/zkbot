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
//!   - Gateway balance update (spot) → PublishBalanceUpdate
//!   - Gateway position update (perp) → PublishPositionUpdate
//!   - Gateway mixed update → both PublishBalanceUpdate + PublishPositionUpdate
//!   - PositionManager: freeze, delta, sign-flip, reconcile
//!   - ReservationManager: reserve, release, check_available
//!   - BalanceManager: exchange-only storage

use std::sync::atomic::{AtomicI64, Ordering};

use zk_oms_rs::{
    config::{ConfdataManager, InstrumentTradingConfig},
    models::{OmsAction, OmsManagedPosition, OmsMessage, PositionDelta, ReconcileStatus},
    oms_core::OmsCore,
    position_mgr::PositionManager,
    reservation_mgr::ReservationManager,
};
use zk_proto_rs::{
    ods::{GwConfigEntry, OmsConfigEntry, OmsRouteEntry},
    zk::{
        common::v1::{
            BasicOrderType, BuySellType, InstrumentRefData, InstrumentType, LongShortType,
            OpenCloseType,
        },
        exch_gw::v1::{
            order_report_entry::Report, BalanceUpdate, ExchangeOrderStatus, OrderIdLinkageReport,
            OrderReport, OrderReportEntry, OrderReportType, OrderStateReport, PositionReport,
            TradeReport,
        },
        oms::v1::{
            BalanceUpdateEvent, OrderCancelRequest, OrderRequest, OrderStatus, OrderUpdateEvent,
            PositionUpdateEvent,
        },
    },
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
    oms.init_state(vec![], vec![], vec![]);
    oms
}

// ---------------------------------------------------------------------------
// Message builders  (mirrors Python GwMessageHelper)
// ---------------------------------------------------------------------------

fn place_order(
    account_id: i64,
    symbol: &str,
    buy: bool,
    qty: f64,
    price: f64,
    ts: i64,
) -> OmsMessage {
    OmsMessage::PlaceOrder(OrderRequest {
        order_id: next_id(),
        account_id,
        instrument_code: symbol.into(),
        buy_sell_type: if buy {
            BuySellType::BsBuy as i32
        } else {
            BuySellType::BsSell as i32
        },
        open_close_type: OpenCloseType::OcOpen as i32,
        order_type: BasicOrderType::OrdertypeLimit as i32,
        price,
        qty,
        source_id: "TEST".into(),
        timestamp: ts,
        ..Default::default()
    })
}

fn place_and_get_id(
    oms: &mut OmsCore,
    account_id: i64,
    symbol: &str,
    buy: bool,
    qty: f64,
    price: f64,
    ts: i64,
) -> i64 {
    let id = ID_GEN.fetch_add(1, Ordering::Relaxed);
    oms.process_message(OmsMessage::PlaceOrder(OrderRequest {
        order_id: id,
        account_id,
        instrument_code: symbol.into(),
        buy_sell_type: if buy {
            BuySellType::BsBuy as i32
        } else {
            BuySellType::BsSell as i32
        },
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

fn trade_msg(
    gw_key: &str,
    order_id: i64,
    exch_ref: &str,
    price: f64,
    qty: f64,
    ts: i64,
) -> OmsMessage {
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
    OmsMessage::CancelOrder(OrderCancelRequest {
        order_id,
        ..Default::default()
    })
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

fn find_balance_update(actions: &[OmsAction]) -> Option<&BalanceUpdateEvent> {
    actions.iter().find_map(|a| match a {
        OmsAction::PublishBalanceUpdate(e) => Some(e.as_ref()),
        _ => None,
    })
}

fn find_position_update(actions: &[OmsAction]) -> Option<&PositionUpdateEvent> {
    actions.iter().find_map(|a| match a {
        OmsAction::PublishPositionUpdate(e) => Some(e.as_ref()),
        _ => None,
    })
}

fn has_send_order(actions: &[OmsAction]) -> bool {
    actions
        .iter()
        .any(|a| matches!(a, OmsAction::SendOrderToGw { .. }))
}

fn has_send_cancel(actions: &[OmsAction]) -> bool {
    actions
        .iter()
        .any(|a| matches!(a, OmsAction::SendCancelToGw { .. }))
}

fn has_persist(actions: &[OmsAction]) -> bool {
    actions
        .iter()
        .any(|a| matches!(a, OmsAction::PersistOrder { .. }))
}

fn snap_status(event: &OrderUpdateEvent) -> OrderStatus {
    let s = event
        .order_snapshot
        .as_ref()
        .expect("order_snapshot missing");
    OrderStatus::try_from(s.order_status).expect("bad order_status")
}

fn snap_filled(event: &OrderUpdateEvent) -> f64 {
    event.order_snapshot.as_ref().unwrap().filled_qty
}

fn snap_avg_price(event: &OrderUpdateEvent) -> f64 {
    event.order_snapshot.as_ref().unwrap().filled_avg_price
}

// ===========================================================================
// Original parity tests
// ===========================================================================

#[test]
fn test_place_order_emits_send_and_persist() {
    let mut oms = build_oms();
    let actions = oms.process_message(place_order(
        ACCOUNT_1,
        ETH_PERP_EX1,
        true,
        10.0,
        2000.0,
        1_000,
    ));
    assert_eq!(
        actions.len(),
        2,
        "expected SendOrderToGw + PersistOrder, got {}",
        actions.len()
    );
    assert!(has_send_order(&actions), "missing SendOrderToGw");
    assert!(has_persist(&actions), "missing PersistOrder");
}

#[test]
fn test_place_order_unknown_symbol_rejected() {
    let mut oms = build_oms();
    let actions = oms.process_message(place_order(
        ACCOUNT_1,
        "UNKNOWN@EX1",
        true,
        10.0,
        2000.0,
        1_001,
    ));
    let event = find_order_update(&actions).expect("expected rejection update");
    assert_eq!(snap_status(event), OrderStatus::Rejected);
}

#[test]
fn test_panic_mode_blocks_order() {
    let mut oms = build_oms();
    oms.process_message(OmsMessage::Panic {
        account_id: ACCOUNT_1,
    });

    let actions = oms.process_message(place_order(
        ACCOUNT_1,
        ETH_PERP_EX1,
        true,
        10.0,
        2000.0,
        1_002,
    ));
    assert!(
        actions.is_empty(),
        "panic account should produce no actions"
    );

    let actions2 = oms.process_message(place_order(
        ACCOUNT_2,
        ETH_SPOT_EX2,
        true,
        1.0,
        2000.0,
        1_002,
    ));
    assert!(
        has_send_order(&actions2),
        "non-panic account should still send"
    );
}

#[test]
fn test_linkage_promotes_to_booked() {
    let mut oms = build_oms();
    let oid = place_and_get_id(&mut oms, ACCOUNT_1, ETH_PERP_EX1, true, 20.0, 1620.0, 1_003);

    let actions = oms.process_message(linkage_msg(GW_KEY_1, oid, "ref001", 1_004));
    let event = find_order_update(&actions).expect("linkage must produce order update");
    assert_eq!(snap_status(event), OrderStatus::Booked);
}

#[test]
fn test_ioc_immediate_cancel() {
    let mut oms = build_oms();
    let oid = place_and_get_id(&mut oms, ACCOUNT_1, ETH_PERP_EX1, true, 20.0, 1620.0, 1_010);
    oms.process_message(linkage_msg(GW_KEY_1, oid, "ref010", 1_011));

    let actions = oms.process_message(state_msg(
        GW_KEY_1,
        oid,
        "ref010",
        ExchangeOrderStatus::ExchOrderStatusCancelled,
        0.0,
        0.0,
        0.0,
        1_012,
    ));
    assert_eq!(
        actions.len(),
        2,
        "cancel state: PublishOrderUpdate + PersistOrder"
    );
    let event = find_order_update(&actions).expect("expected order update");
    assert_eq!(snap_status(event), OrderStatus::Cancelled);
    assert_eq!(snap_filled(event), 0.0);
}

#[test]
fn test_state_report_partial_fill_from_unfilled_qty() {
    let mut oms = build_oms();
    let oid = place_and_get_id(&mut oms, ACCOUNT_1, ETH_PERP_EX1, true, 20.0, 1620.0, 1_020);
    oms.process_message(linkage_msg(GW_KEY_1, oid, "ref020", 1_021));

    let actions = oms.process_message(state_msg(
        GW_KEY_1,
        oid,
        "ref020",
        ExchangeOrderStatus::ExchOrderStatusPartialFilled,
        0.0,
        5.0,
        0.0,
        1_022,
    ));
    let event = find_order_update(&actions).expect("expected order update");
    assert_eq!(snap_status(event), OrderStatus::PartiallyFilled);
    assert_eq!(snap_filled(event), 15.0);
}

#[test]
fn test_ioc_partial_fill_then_cancel() {
    let mut oms = build_oms();
    let oid = place_and_get_id(&mut oms, ACCOUNT_1, ETH_PERP_EX1, true, 20.0, 1620.0, 1_030);
    oms.process_message(linkage_msg(GW_KEY_1, oid, "ref030", 1_031));

    let a1 = oms.process_message(state_msg(
        GW_KEY_1,
        oid,
        "ref030",
        ExchangeOrderStatus::ExchOrderStatusPartialFilled,
        0.0,
        5.0,
        0.0,
        1_032,
    ));
    let e1 = find_order_update(&a1).unwrap();
    assert_eq!(snap_status(e1), OrderStatus::PartiallyFilled);
    assert_eq!(snap_filled(e1), 15.0);

    let a2 = oms.process_message(state_msg(
        GW_KEY_1,
        oid,
        "ref030",
        ExchangeOrderStatus::ExchOrderStatusCancelled,
        0.0,
        0.0,
        0.0,
        1_033,
    ));
    let e2 = find_order_update(&a2).unwrap();
    assert_eq!(snap_status(e2), OrderStatus::Cancelled);
    assert_eq!(snap_filled(e2), 15.0, "filled_qty must survive cancel");
}

#[test]
fn test_partial_fill_then_full_fill() {
    let mut oms = build_oms();
    let oid = place_and_get_id(&mut oms, ACCOUNT_1, ETH_PERP_EX1, true, 20.0, 1620.0, 1_040);
    oms.process_message(linkage_msg(GW_KEY_1, oid, "ref040", 1_041));

    let a1 = oms.process_message(state_msg(
        GW_KEY_1,
        oid,
        "ref040",
        ExchangeOrderStatus::ExchOrderStatusPartialFilled,
        0.0,
        5.0,
        0.0,
        1_042,
    ));
    assert_eq!(
        snap_status(find_order_update(&a1).unwrap()),
        OrderStatus::PartiallyFilled
    );
    assert_eq!(snap_filled(find_order_update(&a1).unwrap()), 15.0);

    oms.process_message(trade_msg(GW_KEY_1, oid, "ref040", 1620.0, 15.0, 1_042));

    let a3 = oms.process_message(state_msg(
        GW_KEY_1,
        oid,
        "ref040",
        ExchangeOrderStatus::ExchOrderStatusFilled,
        20.0,
        0.0,
        1620.0,
        1_043,
    ));
    let e3 = find_order_update(&a3).unwrap();
    assert_eq!(snap_status(e3), OrderStatus::Filled);
    assert_eq!(snap_filled(e3), 20.0);
    assert!(snap_avg_price(e3) > 0.0, "avg price must be set on fill");
}

#[test]
fn test_inferred_trade_from_state_report() {
    let mut oms = build_oms();
    let oid = place_and_get_id(&mut oms, ACCOUNT_1, ETH_PERP_EX1, true, 10.0, 2000.0, 1_050);
    oms.process_message(linkage_msg(GW_KEY_1, oid, "ref050", 1_051));

    let actions = oms.process_message(state_msg(
        GW_KEY_1,
        oid,
        "ref050",
        ExchangeOrderStatus::ExchOrderStatusPartialFilled,
        5.0,
        5.0,
        2000.0,
        1_052,
    ));
    let event = find_order_update(&actions).unwrap();
    let inferred = event
        .order_inferred_trade
        .as_ref()
        .expect("state-report fill must generate an inferred trade");
    assert_eq!(inferred.filled_qty, 5.0);
}

#[test]
fn test_cancel_booked_order() {
    let mut oms = build_oms();
    let oid = place_and_get_id(&mut oms, ACCOUNT_1, ETH_PERP_EX1, true, 0.2, 1620.0, 1_060);
    oms.process_message(linkage_msg(GW_KEY_1, oid, "ref060", 1_061));

    let actions = oms.process_message(cancel_msg(oid));
    assert_eq!(actions.len(), 1, "expected SendCancelToGw only");
    assert!(has_send_cancel(&actions), "missing SendCancelToGw");
}

#[test]
fn test_cancel_terminal_order_returns_error() {
    let mut oms = build_oms();
    let oid = place_and_get_id(&mut oms, ACCOUNT_1, ETH_PERP_EX1, true, 0.2, 1620.0, 1_070);
    oms.process_message(linkage_msg(GW_KEY_1, oid, "ref070", 1_071));
    oms.process_message(state_msg(
        GW_KEY_1,
        oid,
        "ref070",
        ExchangeOrderStatus::ExchOrderStatusCancelled,
        0.0,
        0.2,
        0.0,
        1_072,
    ));

    let actions = oms.process_message(cancel_msg(oid));
    assert_eq!(actions.len(), 1, "should get exactly one action");
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, OmsAction::PublishOrderUpdate(_))),
        "expected PublishOrderUpdate for terminal-order cancel"
    );
}

// ===========================================================================
// Gateway classification tests — balance vs position routing
// ===========================================================================

/// PERP entry from gateway → routes to PositionManager → PublishPositionUpdate.
#[test]
fn test_gw_perp_entry_routes_to_position_mgr() {
    let mut oms = build_oms();

    let actions = oms.process_message(OmsMessage::BalanceUpdate(BalanceUpdate {
        balances: vec![PositionReport {
            instrument_code: "ETH".into(), // exchange symbol for ETH-P/USDC@EX1
            instrument_type: InstrumentType::InstTypePerp as i32,
            long_short_type: LongShortType::LsLong as i32,
            exch_account_code: EXCH_ACCOUNT_1.into(),
            qty: 5.0,
            avail_qty: 5.0,
            update_timestamp: 1_080,
            ..Default::default()
        }],
    }));

    let pue = find_position_update(&actions).expect("expected PublishPositionUpdate");
    assert_eq!(pue.account_id, ACCOUNT_1);
    assert!(
        !pue.position_snapshots.is_empty(),
        "position snapshot must not be empty"
    );
    let pos = &pue.position_snapshots[0];
    assert_eq!(pos.instrument_code, "ETH-P/USDC@EX1");
    assert!((pos.total_qty - 5.0).abs() < f64::EPSILON);

    // No balance update should be produced
    assert!(
        find_balance_update(&actions).is_none(),
        "PERP should not produce balance update"
    );
}

/// SPOT entry from gateway → routes to BalanceManager → PublishBalanceUpdate.
#[test]
fn test_gw_spot_entry_routes_to_balance_mgr() {
    let mut oms = build_oms();

    let actions = oms.process_message(OmsMessage::BalanceUpdate(BalanceUpdate {
        balances: vec![PositionReport {
            instrument_code: "USDC".into(),
            instrument_type: InstrumentType::InstTypeSpot as i32,
            exch_account_code: EXCH_ACCOUNT_1.into(),
            qty: 10_000.0,
            avail_qty: 9_500.0,
            update_timestamp: 1_080,
            ..Default::default()
        }],
    }));

    let bue = find_balance_update(&actions).expect("expected PublishBalanceUpdate");
    assert_eq!(bue.account_id, ACCOUNT_1);
    assert!(!bue.balance_snapshots.is_empty());
    let bal = &bue.balance_snapshots[0];
    assert_eq!(bal.asset, "USDC");
    assert!((bal.total_qty - 10_000.0).abs() < f64::EPSILON);
    assert!((bal.avail_qty - 9_500.0).abs() < f64::EPSILON);

    // No position update
    assert!(
        find_position_update(&actions).is_none(),
        "SPOT should not produce position update"
    );
}

/// Mixed SPOT + PERP entries in one gateway message → both updates produced.
#[test]
fn test_gw_mixed_entries_route_both() {
    let mut oms = build_oms();

    let actions = oms.process_message(OmsMessage::BalanceUpdate(BalanceUpdate {
        balances: vec![
            PositionReport {
                instrument_code: "USDC".into(),
                instrument_type: InstrumentType::InstTypeSpot as i32,
                exch_account_code: EXCH_ACCOUNT_1.into(),
                qty: 50_000.0,
                avail_qty: 50_000.0,
                update_timestamp: 2_000,
                ..Default::default()
            },
            PositionReport {
                instrument_code: "ETH".into(),
                instrument_type: InstrumentType::InstTypePerp as i32,
                long_short_type: LongShortType::LsLong as i32,
                exch_account_code: EXCH_ACCOUNT_1.into(),
                qty: 10.0,
                avail_qty: 10.0,
                update_timestamp: 2_000,
                ..Default::default()
            },
        ],
    }));

    assert!(
        find_balance_update(&actions).is_some(),
        "SPOT entry should produce balance update"
    );
    assert!(
        find_position_update(&actions).is_some(),
        "PERP entry should produce position update"
    );
}

// ===========================================================================
// PositionManager unit tests
// ===========================================================================

fn test_position_mgr() -> PositionManager {
    let refdata = test_refdata();
    let mut symbol_map = std::collections::HashMap::new();
    for rd in &refdata {
        symbol_map
            .entry(ACCOUNT_1)
            .or_insert_with(std::collections::HashMap::new)
            .insert(rd.instrument_id_exchange.clone(), rd.clone());
    }
    PositionManager::new(symbol_map, &test_routes())
}

#[test]
fn test_position_freeze_on_sell() {
    let mut mgr = test_position_mgr();
    // Seed position
    let mut pos = OmsManagedPosition::new(ACCOUNT_1, "ETH-P/USDC@EX1", 2, false);
    pos.qty_total = 10.0;
    pos.qty_available = 10.0;
    mgr.init_positions(vec![pos]);

    // Freeze 3.0 for a sell
    mgr.check_and_freeze(ACCOUNT_1, "ETH-P/USDC@EX1", 3.0, false)
        .expect("should succeed");

    let p = mgr.get_position(ACCOUNT_1, "ETH-P/USDC@EX1").unwrap();
    assert!((p.qty_available - 7.0).abs() < f64::EPSILON);
    assert!((p.qty_frozen - 3.0).abs() < f64::EPSILON);
    assert!((p.qty_total - 10.0).abs() < f64::EPSILON);
}

#[test]
fn test_position_freeze_insufficient() {
    let mut mgr = test_position_mgr();
    let mut pos = OmsManagedPosition::new(ACCOUNT_1, "ETH-P/USDC@EX1", 2, false);
    pos.qty_total = 2.0;
    pos.qty_available = 2.0;
    mgr.init_positions(vec![pos]);

    let result = mgr.check_and_freeze(ACCOUNT_1, "ETH-P/USDC@EX1", 5.0, false);
    assert!(result.is_err(), "should fail with insufficient position");
}

#[test]
fn test_position_delta_on_fill() {
    let mut mgr = test_position_mgr();
    let delta = PositionDelta {
        account_id: ACCOUNT_1,
        instrument_code: "ETH-P/USDC@EX1".to_string(),
        is_short: false,
        avail_change: 5.0,
        frozen_change: 0.0,
        total_change: 5.0,
    };
    mgr.apply_delta(&delta, 1000);

    let p = mgr.get_position(ACCOUNT_1, "ETH-P/USDC@EX1").unwrap();
    assert!((p.qty_total - 5.0).abs() < f64::EPSILON);
    assert!((p.qty_available - 5.0).abs() < f64::EPSILON);
}

#[test]
fn test_position_sign_flip() {
    let mut mgr = test_position_mgr();
    let mut pos = OmsManagedPosition::new(ACCOUNT_1, "ETH-P/USDC@EX1", 2, false);
    pos.qty_total = 2.0;
    pos.qty_available = 2.0;
    mgr.init_positions(vec![pos]);

    // Sell 5 → net position goes to -3 → flips to short 3
    let delta = PositionDelta {
        account_id: ACCOUNT_1,
        instrument_code: "ETH-P/USDC@EX1".to_string(),
        is_short: false,
        avail_change: -5.0,
        frozen_change: 0.0,
        total_change: -5.0,
    };
    mgr.apply_delta(&delta, 2000);

    let p = mgr.get_position(ACCOUNT_1, "ETH-P/USDC@EX1").unwrap();
    assert!(p.is_short, "position should be short after sign flip");
    assert!((p.qty_total - 3.0).abs() < f64::EPSILON);
}

#[test]
fn test_reconcile_in_sync() {
    let mut mgr = test_position_mgr();
    // Use BTC which maps uniquely: instrument_id_exchange "BTC" → instrument_id "BTC-P/USDC@EX1"
    let mut pos = OmsManagedPosition::new(ACCOUNT_1, "BTC-P/USDC@EX1", 2, false);
    pos.qty_total = 5.0;
    pos.qty_available = 5.0;
    mgr.init_positions(vec![pos]);

    // Exchange reports same qty
    let entries = vec![PositionReport {
        instrument_code: "BTC".into(),
        instrument_type: InstrumentType::InstTypePerp as i32,
        long_short_type: LongShortType::LsLong as i32,
        exch_account_code: EXCH_ACCOUNT_1.into(),
        qty: 5.0,
        avail_qty: 5.0,
        update_timestamp: 3000,
        ..Default::default()
    }];
    mgr.merge_gw_position_entries(&entries, ACCOUNT_1, 3000);

    assert_eq!(
        mgr.check_reconcile(ACCOUNT_1, "BTC-P/USDC@EX1"),
        ReconcileStatus::InSync
    );
}

#[test]
fn test_reconcile_diverged_transient() {
    let mut mgr = test_position_mgr();
    // Use BTC which maps uniquely: instrument_id_exchange "BTC" → instrument_id "BTC-P/USDC@EX1"
    let mut pos = OmsManagedPosition::new(ACCOUNT_1, "BTC-P/USDC@EX1", 2, false);
    pos.qty_total = 5.0;
    pos.qty_available = 5.0;
    pos.last_exch_sync_ts = 1000; // non-zero: already synced, so merge won't re-seed
    mgr.init_positions(vec![pos]);

    // Exchange reports different qty
    let entries = vec![PositionReport {
        instrument_code: "BTC".into(),
        instrument_type: InstrumentType::InstTypePerp as i32,
        long_short_type: LongShortType::LsLong as i32,
        exch_account_code: EXCH_ACCOUNT_1.into(),
        qty: 3.0,
        avail_qty: 3.0,
        update_timestamp: 4000,
        ..Default::default()
    }];
    mgr.merge_gw_position_entries(&entries, ACCOUNT_1, 4000);

    assert_eq!(
        mgr.check_reconcile(ACCOUNT_1, "BTC-P/USDC@EX1"),
        ReconcileStatus::DivergedTransient
    );
}

// ===========================================================================
// ReservationManager unit tests
// ===========================================================================

#[test]
fn test_cash_reservation_on_buy() {
    let mut mgr = ReservationManager::new();
    mgr.reserve(1001, ACCOUNT_1, "USDC".into(), 20_000.0, false)
        .unwrap();

    assert!((mgr.total_reserved(ACCOUNT_1, "USDC") - 20_000.0).abs() < f64::EPSILON);
}

#[test]
fn test_reservation_released_on_cancel() {
    let mut mgr = ReservationManager::new();
    mgr.reserve(1001, ACCOUNT_1, "USDC".into(), 20_000.0, false)
        .unwrap();
    mgr.release(1001);

    assert!((mgr.total_reserved(ACCOUNT_1, "USDC")).abs() < f64::EPSILON);
}

#[test]
fn test_reservation_partial_release_on_fill() {
    let mut mgr = ReservationManager::new();
    mgr.reserve(1001, ACCOUNT_1, "USDC".into(), 20_000.0, false)
        .unwrap();
    mgr.release_partial(1001, 5_000.0);

    assert!((mgr.total_reserved(ACCOUNT_1, "USDC") - 15_000.0).abs() < f64::EPSILON);
}

#[test]
fn test_check_available_with_reservations() {
    let mut mgr = ReservationManager::new();
    mgr.reserve(1001, ACCOUNT_1, "USDC".into(), 8_000.0, false)
        .unwrap();

    // 10_000 available, 8_000 reserved → 2_000 effective
    let result = mgr.check_available(ACCOUNT_1, "USDC", 3_000.0, 10_000.0);
    assert!(
        result.is_err(),
        "should fail: need 3000, only 2000 effective"
    );

    let result = mgr.check_available(ACCOUNT_1, "USDC", 1_500.0, 10_000.0);
    assert!(
        result.is_ok(),
        "should succeed: need 1500, have 2000 effective"
    );
}

// ===========================================================================
// BalanceManager exchange-only tests
// ===========================================================================

#[test]
fn test_balance_mgr_exchange_only() {
    let mut mgr = zk_oms_rs::balance_mgr::BalanceManager::new(&test_routes(), &test_gw_configs());

    // Merge a spot balance entry
    let entries = vec![PositionReport {
        instrument_code: "USDC".into(),
        instrument_type: InstrumentType::InstTypeSpot as i32,
        exch_account_code: EXCH_ACCOUNT_1.into(),
        qty: 50_000.0,
        avail_qty: 45_000.0,
        update_timestamp: 5000,
        ..Default::default()
    }];
    mgr.merge_gw_balance_entries(&entries, ACCOUNT_1, 5000);

    let snap = mgr.get_balance(ACCOUNT_1, "USDC").unwrap();
    assert!((snap.balance_state.total_qty - 50_000.0).abs() < f64::EPSILON);
    assert!((snap.balance_state.avail_qty - 45_000.0).abs() < f64::EPSILON);
    assert!((snap.balance_state.frozen_qty - 5_000.0).abs() < f64::EPSILON);
    assert!(snap.balance_state.is_from_exch);

    // Build snapshot
    let bue = mgr.build_balance_snapshot(ACCOUNT_1, 5000);
    assert_eq!(bue.balance_snapshots.len(), 1);
    assert_eq!(bue.balance_snapshots[0].asset, "USDC");
}
