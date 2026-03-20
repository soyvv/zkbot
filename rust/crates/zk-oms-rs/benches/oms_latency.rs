//! OMS latency benchmarks.
//!
//! Measures the wall-clock cost of core `OmsCore::process_message` paths:
//!
//! | Benchmark            | What is timed                                          |
//! |----------------------|--------------------------------------------------------|
//! | `place_order`        | One `PlaceOrder` msg (booking + route lookup)          |
//! | `place_and_fill`     | `PlaceOrder` + immediate-fill `GatewayOrderReport`     |
//!
//! Run:
//!     cargo bench --package zk-oms-rs --bench oms_latency
//!
//! HTML report appears in `target/criterion/`.

use criterion::{criterion_group, criterion_main, Criterion};
use std::sync::atomic::{AtomicI64, Ordering};

use zk_oms_rs::{
    config::{ConfdataManager, InstrumentTradingConfig},
    models::OmsMessage,
    oms_core::OmsCore,
};
use zk_proto_rs::{
    common::{BasicOrderType, BuySellType, InstrumentRefData, InstrumentType, TimeInForceType},
    exch_gw::{
        order_report_entry::Report, ExchangeOrderStatus, OrderIdLinkageReport, OrderReport,
        OrderReportEntry, OrderReportType, OrderStateReport, TradeReport,
    },
    ods::{GwConfigEntry, OmsConfigEntry, OmsRouteEntry},
    oms::OrderRequest,
};

// ---------------------------------------------------------------------------
// Shared counter for unique order IDs across benchmark iterations.
// ---------------------------------------------------------------------------

static ORDER_CTR: AtomicI64 = AtomicI64::new(1);

fn next_order_id() -> i64 {
    ORDER_CTR.fetch_add(1, Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// OMS setup
// ---------------------------------------------------------------------------

const ACCOUNT_ID: i64 = 100;
const INSTRUMENT: &str = "BTC-P/USDC@EX1";
const GW_KEY: &str = "EX1";

fn make_oms() -> OmsCore {
    let oms_config = OmsConfigEntry {
        oms_id: "bench".to_string(),
        managed_account_ids: vec![ACCOUNT_ID],
        ..Default::default()
    };
    let account_routes = vec![OmsRouteEntry {
        account_id: ACCOUNT_ID,
        gw_key: GW_KEY.to_string(),
        exch_account_id: "ACC1".to_string(),
        ..Default::default()
    }];
    let gw_configs = vec![GwConfigEntry {
        gw_key: GW_KEY.to_string(),
        exch_name: GW_KEY.to_string(),
        cancel_required_fields: vec!["order_id".to_string()],
        ..Default::default()
    }];
    let refdata = vec![InstrumentRefData {
        instrument_id: INSTRUMENT.to_string(),
        instrument_id_exchange: "BTC".to_string(),
        base_asset: "BTC".to_string(),
        quote_asset: "USDC".to_string(),
        instrument_type: InstrumentType::InstTypePerp as i32,
        exchange_name: GW_KEY.to_string(),
        price_precision: 1,
        qty_precision: 5,
        ..Default::default()
    }];
    let trading_configs = vec![InstrumentTradingConfig {
        bookkeeping_balance: true,
        ..InstrumentTradingConfig::default_for(INSTRUMENT)
    }];

    let confdata = ConfdataManager::new(
        oms_config,
        account_routes,
        gw_configs,
        refdata,
        trading_configs,
    );
    OmsCore::new(
        confdata, /*use_time_emulation=*/ true, /*risk_check=*/ false,
        /*external=*/ false, 50_000,
    )
}

// ---------------------------------------------------------------------------
// Message builders
// ---------------------------------------------------------------------------

fn make_place_order(order_id: i64) -> OmsMessage {
    OmsMessage::PlaceOrder(OrderRequest {
        order_id,
        account_id: ACCOUNT_ID,
        instrument_code: INSTRUMENT.to_string(),
        buy_sell_type: BuySellType::BsBuy as i32,
        order_type: BasicOrderType::OrdertypeLimit as i32,
        time_inforce_type: TimeInForceType::TimeinforceGtc as i32,
        price: 30_000.0,
        qty: 0.01,
        timestamp: 1_000_000,
        ..Default::default()
    })
}

/// Builds a fill `OrderReport` that immediately completes the order.
/// Includes Linkage + Trade + State=Filled entries — mirrors `ImmediateMatchPolicy`.
fn make_fill_report(order_id: i64) -> OmsMessage {
    let exch_ref = format!("E{order_id}");
    OmsMessage::GatewayOrderReport(OrderReport {
        order_id,
        exch_order_ref: exch_ref.clone(),
        exchange: GW_KEY.to_string(),
        update_timestamp: 1_000_001,
        order_report_entries: vec![
            OrderReportEntry {
                report_type: OrderReportType::OrderRepTypeLinkage as i32,
                report: Some(Report::OrderIdLinkageReport(OrderIdLinkageReport {
                    order_id,
                    exch_order_ref: exch_ref.clone(),
                })),
            },
            OrderReportEntry {
                report_type: OrderReportType::OrderRepTypeTrade as i32,
                report: Some(Report::TradeReport(TradeReport {
                    exch_trade_id: format!("T{order_id}"),
                    filled_qty: 0.01,
                    filled_price: 30_000.0,
                    filled_ts: 1_000_001,
                    ..Default::default()
                })),
            },
            OrderReportEntry {
                report_type: OrderReportType::OrderRepTypeState as i32,
                report: Some(Report::OrderStateReport(OrderStateReport {
                    exch_order_status: ExchangeOrderStatus::ExchOrderStatusFilled as i32,
                    filled_qty: 0.01,
                    unfilled_qty: 0.0,
                    avg_price: 30_000.0,
                    ..Default::default()
                })),
            },
        ],
        ..Default::default()
    })
}

// ---------------------------------------------------------------------------
// Benchmarks
// ---------------------------------------------------------------------------

fn bench_place_order(c: &mut Criterion) {
    let mut oms = make_oms();

    c.bench_function("place_order", |b| {
        b.iter(|| {
            let id = next_order_id();
            let actions = oms.process_message(make_place_order(id));
            // Prevent the compiler from optimising away the result.
            criterion::black_box(actions);
        });
    });
}

fn bench_place_and_fill(c: &mut Criterion) {
    // Each iteration is self-contained: place + fill → order reaches terminal state
    // and is removed from the open-order cache.  OMS memory stays bounded.
    let mut oms = make_oms();

    c.bench_function("place_and_fill", |b| {
        b.iter(|| {
            let id = next_order_id();
            let a1 = oms.process_message(make_place_order(id));
            let a2 = oms.process_message(make_fill_report(id));
            criterion::black_box((a1, a2));
        });
    });
}

criterion_group!(benches, bench_place_order, bench_place_and_fill);
criterion_main!(benches);
