use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use zk_proto_rs::zk::common::v1::{BuySellType, OpenCloseType};
use zk_proto_rs::zk::oms::v1::{OrderStatus, OrderUpdateEvent};
use zk_proto_rs::zk::rtmd::v1::TickData;
use zk_strategy_sdk_rs::context::StrategyContext;
use zk_strategy_sdk_rs::models::{
    SAction, StrategyCancel, StrategyLog, StrategyOrder, TimerEvent, TimerSchedule,
    TimerSubscription,
};
use zk_strategy_sdk_rs::strategy::Strategy;

const TIMER_KEY: &str = "smoke_mm/refresh";
const MAX_OPEN_ORDERS_BEFORE_PAUSE: usize = 4;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SmokeMMConfig {
    /// Interval between quote refresh cycles (ms).
    pub quote_interval_ms: i64,
    /// Qty for each side of the quote.
    pub quote_qty: f64,
    /// Buy quote distance, in instrument price ticks, below best bid.
    pub buy_tick_distance: f64,
    /// Sell quote distance, in instrument price ticks, above best ask.
    pub sell_tick_distance: f64,
}

impl Default for SmokeMMConfig {
    fn default() -> Self {
        Self {
            quote_interval_ms: 5_000,
            quote_qty: 1.0,
            buy_tick_distance: 0.0,
            sell_tick_distance: 0.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Per-symbol quote state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct SymbolState {
    best_bid: f64,
    best_ask: f64,
    buy_order_id: Option<i64>,
    sell_order_id: Option<i64>,
    /// Approximate internal position accumulated from fill deltas.
    /// Positive = net long, negative = net short.
    /// APPROXIMATION: uses `last_trade` from OrderUpdateEvent when available,
    /// falls back to `filled_qty` delta from Order snapshots. May drift if
    /// events are missed or reordered.
    internal_position: f64,
    /// Last known filled_qty per order, used for delta tracking when
    /// `last_trade` is absent.
    last_filled_qty: HashMap<i64, f64>,
}

impl SymbolState {
    fn new(bid: f64, ask: f64) -> Self {
        Self {
            best_bid: bid,
            best_ask: ask,
            buy_order_id: None,
            sell_order_id: None,
            internal_position: 0.0,
            last_filled_qty: HashMap::new(),
        }
    }

    fn active_order_ids(&self) -> impl Iterator<Item = i64> + '_ {
        self.buy_order_id.into_iter().chain(self.sell_order_id)
    }
}

// ---------------------------------------------------------------------------
// Strategy
// ---------------------------------------------------------------------------

/// Simple market-making smoke strategy for engine/runtime validation.
///
/// Places one buy and one sell quote at BBO for each symbol seen in ticks.
/// Refreshes quotes on a configurable timer interval. Tracks internal position
/// from order fills and logs comparison with the engine context position.
#[derive(Debug, Clone)]
pub struct SmokeTestStrategy {
    config: SmokeMMConfig,
    /// Per-symbol quote and position state.
    symbols: HashMap<String, SymbolState>,
    /// false while paused — suppresses new quotes.
    quoting_enabled: bool,
    /// Cached first account_id from context.
    account_id: Option<i64>,
}

impl Default for SmokeTestStrategy {
    fn default() -> Self {
        Self::with_config(SmokeMMConfig::default())
    }
}

impl SmokeTestStrategy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_config(config: SmokeMMConfig) -> Self {
        Self {
            config,
            symbols: HashMap::new(),
            quoting_enabled: true,
            account_id: None,
        }
    }

    fn log(message: impl Into<String>, ts_ms: i64) -> SAction {
        SAction::Log(StrategyLog {
            ts_ms,
            message: message.into(),
        })
    }

    fn account_id(&mut self, ctx: &StrategyContext) -> Option<i64> {
        if self.account_id.is_none() {
            self.account_id = ctx.account_ids().next();
        }
        self.account_id
    }

    /// Subscribe (or re-arm) the refresh timer as a one-shot relative to `now_ms`.
    fn arm_timer(&self, now_ms: i64) -> SAction {
        SAction::SubscribeTimer(TimerSubscription {
            timer_key: TIMER_KEY.to_string(),
            schedule: TimerSchedule::OnceAt(now_ms + self.config.quote_interval_ms),
        })
    }

    /// Extract best bid/ask from tick price levels.
    /// Returns None if either side is empty.
    fn extract_bbo(tick: &TickData) -> Option<(f64, f64)> {
        let bid = tick.buy_price_levels.first().map(|l| l.price)?;
        let ask = tick.sell_price_levels.first().map(|l| l.price)?;
        if bid <= 0.0 || ask <= 0.0 || bid >= ask {
            return None;
        }
        Some((bid, ask))
    }

    fn tick_size_for(&self, symbol: &str, ctx: &StrategyContext) -> f64 {
        ctx.get_symbol_info(symbol)
            .map(|info| info.price_tick_size)
            .filter(|tick| *tick > 0.0)
            .unwrap_or(1.0)
    }

    fn quote_prices(
        &self,
        symbol: &str,
        bid: f64,
        ask: f64,
        ctx: &StrategyContext,
    ) -> Option<(f64, f64)> {
        let tick_size = self.tick_size_for(symbol, ctx);
        let buy_offset = self.config.buy_tick_distance.max(0.0) * tick_size;
        let sell_offset = self.config.sell_tick_distance.max(0.0) * tick_size;
        let quote_bid = bid - buy_offset;
        let quote_ask = ask + sell_offset;

        if quote_bid <= 0.0 || quote_bid >= quote_ask {
            return None;
        }

        Some((quote_bid, quote_ask))
    }

    /// Place buy+sell quotes for a symbol using configured distance from BBO.
    fn place_quotes(
        &mut self,
        symbol: &str,
        bid: f64,
        ask: f64,
        ctx: &StrategyContext,
        account_id: i64,
        ts_ms: i64,
    ) -> Vec<SAction> {
        let open_orders = ctx.get_open_orders(account_id).len();
        if open_orders > MAX_OPEN_ORDERS_BEFORE_PAUSE {
            return vec![Self::log(
                format!(
                    "smoke_mm.skip_quote symbol={symbol} reason=open_order_limit open_orders={open_orders} limit={MAX_OPEN_ORDERS_BEFORE_PAUSE}"
                ),
                ts_ms,
            )];
        }

        let Some((quote_bid, quote_ask)) = self.quote_prices(symbol, bid, ask, ctx) else {
            return vec![Self::log(
                format!(
                    "smoke_mm.skip_quote symbol={symbol} reason=invalid_quote_prices bid={bid} ask={ask}"
                ),
                ts_ms,
            )];
        };
        let buy_id = ctx.next_id();
        let sell_id = ctx.next_id();
        let ss = self.symbols.get_mut(symbol).expect("symbol must exist");
        let mut actions = Vec::with_capacity(3);
        ss.buy_order_id = Some(buy_id);
        ss.sell_order_id = Some(sell_id);

        actions.push(Self::log(
            format!(
                "smoke_mm.quote symbol={symbol} buy_id={buy_id}@{quote_bid} sell_id={sell_id}@{quote_ask} qty={} buy_ticks={} sell_ticks={}",
                self.config.quote_qty, self.config.buy_tick_distance, self.config.sell_tick_distance,
            ),
            ts_ms,
        ));
        actions.push(SAction::PlaceOrder(StrategyOrder {
            order_id: buy_id,
            symbol: symbol.to_string(),
            price: quote_bid,
            qty: self.config.quote_qty,
            side: BuySellType::BsBuy as i32,
            open_close_type: OpenCloseType::OcOpen as i32,
            account_id,
        }));
        actions.push(SAction::PlaceOrder(StrategyOrder {
            order_id: sell_id,
            symbol: symbol.to_string(),
            price: quote_ask,
            qty: self.config.quote_qty,
            side: BuySellType::BsSell as i32,
            open_close_type: OpenCloseType::OcOpen as i32,
            account_id,
        }));
        actions
    }

    /// Cancel all active quotes across all symbols.
    fn cancel_all(&mut self, account_id: i64, ts_ms: i64) -> Vec<SAction> {
        let mut actions = Vec::new();
        for (sym, ss) in &mut self.symbols {
            for oid in ss.active_order_ids().collect::<Vec<_>>() {
                actions.push(Self::log(
                    format!("smoke_mm.cancel symbol={sym} order_id={oid}"),
                    ts_ms,
                ));
                actions.push(SAction::Cancel(StrategyCancel {
                    order_id: oid,
                    account_id,
                }));
                ss.last_filled_qty.remove(&oid);
            }
            ss.buy_order_id = None;
            ss.sell_order_id = None;
        }
        actions
    }

    /// Cancel active quotes for one symbol.
    fn cancel_symbol(&mut self, symbol: &str, account_id: i64, ts_ms: i64) -> Vec<SAction> {
        let mut actions = Vec::new();
        if let Some(ss) = self.symbols.get_mut(symbol) {
            for oid in ss.active_order_ids().collect::<Vec<_>>() {
                actions.push(Self::log(
                    format!("smoke_mm.cancel symbol={symbol} order_id={oid}"),
                    ts_ms,
                ));
                actions.push(SAction::Cancel(StrategyCancel {
                    order_id: oid,
                    account_id,
                }));
                ss.last_filled_qty.remove(&oid);
            }
            ss.buy_order_id = None;
            ss.sell_order_id = None;
        }
        actions
    }

    /// Find which symbol an order belongs to, and whether it is the buy or sell side.
    fn find_order(&self, order_id: i64) -> Option<(&str, BuySellType)> {
        for (sym, ss) in &self.symbols {
            if ss.buy_order_id == Some(order_id) {
                return Some((sym.as_str(), BuySellType::BsBuy));
            }
            if ss.sell_order_id == Some(order_id) {
                return Some((sym.as_str(), BuySellType::BsSell));
            }
        }
        None
    }

    /// Apply a fill delta to internal position and log comparison with ctx position.
    fn apply_fill(
        &mut self,
        symbol: &str,
        side: BuySellType,
        fill_qty: f64,
        ctx: &StrategyContext,
        ts_ms: i64,
    ) -> SAction {
        let ss = self.symbols.get_mut(symbol).expect("symbol must exist");
        let signed = match side {
            BuySellType::BsBuy => fill_qty,
            _ => -fill_qty,
        };
        ss.internal_position += signed;

        let account_id = self.account_id.unwrap_or(0);
        let ctx_pos = ctx
            .get_position(account_id, symbol)
            .map(|p| p.total_qty)
            .unwrap_or(0.0);
        let diff = ss.internal_position - ctx_pos;

        Self::log(
            format!(
                "smoke_mm.fill symbol={symbol} side={side:?} qty={fill_qty} \
                 internal_pos={:.4} ctx_pos={:.4} diff={:.4}",
                ss.internal_position, ctx_pos, diff,
            ),
            ts_ms,
        )
    }

    /// Re-quote if both order slots are cleared (filled, cancelled, or rejected).
    fn maybe_requote_on_both_cleared(
        &mut self,
        symbol: &str,
        ctx: &StrategyContext,
        account_id: i64,
        ts_ms: i64,
    ) -> Vec<SAction> {
        let ss = self.symbols.get(symbol).unwrap();
        if ss.buy_order_id.is_none() && ss.sell_order_id.is_none() && self.quoting_enabled {
            let bid = ss.best_bid;
            let ask = ss.best_ask;
            if bid > 0.0 && ask > 0.0 && bid < ask {
                return self.place_quotes(symbol, bid, ask, ctx, account_id, ts_ms);
            }
        }
        vec![]
    }
}

impl Strategy for SmokeTestStrategy {
    fn on_create(&mut self, ctx: &StrategyContext) -> Vec<SAction> {
        vec![Self::log("smoke_mm.on_create", ctx.current_ts_ms)]
    }

