use std::collections::HashMap;

use zk_proto_rs::zk::{
    common::v1::InstrumentRefData,
    rtmd::v1::{kline::KlineType, Kline, PriceLevel, TickData},
};
use zk_strategy_sdk_rs::strategy::Strategy;

use crate::{
    backtester::{BacktestConfig, BacktestResult, Backtester},
    event_queue::BtEventKind,
    match_policy::{FirstComeFirstServedMatchPolicy, ImmediateMatchPolicy, MatchPolicy},
};

pub enum TestMatchPolicyKind {
    Immediate,
    Fcfs,
}

pub struct TestEventSequenceBuilder {
    current_ts_ms: i64,
    events: Vec<(i64, BtEventKind)>,
}

impl Default for TestEventSequenceBuilder {
    fn default() -> Self {
        Self {
            current_ts_ms: 0,
            events: Vec::new(),
        }
    }
}

impl TestEventSequenceBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start_at_ms(mut self, ts_ms: i64) -> Self {
        self.current_ts_ms = ts_ms;
        self
    }

    pub fn clock_forward_millis(mut self, delta_ms: i64) -> Self {
        self.current_ts_ms += delta_ms;
        self
    }

    pub fn add_tick_ob(mut self, symbol: &str, b1: f64, s1: f64) -> Self {
        self.events.push((
            self.current_ts_ms,
            BtEventKind::Tick(TickData {
                instrument_code: symbol.to_string(),
                original_timestamp: self.current_ts_ms,
                buy_price_levels: vec![PriceLevel {
                    price: b1,
                    qty: 1.0,
                    ..Default::default()
                }],
                sell_price_levels: vec![PriceLevel {
                    price: s1,
                    qty: 1.0,
                    ..Default::default()
                }],
                ..Default::default()
            }),
        ));
        self
    }

    pub fn add_tick_levels(
        mut self,
        symbol: &str,
        bids: Vec<(f64, f64)>,
        asks: Vec<(f64, f64)>,
    ) -> Self {
        self.events.push((
            self.current_ts_ms,
            BtEventKind::Tick(TickData {
                instrument_code: symbol.to_string(),
                original_timestamp: self.current_ts_ms,
                buy_price_levels: bids
                    .into_iter()
                    .map(|(price, qty)| PriceLevel {
                        price,
                        qty,
                        ..Default::default()
                    })
                    .collect(),
                sell_price_levels: asks
                    .into_iter()
                    .map(|(price, qty)| PriceLevel {
                        price,
                        qty,
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            }),
        ));
        self
    }

    pub fn add_bar(
        mut self,
        symbol: &str,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
        volume: f64,
    ) -> Self {
        self.events.push((
            self.current_ts_ms,
            BtEventKind::Bar(Kline {
                kline_type: KlineType::Kline1min as i32,
                symbol: symbol.to_string(),
                open,
                high,
                low,
                close,
                volume,
                amount: 0.0,
                timestamp: self.current_ts_ms,
                kline_end_timestamp: self.current_ts_ms,
                source: String::new(),
            }),
        ));
        self
    }

    pub fn build(mut self) -> Vec<(i64, BtEventKind)> {
        self.events.sort_by_key(|event| event.0);
        self.events
    }
}

pub struct StrategyTestBuilder {
    account_ids: Vec<i64>,
    refdata: Vec<InstrumentRefData>,
    init_balances: Option<HashMap<i64, HashMap<String, f64>>>,
    init_positions: Option<HashMap<i64, HashMap<String, f64>>>,
    events: Vec<(i64, BtEventKind)>,
    match_policy: TestMatchPolicyKind,
}

impl Default for StrategyTestBuilder {
    fn default() -> Self {
        Self {
            account_ids: Vec::new(),
            refdata: Vec::new(),
            init_balances: None,
            init_positions: None,
            events: Vec::new(),
            match_policy: TestMatchPolicyKind::Immediate,
        }
    }
}

impl StrategyTestBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_accounts(mut self, account_ids: Vec<i64>) -> Self {
        self.account_ids = account_ids;
        self
    }

    pub fn with_refdata(mut self, refdata: Vec<InstrumentRefData>) -> Self {
        self.refdata = refdata;
        self
    }

    pub fn with_init_balances(
        mut self,
        init_balances: HashMap<i64, HashMap<String, f64>>,
    ) -> Self {
        self.init_balances = Some(init_balances);
        self
    }

    pub fn with_init_positions(
        mut self,
        init_positions: HashMap<i64, HashMap<String, f64>>,
    ) -> Self {
        self.init_positions = Some(init_positions);
        self
    }

    pub fn with_event_sequence(mut self, events: Vec<(i64, BtEventKind)>) -> Self {
        self.events = events;
        self
    }

    pub fn with_match_policy(mut self, match_policy: TestMatchPolicyKind) -> Self {
        self.match_policy = match_policy;
        self
    }

    pub fn build(self) -> StrategyTestHarness {
        let match_policy: Box<dyn MatchPolicy> = match self.match_policy {
            TestMatchPolicyKind::Immediate => Box::new(ImmediateMatchPolicy),
            TestMatchPolicyKind::Fcfs => Box::new(FirstComeFirstServedMatchPolicy),
        };

        let mut backtester = Backtester::new(BacktestConfig {
            account_ids: self.account_ids,
            refdata: self.refdata,
            init_balances: self.init_balances,
            init_positions: self.init_positions,
            match_policy,
            init_data_fetcher: None,
            progress_callback: None,
        });
        if !self.events.is_empty() {
            backtester.add_sorted_stream(self.events);
        }
        StrategyTestHarness { backtester }
    }
}

pub struct StrategyTestHarness {
    backtester: Backtester,
}

impl StrategyTestHarness {
    pub fn run<S: Strategy>(&mut self, strategy: &mut S) -> &BacktestResult {
        self.backtester.run(strategy)
    }

    pub fn backtester(&self) -> &Backtester {
        &self.backtester
    }

    pub fn backtester_mut(&mut self) -> &mut Backtester {
        &mut self.backtester
    }
}

pub fn instrument_refdata(symbol: &str) -> InstrumentRefData {
    InstrumentRefData {
        instrument_id: symbol.to_string(),
        instrument_id_exchange: symbol.to_string(),
        exchange_name: "SIM1".to_string(),
        ..Default::default()
    }
}
