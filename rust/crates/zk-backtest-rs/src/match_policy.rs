use zk_proto_rs::zk::{common::v1::BuySellType, rtmd::v1::TickData};

use crate::models::{MatchResult, SimOrder};

/// Decides how a pending order is matched against market data.
pub trait MatchPolicy: Send + 'static {
    /// Returns zero or more fill results for `order` given the current `tick`.
    /// `tick` is `None` when the policy does not require market data.
    fn match_order(&self, order: &SimOrder, tick: Option<&TickData>) -> Vec<MatchResult>;

    /// Whether this policy needs tick (orderbook) data to function.
    fn need_tick(&self) -> bool {
        true
    }
}

/// Immediately fills any order at its requested price, one millisecond after placement.
/// Does not require tick data.
pub struct ImmediateMatchPolicy;

impl MatchPolicy for ImmediateMatchPolicy {
    fn match_order(&self, order: &SimOrder, _tick: Option<&TickData>) -> Vec<MatchResult> {
        vec![MatchResult {
            trigger_ts: order.req.timestamp + 1,
            qty: order.remaining_qty,
            price: order.req.scaled_price,
            delay_ms: 0,
            ioc_cancel: false,
        }]
    }

    fn need_tick(&self) -> bool {
        false
    }
}

/// Matches buy orders against ask levels and sell orders against bid levels,
/// walking levels greedily. Requires tick (orderbook) data.
pub struct FirstComeFirstServedMatchPolicy;

impl MatchPolicy for FirstComeFirstServedMatchPolicy {
    fn match_order(&self, order: &SimOrder, tick: Option<&TickData>) -> Vec<MatchResult> {
        let tick = match tick {
            Some(t) => t,
            None => return vec![],
        };

        let ts = tick.original_timestamp;
        let limit_price = order.req.scaled_price;
        let mut remaining = order.remaining_qty;
        let mut results = Vec::new();
        let mut delay = 10i64;

        let is_buy = order.req.buysell_type == BuySellType::BsBuy as i32;

        if is_buy {
            // buy: walk ask levels low-to-high
            for level in &tick.sell_price_levels {
                if remaining <= 0.0 {
                    break;
                }
                if level.price <= limit_price {
                    let fill_qty = remaining.min(level.qty);
                    results.push(MatchResult {
                        trigger_ts: ts,
                        qty: fill_qty,
                        price: level.price,
                        delay_ms: delay,
                        ioc_cancel: false,
                    });
                    remaining -= fill_qty;
                    delay += 10;
                }
            }
        } else {
            // sell: walk bid levels high-to-low
            for level in &tick.buy_price_levels {
                if remaining <= 0.0 {
                    break;
                }
                if level.price >= limit_price {
                    let fill_qty = remaining.min(level.qty);
                    results.push(MatchResult {
                        trigger_ts: ts,
                        qty: fill_qty,
                        price: level.price,
                        delay_ms: delay,
                        ioc_cancel: false,
                    });
                    remaining -= fill_qty;
                    delay += 10;
                }
            }
        }

        results
    }
}
