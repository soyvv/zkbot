//! OMS V2 latency benchmarks — side-by-side with V1.
//!
//! Criterion groups:
//!   - `place_order/{v1,v2}`      — single PlaceOrder message
//!   - `place_and_fill/{v1,v2}`   — PlaceOrder + immediate-fill report
//!   - `place_and_fill_with_materialize/v2` — V2 process + materialize_action
//!
//! Run:
//!     cargo bench --package zk-oms-rs --bench oms_v2_latency

use criterion::{criterion_group, criterion_main, Criterion};
use std::sync::atomic::{AtomicI64, Ordering};

use zk_oms_rs::{
    config::{ConfdataManager, InstrumentTradingConfig},
    models::OmsMessage,
    oms_core::OmsCore,
    oms_core_v2::OmsCoreV2,
};
use zk_proto_rs::{
    ods::{GwConfigEntry, OmsConfigEntry, OmsRouteEntry},
    zk::{
        common::v1::{BasicOrderType, BuySellType, InstrumentRefData, InstrumentType},
        exch_gw::v1::{
            order_report_entry::Report, ExchangeOrderStatus, OrderIdLinkageReport, OrderReport,
            OrderReportEntry, OrderReportType, OrderStateReport, TradeReport,
        },
        oms::v1::OrderRequest,
    },
};

// ---------------------------------------------------------------------------
// Shared ID counter
// ---------------------------------------------------------------------------

static ORDER_CTR: AtomicI64 = AtomicI64::new(1);

fn next_order_id() -> i64 {
    ORDER_CTR.fetch_add(1, Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

const ACCOUNT_ID: i64 = 100;
const INSTRUMENT: &str = "BTC-P/USDC@EX1";
const GW_KEY: &str = "EX1";

fn make_confdata() -> ConfdataManager {
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
    ConfdataManager::new(
        oms_config,
        account_routes,
        gw_configs,
        refdata,
        trading_configs,
    )
}

fn make_oms_v1() -> OmsCore {
    OmsCore::new(make_confdata(), true, false, false, 50_000)
}

fn make_oms_v2() -> OmsCoreV2 {
    OmsCoreV2::new(&make_confdata(), true, false, false, 50_000)
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
        price: 30_000.0,
        qty: 0.01,
        timestamp: 1_000_000,
        ..Default::default()
    })
}

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
    let mut group = c.benchmark_group("place_order");

    let mut v1 = make_oms_v1();
    group.bench_function("v1", |b| {
        b.iter(|| {
            let id = next_order_id();
            criterion::black_box(v1.process_message(make_place_order(id)));
        });
    });

    let mut v2 = make_oms_v2();
    group.bench_function("v2", |b| {
        b.iter(|| {
            let id = next_order_id();
            criterion::black_box(v2.process_message(make_place_order(id)));
        });
    });

    group.finish();
}

fn bench_place_and_fill(c: &mut Criterion) {
    let mut group = c.benchmark_group("place_and_fill");

    let mut v1 = make_oms_v1();
    group.bench_function("v1", |b| {
        b.iter(|| {
            let id = next_order_id();
            let a1 = v1.process_message(make_place_order(id));
            let a2 = v1.process_message(make_fill_report(id));
            criterion::black_box((a1, a2));
        });
    });

    let mut v2 = make_oms_v2();
    group.bench_function("v2", |b| {
        b.iter(|| {
            let id = next_order_id();
            let a1 = v2.process_message(make_place_order(id));
            let a2 = v2.process_message(make_fill_report(id));
            criterion::black_box((a1, a2));
        });
    });

    group.finish();
}

fn bench_place_and_fill_with_materialize(c: &mut Criterion) {
    let mut v2 = make_oms_v2();

    c.bench_function("place_and_fill_with_materialize/v2", |b| {
        b.iter(|| {
            let id = next_order_id();
            let a1 = v2.process_message(make_place_order(id));
            let a2 = v2.process_message(make_fill_report(id));
            // Materialize all actions to measure full cost including string reconstruction
            let mat1: Vec<_> = a1.iter().map(|a| v2.materialize_action(a)).collect();
            let mat2: Vec<_> = a2.iter().map(|a| v2.materialize_action(a)).collect();
            criterion::black_box((mat1, mat2));
        });
    });
}

criterion_group!(
    benches,
    bench_place_order,
    bench_place_and_fill,
    bench_place_and_fill_with_materialize,
);
criterion_main!(benches);
