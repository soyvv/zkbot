//! Parity tests for zk-backtest-rs
//!
//! Each test verifies that the Rust backtester produces the same logical
//! outcomes as the Python `StrategyBacktestor` for a given scenario.

use std::sync::{Arc, Mutex};

use zk_proto_rs::zk::{
    common::v1::{BuySellType, InstrumentRefData},
    oms::v1::{OrderStatus, OrderUpdateEvent, PositionUpdateEvent},
    rtmd::v1::{PriceLevel, TickData},
};
use zk_strategy_sdk_rs::{
    context::StrategyContext,
    models::{SAction, StrategyCancel, StrategyOrder, TimerEvent, TimerSchedule, TimerSubscription},
    strategy::Strategy,
};

use zk_backtest_rs::{
    backtester::{Backtester, BacktestConfig},
    event_queue::BtEventKind,
    match_policy::{FirstComeFirstServedMatchPolicy, ImmediateMatchPolicy},
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn refdata(sym: &str) -> InstrumentRefData {
    InstrumentRefData {
        instrument_id: sym.to_string(),
        instrument_id_exchange: sym.to_string(),
        exchange_name: "SIM1".to_string(),
        ..Default::default()
    }
}

fn tick(sym: &str, ts: i64, bids: Vec<(f64, f64)>, asks: Vec<(f64, f64)>) -> TickData {
    TickData {
        instrument_code: sym.to_string(),
        original_timestamp: ts,
        buy_price_levels: bids
            .into_iter()
            .map(|(price, qty)| PriceLevel { price, qty, ..Default::default() })
            .collect(),
        sell_price_levels: asks
            .into_iter()
            .map(|(price, qty)| PriceLevel { price, qty, ..Default::default() })
            .collect(),
        ..Default::default()
    }
}

fn order(id: i64, sym: &str, side: BuySellType, price: f64, qty: f64, acc: i64) -> StrategyOrder {
    StrategyOrder {
        order_id: id,
        symbol: sym.to_string(),
        price,
        qty,
        side: side as i32,
        account_id: acc,
    }
}

fn snap_status(u: &OrderUpdateEvent) -> Option<OrderStatus> {
    u.order_snapshot.as_ref().and_then(|s| OrderStatus::try_from(s.order_status).ok())
}

fn has_status(updates: &[OrderUpdateEvent], status: OrderStatus) -> bool {
    updates.iter().any(|u| snap_status(u) == Some(status))
}

// ---------------------------------------------------------------------------
// Test 1: ImmediateMatchPolicy — buy placed on reinit, fills immediately
// ---------------------------------------------------------------------------

struct BuyImmediately;
impl Strategy for BuyImmediately {
    fn on_reinit(&mut self, _ctx: &StrategyContext) -> Vec<SAction> {
        vec![SAction::PlaceOrder(order(1, "BTC/USD@SIM1", BuySellType::BsBuy, 50_000.0, 1.0, 100))]
    }
}

#[test]
fn test_immediate_fill_on_reinit() {
    let config = BacktestConfig {
        account_ids: vec![100],
        refdata: vec![refdata("BTC/USD@SIM1")],
        init_balances: None,
        init_positions: None,
        match_policy: Box::new(ImmediateMatchPolicy),
        init_data_fetcher: None,
        progress_callback: None,
    };
    let mut bt = Backtester::new(config);
    let result = bt.run(&mut BuyImmediately);

    assert_eq!(result.order_placements.len(), 1);
    assert!(
        has_status(&result.order_updates, OrderStatus::Filled),
        "expected Filled order update; got: {:?}",
        result.order_updates.iter().map(snap_status).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// Test 2: FCFS — buy fills when matching ask arrives in tick
// ---------------------------------------------------------------------------

struct BuyOnFirstTick { placed: bool }
impl Strategy for BuyOnFirstTick {
    fn on_tick(&mut self, _t: &TickData, _ctx: &StrategyContext) -> Vec<SAction> {
        if !self.placed {
            self.placed = true;
            return vec![SAction::PlaceOrder(order(2, "ETH/USD@SIM1", BuySellType::BsBuy, 3_000.0, 2.0, 200))];
        }
        vec![]
    }
}

#[test]
fn test_fcfs_buy_fills_on_matching_ask() {
    let sym = "ETH/USD@SIM1";
    let config = BacktestConfig {
        account_ids: vec![200],
        refdata: vec![refdata(sym)],
        init_balances: None,
        init_positions: None,
        match_policy: Box::new(FirstComeFirstServedMatchPolicy),
        init_data_fetcher: None,
        progress_callback: None,
    };
    let mut bt = Backtester::new(config);
    // Tick 1: places order; ask at 3001 (misses 3000 limit).
    // Tick 2: ask drops to 2990, which is <= 3000 → fill.
    bt.add_sorted_stream(vec![
        (1_000, BtEventKind::Tick(tick(sym, 1_000, vec![(2_999.0, 5.0)], vec![(3_001.0, 5.0)]))),
        (2_000, BtEventKind::Tick(tick(sym, 2_000, vec![(2_990.0, 5.0)], vec![(2_990.0, 5.0)]))),
    ]);
    let result = bt.run(&mut BuyOnFirstTick { placed: false });

    assert_eq!(result.order_placements.len(), 1);
    assert!(
        has_status(&result.order_updates, OrderStatus::Filled),
        "expected Filled; statuses: {:?}",
        result.order_updates.iter().map(snap_status).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// Test 3: Cancel booked order before any fill
// ---------------------------------------------------------------------------

struct PlaceThenCancel { placed: bool, cancelled: bool }
impl Strategy for PlaceThenCancel {
    fn on_tick(&mut self, _t: &TickData, _ctx: &StrategyContext) -> Vec<SAction> {
        if !self.placed {
            self.placed = true;
            return vec![SAction::PlaceOrder(order(3, "BNB/USD@SIM1", BuySellType::BsBuy, 500.0, 1.0, 300))];
        }
        vec![]
    }
    fn on_order_update(&mut self, u: &OrderUpdateEvent, _ctx: &StrategyContext) -> Vec<SAction> {
        if !self.cancelled && snap_status(u) == Some(OrderStatus::Booked) {
            self.cancelled = true;
            return vec![SAction::Cancel(StrategyCancel { order_id: 3, account_id: 300 })];
        }
        vec![]
    }
}

#[test]
fn test_cancel_booked_order() {
    let sym = "BNB/USD@SIM1";
    let config = BacktestConfig {
        account_ids: vec![300],
        refdata: vec![refdata(sym)],
        init_balances: None,
        init_positions: None,
        match_policy: Box::new(FirstComeFirstServedMatchPolicy),
        init_data_fetcher: None,
        progress_callback: None,
    };
    let mut bt = Backtester::new(config);
    // Ask at 600 — buy limit at 500 never fills, books instead → cancel fires
    bt.add_sorted_stream(vec![
        (1_000, BtEventKind::Tick(tick(sym, 1_000, vec![(499.0, 10.0)], vec![(600.0, 10.0)]))),
        (2_000, BtEventKind::Tick(tick(sym, 2_000, vec![(499.0, 10.0)], vec![(600.0, 10.0)]))),
    ]);
    let result = bt.run(&mut PlaceThenCancel { placed: false, cancelled: false });

    assert_eq!(result.order_placements.len(), 1);
    assert!(
        has_status(&result.order_updates, OrderStatus::Cancelled),
        "expected Cancelled; statuses: {:?}",
        result.order_updates.iter().map(snap_status).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// Test 4: Price-miss orders book but do not fill
// ---------------------------------------------------------------------------

struct TwoPriceMissOrders;
impl Strategy for TwoPriceMissOrders {
    fn on_reinit(&mut self, _ctx: &StrategyContext) -> Vec<SAction> {
        vec![
            SAction::PlaceOrder(order(4, "SOL/USD@SIM1", BuySellType::BsBuy, 10.0, 1.0, 400)),
            SAction::PlaceOrder(order(5, "SOL/USD@SIM1", BuySellType::BsBuy,  9.0, 1.0, 400)),
        ]
    }
}

#[test]
fn test_price_miss_orders_do_not_fill() {
    let sym = "SOL/USD@SIM1";
    let config = BacktestConfig {
        account_ids: vec![400],
        refdata: vec![refdata(sym)],
        init_balances: None,
        init_positions: None,
        match_policy: Box::new(FirstComeFirstServedMatchPolicy),
        init_data_fetcher: None,
        progress_callback: None,
    };
    let mut bt = Backtester::new(config);
    // Ask is 50, both bids far below
    bt.add_sorted_stream(vec![
        (1_000, BtEventKind::Tick(tick(sym, 1_000, vec![(8.0, 100.0)], vec![(50.0, 100.0)]))),
    ]);
    let result = bt.run(&mut TwoPriceMissOrders);

    assert_eq!(result.order_placements.len(), 2);
    let filled_count = result.order_updates.iter()
        .filter(|u| snap_status(u) == Some(OrderStatus::Filled))
        .count();
    assert_eq!(filled_count, 0, "orders with limit below ask must not fill");
}

// ---------------------------------------------------------------------------
// Test 5: Sell order fills against bid when bid >= limit
// ---------------------------------------------------------------------------

struct SellWhenBidRises { placed: bool }
impl Strategy for SellWhenBidRises {
    fn on_tick(&mut self, _t: &TickData, _ctx: &StrategyContext) -> Vec<SAction> {
        if !self.placed {
            self.placed = true;
            return vec![SAction::PlaceOrder(order(6, "BTC/USD@SIM1", BuySellType::BsSell, 49_000.0, 0.5, 500))];
        }
        vec![]
    }
}

#[test]
fn test_sell_fills_when_bid_meets_limit() {
    let sym = "BTC/USD@SIM1";
    let config = BacktestConfig {
        account_ids: vec![500],
        refdata: vec![refdata(sym)],
        init_balances: None,
        init_positions: None,
        match_policy: Box::new(FirstComeFirstServedMatchPolicy),
        init_data_fetcher: None,
        progress_callback: None,
    };
    let mut bt = Backtester::new(config);
    bt.add_sorted_stream(vec![
        // Tick 1: bid 48k misses 49k limit; order books.
        (1_000, BtEventKind::Tick(tick(sym, 1_000, vec![(48_000.0, 1.0)], vec![(50_000.0, 1.0)]))),
        // Tick 2: bid 50k >= 49k limit → fill.
        (2_000, BtEventKind::Tick(tick(sym, 2_000, vec![(50_000.0, 1.0)], vec![(51_000.0, 1.0)]))),
    ]);
    let result = bt.run(&mut SellWhenBidRises { placed: false });

    assert_eq!(result.order_placements.len(), 1);
    assert!(
        has_status(&result.order_updates, OrderStatus::Filled),
        "sell should fill when bid >= limit; statuses: {:?}",
        result.order_updates.iter().map(snap_status).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// Test 6: One-shot timer fires exactly once
// ---------------------------------------------------------------------------

struct OnceTimerStrategy { fires: usize }
impl Strategy for OnceTimerStrategy {
    fn on_reinit(&mut self, _ctx: &StrategyContext) -> Vec<SAction> {
        vec![SAction::SubscribeTimer(TimerSubscription {
            timer_key: "once".to_string(),
            schedule: TimerSchedule::OnceAt(5_000),
        })]
    }
    fn on_timer(&mut self, _event: &TimerEvent, _ctx: &StrategyContext) -> Vec<SAction> {
        self.fires += 1;
        vec![]
    }
}

#[test]
fn test_once_timer_fires_exactly_once() {
    let sym = "BTC/USD@SIM1";
    let config = BacktestConfig {
        account_ids: vec![100],
        refdata: vec![refdata(sym)],
        init_balances: None,
        init_positions: None,
        match_policy: Box::new(ImmediateMatchPolicy),
        init_data_fetcher: None,
        progress_callback: None,
    };
    let mut bt = Backtester::new(config);
    // Ticks at 1s through 10s; timer fires at 5s.
    bt.add_sorted_stream(
        (1..=10).map(|i| {
            let ts = i * 1_000_i64;
            (ts, BtEventKind::Tick(tick(sym, ts, vec![], vec![])))
        }).collect(),
    );

    let mut strategy = OnceTimerStrategy { fires: 0 };
    bt.run(&mut strategy);
    assert_eq!(strategy.fires, 1, "one-shot timer must fire exactly once");
}

// ---------------------------------------------------------------------------
// Test 7: Cron timer fires once per second for 5 seconds
// ---------------------------------------------------------------------------

struct CronTimerStrategy { fires: usize }
impl Strategy for CronTimerStrategy {
    fn on_reinit(&mut self, _ctx: &StrategyContext) -> Vec<SAction> {
        vec![SAction::SubscribeTimer(TimerSubscription {
            timer_key: "every_second".to_string(),
            schedule: TimerSchedule::Cron {
                expr: "* * * * * *".to_string(), // every second
                start_ms: Some(0),
                end_ms: None,
            },
        })]
    }
    fn on_timer(&mut self, _event: &TimerEvent, _ctx: &StrategyContext) -> Vec<SAction> {
        self.fires += 1;
        vec![]
    }
}

#[test]
fn test_cron_timer_fires_at_expected_intervals() {
    let sym = "BTC/USD@SIM1";
    let config = BacktestConfig {
        account_ids: vec![100],
        refdata: vec![refdata(sym)],
        init_balances: None,
        init_positions: None,
        match_policy: Box::new(ImmediateMatchPolicy),
        init_data_fetcher: None,
        progress_callback: None,
    };
    let mut bt = Backtester::new(config);
    // Ticks at 1s, 2s, 3s, 4s, 5s.
    bt.add_sorted_stream(
        (1..=5).map(|i| {
            let ts = i * 1_000_i64;
            (ts, BtEventKind::Tick(tick(sym, ts, vec![], vec![])))
        }).collect(),
    );

    let mut strategy = CronTimerStrategy { fires: 0 };
    bt.run(&mut strategy);
    assert_eq!(strategy.fires, 5, "cron timer (every second) must fire once per tick-second");
}

// ---------------------------------------------------------------------------
// Test 8: Strategy sees correct open orders count via ctx in on_order_update
// ---------------------------------------------------------------------------

struct TrackOpenOrdersStrategy { max_open_seen: usize }
impl Strategy for TrackOpenOrdersStrategy {
    fn on_reinit(&mut self, _ctx: &StrategyContext) -> Vec<SAction> {
        vec![
            SAction::PlaceOrder(order(20, "BTC/USD@SIM1", BuySellType::BsBuy, 1.0, 1.0, 100)),
            SAction::PlaceOrder(order(21, "BTC/USD@SIM1", BuySellType::BsBuy, 1.0, 1.0, 100)),
        ]
    }
    fn on_order_update(&mut self, _u: &OrderUpdateEvent, ctx: &StrategyContext) -> Vec<SAction> {
        let open = ctx.get_open_orders(100).len();
        if open > self.max_open_seen {
            self.max_open_seen = open;
        }
        vec![]
    }
}

#[test]
fn test_strategy_queries_open_orders_in_callback() {
    // FCFS policy books orders first before filling on next tick,
    // giving us a window where open orders are visible in ctx.
    let sym = "BTC/USD@SIM1";
    let config = BacktestConfig {
        account_ids: vec![100],
        refdata: vec![refdata(sym)],
        init_balances: None,
        init_positions: None,
        match_policy: Box::new(FirstComeFirstServedMatchPolicy),
        init_data_fetcher: None,
        progress_callback: None,
    };
    let mut bt = Backtester::new(config);
    // High ask so orders book but never fill.
    bt.add_sorted_stream(vec![
        (1_000, BtEventKind::Tick(tick(sym, 1_000, vec![(0.5, 10.0)], vec![(999_999.0, 10.0)]))),
    ]);

    let mut strategy = TrackOpenOrdersStrategy { max_open_seen: 0 };
    bt.run(&mut strategy);
    assert!(
        strategy.max_open_seen >= 1,
        "strategy should see ≥1 open order via ctx.get_open_orders(); got {}",
        strategy.max_open_seen,
    );
}

// ---------------------------------------------------------------------------
// Test 9: Strategy sees position in ctx during on_position_update callback
// ---------------------------------------------------------------------------

struct ObservePositionStrategy { saw_position: bool }
impl Strategy for ObservePositionStrategy {
    fn on_position_update(&mut self, _pue: &PositionUpdateEvent, ctx: &StrategyContext) -> Vec<SAction> {
        if ctx.get_position(100, "BTC/USD@SIM1").is_some() {
            self.saw_position = true;
        }
        vec![]
    }
}

#[test]
fn test_strategy_queries_position_in_callback() {
    // Inject a position update directly to verify ctx.get_position() is populated
    // before the strategy callback fires. This tests the ctx query mechanism
    // independently of whether the OMS emits balance updates on fills.
    let sym = "BTC/USD@SIM1";
    let config = BacktestConfig {
        account_ids: vec![100],
        refdata: vec![refdata(sym)],
        init_balances: None,
        init_positions: None,
        match_policy: Box::new(ImmediateMatchPolicy),
        init_data_fetcher: None,
        progress_callback: None,
    };
    let mut bt = Backtester::new(config);
    let pos_event = zk_proto_rs::zk::oms::v1::PositionUpdateEvent {
        position_snapshots: vec![zk_proto_rs::zk::oms::v1::Position {
            account_id: 100,
            instrument_code: sym.to_string(),
            ..Default::default()
        }],
        ..Default::default()
    };
    bt.add_sorted_stream(vec![(1_000, BtEventKind::PositionUpdate(pos_event))]);

    let mut strategy = ObservePositionStrategy { saw_position: false };
    bt.run(&mut strategy);
    assert!(strategy.saw_position, "position must be visible in ctx during on_position_update");
}

// ---------------------------------------------------------------------------
// Test 10: init_data_fetcher result readable in on_init via ctx.get_init_data
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct WarmupData { threshold: f64, label: String }

struct ReadsInitInOnInit { observed_threshold: Option<f64> }
impl Strategy for ReadsInitInOnInit {
    fn on_init(&mut self, ctx: &StrategyContext) -> Vec<SAction> {
        if let Some(d) = ctx.get_init_data::<WarmupData>() {
            self.observed_threshold = Some(d.threshold);
        }
        vec![]
    }
}

#[test]
fn test_init_data_readable_in_on_init() {
    let sym = "BTC/USD@SIM1";
    let config = BacktestConfig {
        account_ids: vec![100],
        refdata: vec![refdata(sym)],
        init_balances: None,
        init_positions: None,
        match_policy: Box::new(ImmediateMatchPolicy),
        init_data_fetcher: Some(Box::new(|_ctx| {
            Box::new(WarmupData { threshold: 42.5, label: "test".to_string() })
        })),
        progress_callback: None,
    };
    let mut bt = Backtester::new(config);
    let mut strategy = ReadsInitInOnInit { observed_threshold: None };
    bt.run(&mut strategy);
    assert_eq!(
        strategy.observed_threshold,
        Some(42.5),
        "strategy must read typed init data in on_init via ctx.get_init_data::<WarmupData>()"
    );
}

// ---------------------------------------------------------------------------
// Test 11: init_data persists and is readable in on_reinit
// ---------------------------------------------------------------------------

struct ReadsInitInOnReinit { observed_label: Option<String> }
impl Strategy for ReadsInitInOnReinit {
    fn on_reinit(&mut self, ctx: &StrategyContext) -> Vec<SAction> {
        if let Some(d) = ctx.get_init_data::<WarmupData>() {
            self.observed_label = Some(d.label.clone());
        }
        vec![]
    }
}

#[test]
fn test_init_data_readable_in_on_reinit() {
    let sym = "BTC/USD@SIM1";
    let config = BacktestConfig {
        account_ids: vec![100],
        refdata: vec![refdata(sym)],
        init_balances: None,
        init_positions: None,
        match_policy: Box::new(ImmediateMatchPolicy),
        init_data_fetcher: Some(Box::new(|_ctx| {
            Box::new(WarmupData { threshold: 1.0, label: "warmup".to_string() })
        })),
        progress_callback: None,
    };
    let mut bt = Backtester::new(config);
    let mut strategy = ReadsInitInOnReinit { observed_label: None };
    bt.run(&mut strategy);
    assert_eq!(
        strategy.observed_label.as_deref(),
        Some("warmup"),
        "init data must persist into on_reinit and be readable via ctx.get_init_data::<WarmupData>()"
    );
}

// ---------------------------------------------------------------------------
// Test 12: progress callback receives 0 and 100
// ---------------------------------------------------------------------------

struct NoopStrategy;
impl Strategy for NoopStrategy {}

#[test]
fn test_progress_callback_fires_0_and_100() {
    let sym = "BTC/USD@SIM1";
    let received: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let received_clone = received.clone();

    let config = BacktestConfig {
        account_ids: vec![100],
        refdata: vec![refdata(sym)],
        init_balances: None,
        init_positions: None,
        match_policy: Box::new(ImmediateMatchPolicy),
        init_data_fetcher: None,
        progress_callback: Some(Box::new(move |pct| {
            received_clone.lock().unwrap().push(pct);
        })),
    };
    let mut bt = Backtester::new(config);
    // 10 ticks spanning 1s–10s give a clear 0–100% range.
    bt.add_sorted_stream(
        (1..=10).map(|i| {
            let ts = i * 1_000_i64;
            (ts, BtEventKind::Tick(tick(sym, ts, vec![], vec![])))
        }).collect(),
    );
    bt.run(&mut NoopStrategy);

    let r = received.lock().unwrap();
    assert!(r.contains(&0), "progress callback must fire for 0%");
    assert!(r.contains(&100), "progress callback must fire for 100%");
}

// ---------------------------------------------------------------------------
// Test 13: progress callback fires in non-decreasing order
// ---------------------------------------------------------------------------

#[test]
fn test_progress_fires_in_order() {
    let sym = "BTC/USD@SIM1";
    let received: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let received_clone = received.clone();

    let config = BacktestConfig {
        account_ids: vec![100],
        refdata: vec![refdata(sym)],
        init_balances: None,
        init_positions: None,
        match_policy: Box::new(ImmediateMatchPolicy),
        init_data_fetcher: None,
        progress_callback: Some(Box::new(move |pct| {
            received_clone.lock().unwrap().push(pct);
        })),
    };
    let mut bt = Backtester::new(config);
    bt.add_sorted_stream(
        (1..=100).map(|i| {
            let ts = i * 1_000_i64;
            (ts, BtEventKind::Tick(tick(sym, ts, vec![], vec![])))
        }).collect(),
    );
    bt.run(&mut NoopStrategy);

    let r = received.lock().unwrap();
    assert!(!r.is_empty(), "progress must be called at least once");
    for w in r.windows(2) {
        assert!(w[0] <= w[1], "progress must be non-decreasing: {:?}", r);
    }
}