    fn on_init(&mut self, ctx: &StrategyContext) -> Vec<SAction> {
        let account_count = ctx.account_ids().count();
        self.account_id(ctx);
        vec![Self::log(
            format!("smoke_mm.on_init accounts={account_count}"),
            ctx.current_ts_ms,
        )]
    }

    fn on_reinit(&mut self, ctx: &StrategyContext) -> Vec<SAction> {
        let mut actions = vec![Self::log("smoke_mm.on_reinit", ctx.current_ts_ms)];
        actions.push(self.arm_timer(ctx.current_ts_ms));
        actions
    }

    fn on_tick(&mut self, tick: &TickData, ctx: &StrategyContext) -> Vec<SAction> {
        let symbol = &tick.instrument_code;
        let Some((bid, ask)) = Self::extract_bbo(tick) else {
            return vec![Self::log(
                format!("smoke_mm.on_tick symbol={symbol} skipped=invalid_bbo"),
                ctx.current_ts_ms,
            )];
        };

        let is_new_symbol = !self.symbols.contains_key(symbol);
        let ss = self
            .symbols
            .entry(symbol.clone())
            .or_insert_with(|| SymbolState::new(bid, ask));
        ss.best_bid = bid;
        ss.best_ask = ask;

        if !self.quoting_enabled {
            return vec![];
        }

        // First tick for a new symbol: place quotes immediately.
        if is_new_symbol {
            let Some(account_id) = self.account_id.or_else(|| ctx.account_ids().next()) else {
                return vec![Self::log(
                    "smoke_mm.on_tick no account_ids",
                    ctx.current_ts_ms,
                )];
            };
            self.account_id = Some(account_id);
            return self.place_quotes(symbol, bid, ask, ctx, account_id, ctx.current_ts_ms);
        }

        vec![]
    }

