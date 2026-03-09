//! Backtester throughput benchmark — pure Rust, no Python overhead.
//!
//! Measures the event-loop cost of `Backtester::run` with a no-op native strategy.
//! Uses the same bar count as the USDJPY 2021-Q1 fixture (90 054 bars) so the
//! numbers are directly comparable to `bench_backtest.py`.
//!
//! Run:
//!     cargo bench --package zk-backtest-rs --bench backtest_throughput
//!
//! HTML report: zkbot/rust/target/criterion/

use criterion::{criterion_group, criterion_main, Criterion};

use zk_backtest_rs::{
    backtester::{BacktestConfig, Backtester},
    event_queue::BtEventKind,
    match_policy::ImmediateMatchPolicy,
};
use zk_proto_rs::{
    common::{InstrumentRefData, InstrumentType},
    rtmd::Kline,
};
use zk_strategy_sdk_rs::strategy::Strategy;

// ---------------------------------------------------------------------------
// Constants — match the Python bench fixture
// ---------------------------------------------------------------------------

const ACCOUNT_ID: i64 = 123;
const INSTRUMENT: &str = "USD-P/JPY@SIM1";
const EXCH_SYMBOL: &str = "USDJPY";
const EXCHANGE: &str = "SIM1";

/// Total bars matching the USDJPY 2021-Q1 parquet fixture (90 054 rows).
const N_BARS: usize = 90_054;

// ---------------------------------------------------------------------------
// No-op strategy
// ---------------------------------------------------------------------------

struct NoOpStrategy;

impl Strategy for NoOpStrategy {}

// ---------------------------------------------------------------------------
// Setup helpers
// ---------------------------------------------------------------------------

fn make_refdata() -> InstrumentRefData {
    InstrumentRefData {
        instrument_id: INSTRUMENT.to_string(),
        instrument_id_exchange: EXCH_SYMBOL.to_string(),
        base_asset: "USD".to_string(),
        quote_asset: "JPY".to_string(),
        instrument_type: InstrumentType::InstTypePerp as i32,
        exchange_name: EXCHANGE.to_string(),
        price_precision: 4,
        qty_precision: 2,
        ..Default::default()
    }
}

/// Generates `N_BARS` synthetic 1-minute klines starting at 2021-01-01T00:00Z.
fn make_bars() -> Vec<(i64, BtEventKind)> {
    let base_ms: i64 = 1_609_459_200_000; // 2021-01-01T00:00:00Z in ms
    let step_ms: i64 = 60_000;            // 1 minute

    (0..N_BARS as i64)
        .map(|i| {
            let ts = base_ms + i * step_ms;
            let price = 103.0 + (i as f64 * 0.0001).sin() * 0.5; // gentle oscillation
            let bar = Kline {
                symbol: EXCH_SYMBOL.to_string(),
                timestamp: ts,
                kline_end_timestamp: ts + step_ms - 1,
                open: price,
                high: price + 0.02,
                low: price - 0.02,
                close: price + 0.01,
                volume: 100.0,
                ..Default::default()
            };
            (ts, BtEventKind::Bar(bar))
        })
        .collect()
}

/// Build a fresh `Backtester` with bars pre-loaded (setup cost excluded from timing).
fn make_backtester() -> Backtester {
    let refdata = make_refdata();
    let config = BacktestConfig {
        account_ids: vec![ACCOUNT_ID],
        refdata: vec![refdata],
        init_balances: Some(
            [(ACCOUNT_ID, [("USD".to_string(), 1500.0)].into())].into(),
        ),
        match_policy: Box::new(ImmediateMatchPolicy),
        init_data_fetcher: None,
        progress_callback: None,
    };
    let mut bt = Backtester::new(config);
    bt.add_sorted_stream(make_bars());
    bt
}

// ---------------------------------------------------------------------------
// Benchmarks
// ---------------------------------------------------------------------------

fn bench_noop_strategy(c: &mut Criterion) {
    let mut group = c.benchmark_group("backtest_throughput");

    group.bench_function("noop_strategy_90k_bars", |b| {
        b.iter_batched(
            // Setup: build a fresh backtester per iteration (bars are consumed by run()).
            make_backtester,
            // Measured: just the event-loop replay.
            |mut bt| {
                let mut strategy = NoOpStrategy;
                criterion::black_box(bt.run(&mut strategy));
            },
            criterion::BatchSize::LargeInput,
        );
    });

    group.finish();
}

criterion_group!(benches, bench_noop_strategy);
criterion_main!(benches);
