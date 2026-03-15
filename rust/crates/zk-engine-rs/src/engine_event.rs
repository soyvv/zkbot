/// `EngineEvent` — unified event type flowing through the live engine's channel.
///
/// Mirrors the asyncio.Queue payload types from Python `StrategyRealtimeEngine`.
/// All NATS subscription callbacks and the timer clock funnel events into a
/// `tokio::sync::mpsc::Sender<EngineEvent>` so the engine loop processes
/// them serially, one at a time (no concurrent strategy calls).
use std::collections::HashMap;

use zk_proto_rs::zk::{
    oms::v1::{BalanceUpdateEvent, OrderUpdateEvent, PositionUpdateEvent},
    rtmd::v1::{Kline, RealtimeSignal, TickData},
};

#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// Market tick (bid/ask/last). Multiple ticks for the same symbol within
    /// a batch are coalesced: only the last one is dispatched to the strategy.
    Tick(TickData),
    /// OHLCV bar (1-minute or any kline type).
    Bar(Kline),
    /// Order fill, rejection, or status change from OMS.
    OrderUpdate(OrderUpdateEvent),
    /// Asset inventory (cash/spot) update from OMS.
    BalanceUpdate(BalanceUpdateEvent),
    /// Instrument exposure (derivatives) update from OMS.
    PositionUpdate(PositionUpdateEvent),
    /// External real-time signal.
    Signal(RealtimeSignal),
    /// Timer clock pulse. The engine passes `now_ms` to `StrategyRunner::advance_time`,
    /// which drains all due timers and calls `on_timer` for each.
    Timer(i64),
}

/// Deduplicate ticks: for each `(symbol, exchange)` pair, keep only the last.
/// All non-tick events are passed through in original order.
///
/// Mirrors Python engine's coalescing in `run_strategy()`:
/// `deduplicated_ticks: dict[(symbol, exchange) -> TickData]`
pub fn coalesce_ticks(events: Vec<EngineEvent>) -> Vec<EngineEvent> {
    // Pass 1: record the index of the *last* occurrence of each (symbol, exchange) pair.
    let mut last_seen: HashMap<(String, String), usize> = HashMap::new();
    for (i, ev) in events.iter().enumerate() {
        if let EngineEvent::Tick(t) = ev {
            last_seen.insert((t.instrument_code.clone(), t.exchange.clone()), i);
        }
    }

    // Convert to a HashSet of keep-indices so the filter pass needs no string allocation.
    let keep: std::collections::HashSet<usize> = last_seen.into_values().collect();

    // Pass 2: emit non-tick events unconditionally; emit ticks only at their last index.
    events
        .into_iter()
        .enumerate()
        .filter(|(i, ev)| !matches!(ev, EngineEvent::Tick(_)) || keep.contains(i))
        .map(|(_, ev)| ev)
        .collect()
}