    fn on_timer(&mut self, event: &TimerEvent, _ctx: &StrategyContext) -> Vec<SAction> {
        if event.timer_key != TIMER_KEY {
            return vec![];
        }

        let mut actions = vec![Self::log("smoke_mm.on_timer refresh", event.ts_ms)];

        // Re-arm timer for next cycle.
        actions.push(self.arm_timer(event.ts_ms));

        if !self.quoting_enabled {
            return actions;
        }

        let Some(account_id) = self.account_id else {
            return actions;
        };

        // Collect symbols to refresh (need to borrow self mutably later).
        let syms: Vec<(String, f64, f64)> = self
            .symbols
            .iter()
            .map(|(s, ss)| (s.clone(), ss.best_bid, ss.best_ask))
            .collect();

        for (sym, bid, ask) in syms {
            // Cancel existing quotes.
            actions.extend(self.cancel_symbol(&sym, account_id, event.ts_ms));

            // Re-quote if BBO is valid.
            if bid > 0.0 && ask > 0.0 && bid < ask {
                actions.extend(self.place_quotes(&sym, bid, ask, _ctx, account_id, event.ts_ms));
            } else {
                actions.push(Self::log(
                    format!("smoke_mm.skip_quote symbol={sym} bid={bid} ask={ask}"),
                    event.ts_ms,
                ));
            }
        }

        actions
    }

    fn on_order_update(
        &mut self,
        update: &OrderUpdateEvent,
        ctx: &StrategyContext,
    ) -> Vec<SAction> {
        let order_id = update.order_id;
        let Some((symbol, side)) = self.find_order(order_id) else {
            return vec![];
        };
        let symbol = symbol.to_string();

        let mut actions = Vec::new();

        // Track fills for internal position bookkeeping.
        // Prefer last_trade (incremental fill) when present; otherwise use
        // filled_qty delta from the order snapshot.
        if let Some(trade) = &update.last_trade {
            if trade.filled_qty > 0.0 {
                actions.push(self.apply_fill(
                    &symbol,
                    side,
                    trade.filled_qty,
                    ctx,
                    update.timestamp,
                ));
            }
        } else if let Some(snap) = &update.order_snapshot {
            let ss = self.symbols.get_mut(&symbol).unwrap();
            let prev = ss.last_filled_qty.get(&order_id).copied().unwrap_or(0.0);
            let delta = snap.filled_qty - prev;
            if delta > 0.0 {
                ss.last_filled_qty.insert(order_id, snap.filled_qty);
                actions.push(self.apply_fill(&symbol, side, delta, ctx, update.timestamp));
            }
        }

        // Clear our tracking when order reaches terminal state.
        let is_terminal = update.order_snapshot.as_ref().is_some_and(|snap| {
            matches!(
                OrderStatus::try_from(snap.order_status),
                Ok(OrderStatus::Filled) | Ok(OrderStatus::Cancelled) | Ok(OrderStatus::Rejected)
            )
        });

        if is_terminal {
            let ss = self.symbols.get_mut(&symbol).unwrap();
            ss.last_filled_qty.remove(&order_id);
            if ss.buy_order_id == Some(order_id) {
                ss.buy_order_id = None;
            }
            if ss.sell_order_id == Some(order_id) {
                ss.sell_order_id = None;
            }

            // If both sides filled, re-quote immediately.
            let account_id = self.account_id.unwrap_or(0);
            actions.extend(self.maybe_requote_on_both_cleared(
                &symbol,
                ctx,
                account_id,
                update.timestamp,
            ));
        }

        actions
    }

