//! Event bridge — wires `TradingClient` NATS subscriptions into the engine's
//! event channels.
//!
//! Enqueue policy by event class:
//! - **Ticks**: lossy (`try_send` to engine channel) — ok to drop under
//!   backpressure, engine coalesces ticks anyway. Drops are counted.
//! - **OMS events & bars**: reliable (`try_send` to grace queue) — the grace
//!   forwarder drains into the engine channel with backpressure. Only drops
//!   (with loud logging) if the grace queue itself is full.
//!
//! All reliable events go through a single grace queue path to preserve
//! arrival ordering across non-tick event classes.

use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::mpsc;
use tracing::{error, info, warn};

use zk_engine_rs::{EngineEvent, EventEnvelope};
use zk_trading_sdk_rs::client::TradingClient;

/// Counter for tick events dropped due to channel backpressure.
pub static TICKS_DROPPED: AtomicU64 = AtomicU64::new(0);

/// Counter for non-lossy events dropped because the grace queue was full.
pub static GRACE_OVERFLOW: AtomicU64 = AtomicU64::new(0);

/// Enqueue a non-lossy event into the grace queue.
///
/// Returns `true` if enqueued, `false` if the grace queue is full or closed
/// (overflow — the event is lost and counted).
fn enqueue_reliable(grace_tx: &mpsc::Sender<EventEnvelope>, envelope: EventEnvelope) -> bool {
    match grace_tx.try_send(envelope) {
        Ok(()) => true,
        Err(mpsc::error::TrySendError::Full(_)) => {
            GRACE_OVERFLOW.fetch_add(1, Ordering::Relaxed);
            error!("grace queue overflow — non-lossy event dropped");
            false
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            warn!("grace queue closed — event lost");
            false
        }
    }
}

/// Subscribe to all OMS update channels (order, balance, position).
///
/// OMS events use `enqueue_reliable` via the grace queue.
pub async fn subscribe_oms_updates(
    client: &TradingClient,
    grace_tx: mpsc::Sender<EventEnvelope>,
) -> Vec<tokio::task::JoinHandle<()>> {
    let mut handles = Vec::new();

    // Order updates — reliable.
    let gtx = grace_tx.clone();
    let h = client
        .subscribe_order_updates(move |oue| {
            let envelope = EventEnvelope::now(EngineEvent::OrderUpdate(oue));
            enqueue_reliable(&gtx, envelope);
        })
        .await;
    handles.push(h);
    info!("subscribed to order updates");

    // Balance updates — reliable.
    let gtx = grace_tx.clone();
    let h = client
        .subscribe_balance_updates("*", move |bue| {
            let envelope = EventEnvelope::now(EngineEvent::BalanceUpdate(bue));
            enqueue_reliable(&gtx, envelope);
        })
        .await;
    handles.push(h);
    info!("subscribed to balance updates");

    // Position updates — reliable.
    let gtx = grace_tx.clone();
    let h = client
        .subscribe_position_updates("*", move |pue| {
            let envelope = EventEnvelope::now(EngineEvent::PositionUpdate(pue));
            enqueue_reliable(&gtx, envelope);
        })
        .await;
    handles.push(h);
    info!("subscribed to position updates");

    handles
}

/// Subscribe to RTMD tick data for each instrument.
///
/// Ticks use `try_send` (lossy) directly to the engine channel — drops are
/// acceptable and counted.
pub async fn subscribe_ticks(
    client: &TradingClient,
    instruments: &[String],
    engine_tx: mpsc::Sender<EventEnvelope>,
) -> Vec<tokio::task::JoinHandle<()>> {
    let mut handles = Vec::new();

    for instrument in instruments {
        let tx_tick = engine_tx.clone();
        match client
            .subscribe_ticks(instrument, move |tick| {
                let envelope = EventEnvelope::now(EngineEvent::Tick(tick));
                if tx_tick.try_send(envelope).is_err() {
                    TICKS_DROPPED.fetch_add(1, Ordering::Relaxed);
                }
            })
            .await
        {
            Ok(h) => {
                info!(instrument, "subscribed to ticks");
                handles.push(h);
            }
            Err(e) => {
                warn!(instrument, error = %e, "failed to subscribe to ticks");
            }
        }
    }

    handles
}

/// Subscribe to RTMD kline (bar) data for each instrument.
///
/// Bars use `enqueue_reliable` via the grace queue.
pub async fn subscribe_klines(
    client: &TradingClient,
    instruments: &[String],
    interval: &str,
    grace_tx: mpsc::Sender<EventEnvelope>,
) -> Vec<tokio::task::JoinHandle<()>> {
    let mut handles = Vec::new();

    for instrument in instruments {
        let gtx = grace_tx.clone();
        match client
            .subscribe_klines(instrument, interval, move |kline| {
                let envelope = EventEnvelope::now(EngineEvent::Bar(kline));
                enqueue_reliable(&gtx, envelope);
            })
            .await
        {
            Ok(h) => {
                info!(instrument, interval, "subscribed to klines");
                handles.push(h);
            }
            Err(e) => {
                warn!(instrument, error = %e, "failed to subscribe to klines");
            }
        }
    }

    handles
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(event: EngineEvent) -> EventEnvelope {
        EventEnvelope::now(event)
    }

    fn timer_event(ms: i64) -> EventEnvelope {
        envelope(EngineEvent::Timer(ms))
    }

    #[test]
    fn enqueue_reliable_succeeds_when_grace_has_capacity() {
        let (grace_tx, _grace_rx) = mpsc::channel(4);
        assert!(enqueue_reliable(&grace_tx, timer_event(1)));
        assert!(enqueue_reliable(&grace_tx, timer_event(2)));
    }

    #[test]
    fn enqueue_reliable_overflows_when_grace_full() {
        let (grace_tx, _grace_rx) = mpsc::channel(2);
        assert!(enqueue_reliable(&grace_tx, timer_event(1)));
        assert!(enqueue_reliable(&grace_tx, timer_event(2)));
        // Grace queue is now full — returns false.
        assert!(!enqueue_reliable(&grace_tx, timer_event(3)));
        assert!(!enqueue_reliable(&grace_tx, timer_event(4)));
    }

    #[test]
    fn enqueue_reliable_returns_false_when_closed() {
        let (grace_tx, grace_rx) = mpsc::channel(4);
        drop(grace_rx);
        assert!(!enqueue_reliable(&grace_tx, timer_event(1)));
    }

    #[test]
    fn tick_try_send_drops_on_full_channel() {
        let (engine_tx, _engine_rx) = mpsc::channel(1);
        // Fill the channel.
        let env = envelope(EngineEvent::Tick(Default::default()));
        assert!(engine_tx.try_send(env).is_ok());
        // Second send should fail (channel full).
        let env = envelope(EngineEvent::Tick(Default::default()));
        assert!(engine_tx.try_send(env).is_err());
    }
}