    fn on_pause(&mut self, reason: &str, ctx: &StrategyContext) -> Vec<SAction> {
        let mut actions = vec![Self::log(
            format!("smoke_mm.on_pause reason={reason}"),
            ctx.current_ts_ms,
        )];
        self.quoting_enabled = false;
        let account_id = self.account_id.unwrap_or(0);
        actions.extend(self.cancel_all(account_id, ctx.current_ts_ms));
        actions
    }

    fn on_resume(&mut self, reason: &str, ctx: &StrategyContext) -> Vec<SAction> {
        let mut actions = vec![Self::log(
            format!("smoke_mm.on_resume reason={reason}"),
            ctx.current_ts_ms,
        )];
        self.quoting_enabled = true;

        // Re-quote immediately using cached BBO for all known symbols.
        let Some(account_id) = self.account_id else {
            return actions;
        };
        let syms: Vec<(String, f64, f64)> = self
            .symbols
            .iter()
            .map(|(s, ss)| (s.clone(), ss.best_bid, ss.best_ask))
            .collect();
        for (sym, bid, ask) in syms {
            if bid > 0.0 && ask > 0.0 && bid < ask {
                actions.extend(self.place_quotes(
                    &sym,
                    bid,
                    ask,
                    ctx,
                    account_id,
                    ctx.current_ts_ms,
                ));
            }
        }
        actions
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use zk_backtest_rs::testutils::{
        instrument_refdata, StrategyTestBuilder, TestEventSequenceBuilder, TestMatchPolicyKind,
    };
    use zk_proto_rs::zk::common::v1::InstrumentRefData;
    use zk_proto_rs::zk::oms::v1::{Order, Trade};
    use zk_proto_rs::zk::rtmd::v1::PriceLevel;
    use zk_strategy_sdk_rs::context::StrategyContext;

    fn make_ctx() -> StrategyContext {
        StrategyContext::new(&[100], &[])
    }

    fn make_ctx_with_refdata(symbol: &str, price_tick_size: f64) -> StrategyContext {
        StrategyContext::new(
            &[100],
            &[InstrumentRefData {
                instrument_id: symbol.to_string(),
                price_tick_size,
                ..Default::default()
            }],
        )
    }

    fn make_tick(symbol: &str, bid: f64, ask: f64) -> TickData {
        TickData {
            instrument_code: symbol.to_string(),
            buy_price_levels: vec![PriceLevel {
                price: bid,
                qty: 10.0,
                ..Default::default()
            }],
            sell_price_levels: vec![PriceLevel {
                price: ask,
                qty: 10.0,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn make_oue_filled(order_id: i64, filled_qty: f64, trade_qty: f64) -> OrderUpdateEvent {
        OrderUpdateEvent {
            order_id,
            account_id: 100,
            order_snapshot: Some(Order {
                order_id,
                order_status: OrderStatus::Filled as i32,
                filled_qty,
                ..Default::default()
            }),
            last_trade: if trade_qty > 0.0 {
                Some(Trade {
                    order_id,
                    filled_qty: trade_qty,
                    ..Default::default()
                })
            } else {
                None
            },
            ..Default::default()
        }
    }

    fn make_oue_cancelled(order_id: i64) -> OrderUpdateEvent {
        OrderUpdateEvent {
            order_id,
            account_id: 100,
            order_snapshot: Some(Order {
                order_id,
                order_status: OrderStatus::Cancelled as i32,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn make_oue_open(order_id: i64) -> OrderUpdateEvent {
        OrderUpdateEvent {
            order_id,
            account_id: 100,
            order_snapshot: Some(Order {
                order_id,
                order_status: OrderStatus::Booked as i32,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn count_orders(actions: &[SAction]) -> usize {
        actions
            .iter()
            .filter(|a| matches!(a, SAction::PlaceOrder(_)))
            .count()
    }

    fn count_cancels(actions: &[SAction]) -> usize {
        actions
            .iter()
            .filter(|a| matches!(a, SAction::Cancel(_)))
            .count()
    }

    fn get_order_ids(actions: &[SAction]) -> Vec<i64> {
        actions
            .iter()
            .filter_map(|a| match a {
                SAction::PlaceOrder(o) => Some(o.order_id),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn end_to_end_backtest_places_fills_and_requotes() {
        let symbol = "BTC/USD@SIM1";
        let events = TestEventSequenceBuilder::new()
            .start_at_ms(1_000)
            .add_tick_ob(symbol, 100.0, 101.0)
            .clock_forward_millis(1_000)
            .add_tick_ob(symbol, 99.0, 100.0)
            .clock_forward_millis(1_000)
            .add_tick_ob(symbol, 101.0, 102.0)
            .build();

        let mut init_balances = HashMap::new();
        init_balances.insert(
            100,
            HashMap::from([
                ("USD".to_string(), 10_000.0),
                ("BTC".to_string(), 10.0),
            ]),
        );

        let refdata = vec![InstrumentRefData {
            price_tick_size: 1.0,
            ..instrument_refdata(symbol)
        }];

        let mut harness = StrategyTestBuilder::new()
            .with_accounts(vec![100])
            .with_refdata(refdata)
            .with_init_balances(init_balances)
            .with_match_policy(TestMatchPolicyKind::Fcfs)
            .with_event_sequence(events)
            .build();

        let mut strategy = SmokeTestStrategy::with_config(SmokeMMConfig {
            quote_interval_ms: 5_000,
            quote_qty: 1.0,
            buy_tick_distance: 0.0,
            sell_tick_distance: 0.0,
        });

        let result = harness.run(&mut strategy);

        assert_eq!(result.order_placements.len(), 4);
        assert_eq!(result.trades.len(), 2);

        let prices: Vec<f64> = result
            .order_placements
            .iter()
            .map(|(_, order)| order.price)
            .collect();
        assert_eq!(prices, vec![100.0, 101.0, 101.0, 102.0]);

        let fill_logs: Vec<&str> = result
            .logs
            .iter()
            .map(|log| log.message.as_str())
            .filter(|msg| msg.contains("smoke_mm.fill"))
            .collect();
        assert_eq!(fill_logs.len(), 2);

        let requote_logs: Vec<&str> = result
            .logs
            .iter()
            .map(|log| log.message.as_str())
            .filter(|msg| msg.contains("smoke_mm.quote"))
            .collect();
        assert_eq!(requote_logs.len(), 2);
    }

    // ── First tick triggers quotes ──────────────────────────────────────

    #[test]
    fn first_tick_places_buy_and_sell() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        let tick = make_tick("BTC-USDT", 100.0, 101.0);

        let actions = strat.on_tick(&tick, &ctx);

        assert_eq!(count_orders(&actions), 2);
        let ids = get_order_ids(&actions);
        assert_eq!(ids.len(), 2);

        // Verify buy and sell sides
        let buy = actions.iter().find_map(|a| match a {
            SAction::PlaceOrder(o) if o.side == BuySellType::BsBuy as i32 => Some(o),
            _ => None,
        });
        let sell = actions.iter().find_map(|a| match a {
            SAction::PlaceOrder(o) if o.side == BuySellType::BsSell as i32 => Some(o),
            _ => None,
        });
        assert!(buy.is_some());
        assert!(sell.is_some());
        assert_eq!(buy.unwrap().price, 100.0);
        assert_eq!(sell.unwrap().price, 101.0);
        assert_eq!(buy.unwrap().symbol, "BTC-USDT");
    }

    #[test]
    fn first_tick_skips_when_more_than_four_open_orders() {
        let mut strat = SmokeTestStrategy::new();
        let mut ctx = make_ctx();
        for order_id in 1..=5 {
            ctx.on_order_update(&make_oue_open(order_id));
        }

        let actions = strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);

        assert_eq!(count_orders(&actions), 0);
        let logs: Vec<&str> = actions
            .iter()
            .filter_map(|action| match action {
                SAction::Log(log) => Some(log.message.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            logs.iter().any(|msg| msg.contains("reason=open_order_limit")),
            "expected open-order-limit skip log, got {logs:?}"
        );
    }

    #[test]
    fn second_tick_same_symbol_does_not_quote() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        let tick = make_tick("BTC-USDT", 100.0, 101.0);
        strat.on_tick(&tick, &ctx);

        let tick2 = make_tick("BTC-USDT", 100.5, 101.5);
        let actions = strat.on_tick(&tick2, &ctx);
        assert_eq!(count_orders(&actions), 0);
    }

    #[test]
    fn new_symbol_gets_its_own_quotes() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);
        let actions = strat.on_tick(&make_tick("ETH-USDT", 50.0, 51.0), &ctx);
        assert_eq!(count_orders(&actions), 2);
    }

    #[test]
    fn invalid_bbo_does_not_quote() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();

        // Empty price levels
        let tick = TickData {
            instrument_code: "BTC-USDT".into(),
            ..Default::default()
        };
        let actions = strat.on_tick(&tick, &ctx);
        assert_eq!(count_orders(&actions), 0);

        // bid >= ask
        let tick = make_tick("BTC-USDT", 101.0, 100.0);
        let actions = strat.on_tick(&tick, &ctx);
        assert_eq!(count_orders(&actions), 0);
    }

    // ── Timer triggers cancel+replace ───────────────────────────────────

    #[test]
    fn timer_cancels_and_replaces_quotes() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);

        let timer_event = TimerEvent {
            timer_key: TIMER_KEY.to_string(),
            ts_ms: 5000,
        };
        let actions = strat.on_timer(&timer_event, &ctx);

        // Should cancel 2 old + place 2 new
        assert_eq!(count_cancels(&actions), 2);
        assert_eq!(count_orders(&actions), 2);

        // New orders should use updated BBO (unchanged here since no new tick).
        let buy = actions.iter().find_map(|a| match a {
            SAction::PlaceOrder(o) if o.side == BuySellType::BsBuy as i32 => Some(o),
            _ => None,
        });
        assert_eq!(buy.unwrap().price, 100.0);
    }

    #[test]
    fn timer_uses_latest_bbo() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);
        // Update BBO via second tick
        strat.on_tick(&make_tick("BTC-USDT", 200.0, 201.0), &ctx);

        let timer_event = TimerEvent {
            timer_key: TIMER_KEY.to_string(),
            ts_ms: 5000,
        };
        let actions = strat.on_timer(&timer_event, &ctx);

        let buy = actions.iter().find_map(|a| match a {
            SAction::PlaceOrder(o) if o.side == BuySellType::BsBuy as i32 => Some(o),
            _ => None,
        });
        assert_eq!(buy.unwrap().price, 200.0);
    }

    #[test]
    fn timer_rearms_itself() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();

        let timer_event = TimerEvent {
            timer_key: TIMER_KEY.to_string(),
            ts_ms: 5000,
        };
        let actions = strat.on_timer(&timer_event, &ctx);

        let has_timer_sub = actions
            .iter()
            .any(|a| matches!(a, SAction::SubscribeTimer(_)));
        assert!(has_timer_sub);
    }

    // ── Pause cancels and suppresses ────────────────────────────────────

    #[test]
    fn pause_cancels_active_quotes() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);

        let actions = strat.on_pause("test", &ctx);
        assert_eq!(count_cancels(&actions), 2);
    }

    #[test]
    fn tick_while_paused_does_not_quote() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        strat.on_pause("test", &ctx);

        let actions = strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);
        assert_eq!(count_orders(&actions), 0);
    }

    #[test]
    fn timer_while_paused_does_not_quote() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);
        strat.on_pause("test", &ctx);

        let timer_event = TimerEvent {
            timer_key: TIMER_KEY.to_string(),
            ts_ms: 5000,
        };
        let actions = strat.on_timer(&timer_event, &ctx);
        assert_eq!(count_orders(&actions), 0);
        assert_eq!(count_cancels(&actions), 0);
    }

    // ── Resume re-quotes ────────────────────────────────────────────────

    #[test]
    fn resume_places_quotes_from_cached_bbo() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);
        strat.on_pause("test", &ctx);

        let actions = strat.on_resume("test", &ctx);
        assert_eq!(count_orders(&actions), 2);
    }

    // ── Order update position tracking ──────────────────────────────────

    #[test]
    fn fill_updates_internal_position_via_last_trade() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);

        let buy_id = strat.symbols["BTC-USDT"].buy_order_id.unwrap();
        let oue = make_oue_filled(buy_id, 1.0, 1.0);
        strat.on_order_update(&oue, &ctx);

        assert_eq!(strat.symbols["BTC-USDT"].internal_position, 1.0);
    }

    #[test]
    fn sell_fill_decreases_position() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);

        let sell_id = strat.symbols["BTC-USDT"].sell_order_id.unwrap();
        let oue = make_oue_filled(sell_id, 1.0, 1.0);
        strat.on_order_update(&oue, &ctx);

        assert_eq!(strat.symbols["BTC-USDT"].internal_position, -1.0);
    }

    #[test]
    fn fill_without_last_trade_uses_snapshot_delta() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);

        let buy_id = strat.symbols["BTC-USDT"].buy_order_id.unwrap();
        // No last_trade, just snapshot with filled_qty
        let oue = make_oue_filled(buy_id, 0.5, 0.0);
        strat.on_order_update(&oue, &ctx);

        assert_eq!(strat.symbols["BTC-USDT"].internal_position, 0.5);
    }

    #[test]
    fn both_sides_filled_triggers_requote() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);

        let buy_id = strat.symbols["BTC-USDT"].buy_order_id.unwrap();
        let sell_id = strat.symbols["BTC-USDT"].sell_order_id.unwrap();

        strat.on_order_update(&make_oue_filled(buy_id, 1.0, 1.0), &ctx);
        let actions = strat.on_order_update(&make_oue_filled(sell_id, 1.0, 1.0), &ctx);

        // Second fill should trigger re-quote (2 new orders)
        assert_eq!(count_orders(&actions), 2);
    }

    #[test]
    fn cancelled_order_clears_tracking() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);

        let buy_id = strat.symbols["BTC-USDT"].buy_order_id.unwrap();
        strat.on_order_update(&make_oue_cancelled(buy_id), &ctx);

        assert!(strat.symbols["BTC-USDT"].buy_order_id.is_none());
    }

    // ── Config ──────────────────────────────────────────────────────────

    #[test]
    fn custom_config_sets_quote_qty() {
        let cfg = SmokeMMConfig {
            quote_qty: 5.0,
            ..Default::default()
        };
        let mut strat = SmokeTestStrategy::with_config(cfg);
        let ctx = make_ctx();
        let actions = strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);

        let buy = actions.iter().find_map(|a| match a {
            SAction::PlaceOrder(o) if o.side == BuySellType::BsBuy as i32 => Some(o),
            _ => None,
        });
        assert_eq!(buy.unwrap().qty, 5.0);
    }

    #[test]
    fn custom_tick_distance_offsets_quotes_from_bbo() {
        let cfg = SmokeMMConfig {
            buy_tick_distance: 2.0,
            sell_tick_distance: 3.0,
            ..Default::default()
        };
        let mut strat = SmokeTestStrategy::with_config(cfg);
        let ctx = make_ctx_with_refdata("BTC-USDT", 0.5);
        let actions = strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);

        let buy = actions.iter().find_map(|a| match a {
            SAction::PlaceOrder(o) if o.side == BuySellType::BsBuy as i32 => Some(o),
            _ => None,
        });
        let sell = actions.iter().find_map(|a| match a {
            SAction::PlaceOrder(o) if o.side == BuySellType::BsSell as i32 => Some(o),
            _ => None,
        });

        assert_eq!(buy.unwrap().price, 99.0);
        assert_eq!(sell.unwrap().price, 102.5);
    }

    // ── Reinit arms timer ───────────────────────────────────────────────

    #[test]
    fn reinit_subscribes_timer() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();

        let actions = strat.on_reinit(&ctx);
        let has_timer = actions
            .iter()
            .any(|a| matches!(a, SAction::SubscribeTimer(_)));
        assert!(has_timer);
    }

    // ── Lifecycle callbacks ─────────────────────────────────────────────

    #[test]
    fn on_create_logs() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        let actions = strat.on_create(&ctx);
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], SAction::Log(l) if l.message.contains("on_create")));
    }

    #[test]
    fn on_init_caches_account_id() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        strat.on_init(&ctx);
        assert_eq!(strat.account_id, Some(100));
    }

    // ── Multi-symbol scenarios ──────────────────────────────────────────

    #[test]
    fn timer_refreshes_multiple_symbols() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);
        strat.on_tick(&make_tick("ETH-USDT", 50.0, 51.0), &ctx);

        let timer_event = TimerEvent {
            timer_key: TIMER_KEY.to_string(),
            ts_ms: 5000,
        };
        let actions = strat.on_timer(&timer_event, &ctx);

        // 2 symbols x (2 cancels + 2 orders) = 8 order actions
        assert_eq!(count_cancels(&actions), 4);
        assert_eq!(count_orders(&actions), 4);
    }

    #[test]
    fn pause_cancels_across_all_symbols() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);
        strat.on_tick(&make_tick("ETH-USDT", 50.0, 51.0), &ctx);

        let actions = strat.on_pause("test", &ctx);
        assert_eq!(count_cancels(&actions), 4);
    }

    #[test]
    fn resume_requotes_all_symbols() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);
        strat.on_tick(&make_tick("ETH-USDT", 50.0, 51.0), &ctx);
        strat.on_pause("test", &ctx);

        let actions = strat.on_resume("test", &ctx);
        assert_eq!(count_orders(&actions), 4);
    }

    // ── Order ID uniqueness ─────────────────────────────────────────────

    #[test]
    fn cancel_replace_cycle_produces_unique_order_ids() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        let actions1 = strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);
        let ids1 = get_order_ids(&actions1);

        let timer_event = TimerEvent {
            timer_key: TIMER_KEY.to_string(),
            ts_ms: 5000,
        };
        let actions2 = strat.on_timer(&timer_event, &ctx);
        let ids2 = get_order_ids(&actions2);

        // All 4 IDs should be distinct.
        let mut all_ids = ids1;
        all_ids.extend(ids2);
        let unique: std::collections::HashSet<i64> = all_ids.iter().copied().collect();
        assert_eq!(unique.len(), all_ids.len());
    }

    // ── Edge cases ──────────────────────────────────────────────────────

    #[test]
    fn unknown_order_update_is_ignored() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);

        let oue = make_oue_filled(999, 1.0, 1.0);
        let actions = strat.on_order_update(&oue, &ctx);
        assert!(actions.is_empty());
    }

    #[test]
    fn rejected_order_clears_tracking() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);

        let sell_id = strat.symbols["BTC-USDT"].sell_order_id.unwrap();
        let oue = OrderUpdateEvent {
            order_id: sell_id,
            account_id: 100,
            order_snapshot: Some(Order {
                order_id: sell_id,
                order_status: OrderStatus::Rejected as i32,
                ..Default::default()
            }),
            ..Default::default()
        };
        strat.on_order_update(&oue, &ctx);
        assert!(strat.symbols["BTC-USDT"].sell_order_id.is_none());
        // Buy side untouched.
        assert!(strat.symbols["BTC-USDT"].buy_order_id.is_some());
    }

    #[test]
    fn one_side_filled_one_cancelled_requotes_when_both_slots_empty() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);

        let buy_id = strat.symbols["BTC-USDT"].buy_order_id.unwrap();
        let sell_id = strat.symbols["BTC-USDT"].sell_order_id.unwrap();

        strat.on_order_update(&make_oue_filled(buy_id, 1.0, 1.0), &ctx);
        let actions = strat.on_order_update(&make_oue_cancelled(sell_id), &ctx);

        // Both slots empty → re-quote (strategy doesn't distinguish
        // how the slot was cleared, it just knows it needs fresh quotes).
        assert_eq!(count_orders(&actions), 2);
    }

    #[test]
    fn both_filled_while_paused_does_not_requote() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);

        let buy_id = strat.symbols["BTC-USDT"].buy_order_id.unwrap();
        let sell_id = strat.symbols["BTC-USDT"].sell_order_id.unwrap();

        strat.on_pause("test", &ctx);

        // Fills arrive while paused (OMS events still flow).
        strat.on_order_update(&make_oue_filled(buy_id, 1.0, 1.0), &ctx);
        let actions = strat.on_order_update(&make_oue_filled(sell_id, 1.0, 1.0), &ctx);

        // Should NOT re-quote while paused.
        assert_eq!(count_orders(&actions), 0);
    }

    #[test]
    fn pause_with_no_active_orders_is_safe() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        // No ticks received, no symbols known.
        let actions = strat.on_pause("test", &ctx);
        assert_eq!(count_cancels(&actions), 0);
    }

    #[test]
    fn resume_before_any_ticks_does_not_quote() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        strat.on_pause("test", &ctx);
        let actions = strat.on_resume("test", &ctx);
        assert_eq!(count_orders(&actions), 0);
    }

    #[test]
    fn unrelated_timer_key_is_ignored() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);

        let timer_event = TimerEvent {
            timer_key: "other/timer".to_string(),
            ts_ms: 5000,
        };
        let actions = strat.on_timer(&timer_event, &ctx);
        assert!(actions.is_empty());
    }

    // ── Partial fill tracking ───────────────────────────────────────────

    fn make_oue_partial(order_id: i64, filled_qty: f64, trade_qty: f64) -> OrderUpdateEvent {
        OrderUpdateEvent {
            order_id,
            account_id: 100,
            order_snapshot: Some(Order {
                order_id,
                order_status: OrderStatus::PartiallyFilled as i32,
                filled_qty,
                ..Default::default()
            }),
            last_trade: if trade_qty > 0.0 {
                Some(Trade {
                    order_id,
                    filled_qty: trade_qty,
                    ..Default::default()
                })
            } else {
                None
            },
            ..Default::default()
        }
    }

    #[test]
    fn partial_fill_does_not_clear_order_tracking() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);

        let buy_id = strat.symbols["BTC-USDT"].buy_order_id.unwrap();
        strat.on_order_update(&make_oue_partial(buy_id, 0.3, 0.3), &ctx);

        // Order should still be tracked.
        assert_eq!(strat.symbols["BTC-USDT"].buy_order_id, Some(buy_id));
        assert_eq!(strat.symbols["BTC-USDT"].internal_position, 0.3);
    }

    #[test]
    fn multiple_partial_fills_accumulate_via_snapshot_delta() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);

        let buy_id = strat.symbols["BTC-USDT"].buy_order_id.unwrap();

        // First partial: 0.3 (no last_trade, uses snapshot delta)
        strat.on_order_update(&make_oue_partial(buy_id, 0.3, 0.0), &ctx);
        assert_eq!(strat.symbols["BTC-USDT"].internal_position, 0.3);

        // Second partial: snapshot says 0.7 total, delta = 0.4
        strat.on_order_update(&make_oue_partial(buy_id, 0.7, 0.0), &ctx);
        assert!((strat.symbols["BTC-USDT"].internal_position - 0.7).abs() < 1e-10);

        // Final fill
        strat.on_order_update(&make_oue_filled(buy_id, 1.0, 0.0), &ctx);
        assert!((strat.symbols["BTC-USDT"].internal_position - 1.0).abs() < 1e-10);
    }

    #[test]
    fn partial_fills_via_last_trade_accumulate() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);

        let sell_id = strat.symbols["BTC-USDT"].sell_order_id.unwrap();

        strat.on_order_update(&make_oue_partial(sell_id, 0.4, 0.4), &ctx);
        assert_eq!(strat.symbols["BTC-USDT"].internal_position, -0.4);

        strat.on_order_update(&make_oue_partial(sell_id, 0.7, 0.3), &ctx);
        assert!((strat.symbols["BTC-USDT"].internal_position - (-0.7)).abs() < 1e-10);
    }

    // ── Position after full cycle ───────────────────────────────────────

    #[test]
    fn full_cycle_buy_sell_position_nets_to_zero() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);

        let buy_id = strat.symbols["BTC-USDT"].buy_order_id.unwrap();
        let sell_id = strat.symbols["BTC-USDT"].sell_order_id.unwrap();

        strat.on_order_update(&make_oue_filled(buy_id, 1.0, 1.0), &ctx);
        strat.on_order_update(&make_oue_filled(sell_id, 1.0, 1.0), &ctx);

        // Buy +1 and sell -1 = net 0.
        assert!((strat.symbols["BTC-USDT"].internal_position).abs() < 1e-10);
    }

    // ── Order placement details ─────────────────────────────────────────

    #[test]
    fn orders_use_correct_account_id() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        let actions = strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);

        for action in &actions {
            if let SAction::PlaceOrder(o) = action {
                assert_eq!(o.account_id, 100);
            }
        }
    }

    #[test]
    fn cancel_uses_correct_order_ids() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);

        let buy_id = strat.symbols["BTC-USDT"].buy_order_id.unwrap();
        let sell_id = strat.symbols["BTC-USDT"].sell_order_id.unwrap();

        let actions = strat.on_pause("test", &ctx);
        let cancel_ids: Vec<i64> = actions
            .iter()
            .filter_map(|a| match a {
                SAction::Cancel(c) => Some(c.order_id),
                _ => None,
            })
            .collect();
        assert!(cancel_ids.contains(&buy_id));
        assert!(cancel_ids.contains(&sell_id));
    }

    // ── Timer interval config ───────────────────────────────────────────

    #[test]
    fn timer_rearms_at_configured_interval() {
        let cfg = SmokeMMConfig {
            quote_interval_ms: 2000,
            ..Default::default()
        };
        let mut strat = SmokeTestStrategy::with_config(cfg);
        let ctx = make_ctx();

        let timer_event = TimerEvent {
            timer_key: TIMER_KEY.to_string(),
            ts_ms: 10_000,
        };
        let actions = strat.on_timer(&timer_event, &ctx);

        let sub = actions.iter().find_map(|a| match a {
            SAction::SubscribeTimer(s) => Some(s),
            _ => None,
        });
        assert!(sub.is_some());
        match &sub.unwrap().schedule {
            TimerSchedule::OnceAt(fire_ms) => assert_eq!(*fire_ms, 12_000),
            other => panic!("expected OnceAt, got {:?}", other),
        }
    }

    // ── Reinit timer offset ─────────────────────────────────────────────

    #[test]
    fn reinit_timer_offset_from_current_ts() {
        let mut strat = SmokeTestStrategy::new();
        let mut ctx = make_ctx();
        ctx.current_ts_ms = 5000;

        let actions = strat.on_reinit(&ctx);
        let sub = actions.iter().find_map(|a| match a {
            SAction::SubscribeTimer(s) => Some(s),
            _ => None,
        });
        match &sub.unwrap().schedule {
            TimerSchedule::OnceAt(fire_ms) => assert_eq!(*fire_ms, 10_000),
            other => panic!("expected OnceAt, got {:?}", other),
        }
    }

    // ── Symbol discovered while paused gets quoted on resume ────────────

    #[test]
    fn symbol_seen_while_paused_is_quoted_on_resume() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        strat.on_init(&ctx);
        strat.on_pause("test", &ctx);

        // New symbol arrives while paused — BBO is cached, no quotes placed.
        let actions = strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);
        assert_eq!(count_orders(&actions), 0);
        assert!(strat.symbols.contains_key("BTC-USDT"));

        // Resume should quote the cached symbol.
        let actions = strat.on_resume("test", &ctx);
        assert_eq!(count_orders(&actions), 2);
    }

    // ── last_filled_qty cleanup on cancel ───────────────────────────────

    #[test]
    fn cancel_cleans_up_last_filled_qty() {
        let mut strat = SmokeTestStrategy::new();
        let ctx = make_ctx();
        strat.on_tick(&make_tick("BTC-USDT", 100.0, 101.0), &ctx);

        let buy_id = strat.symbols["BTC-USDT"].buy_order_id.unwrap();

        // Simulate a partial fill to populate last_filled_qty.
        strat.on_order_update(&make_oue_partial(buy_id, 0.3, 0.0), &ctx);
        assert!(strat.symbols["BTC-USDT"]
            .last_filled_qty
            .contains_key(&buy_id));

        // Timer cancel+replace should clean up the old entry.
        let timer_event = TimerEvent {
            timer_key: TIMER_KEY.to_string(),
            ts_ms: 5000,
        };
        strat.on_timer(&timer_event, &ctx);
        assert!(!strat.symbols["BTC-USDT"]
            .last_filled_qty
            .contains_key(&buy_id));
    }
}
