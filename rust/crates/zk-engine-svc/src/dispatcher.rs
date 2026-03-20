//! Production `ActionDispatcher` — routes strategy actions to OMS via `TradingClient`.

use std::sync::Arc;

use tracing::{info, warn};

use zk_engine_rs::{ActionDispatcher, TriggerContext};
use zk_strategy_sdk_rs::models::{StrategyCancel, StrategyLog, StrategyOrder};
use zk_trading_sdk_rs::{
    client::TradingClient,
    model::{OrderType, Side, TradingCancel, TradingOrder},
};

/// Routes strategy actions to OMS via `TradingClient`.
///
/// `place_order` and `cancel_order` are synchronous (required by `ActionDispatcher` trait),
/// so we spawn a tokio task for the actual async OMS call. This keeps the engine hot loop
/// non-blocking.
pub struct TradingDispatcher {
    trading_client: Arc<TradingClient>,
    source_id: String,
}

impl TradingDispatcher {
    pub fn new(trading_client: Arc<TradingClient>, source_id: String) -> Self {
        Self {
            trading_client,
            source_id,
        }
    }
}

impl ActionDispatcher for TradingDispatcher {
    fn place_order(&mut self, order: &StrategyOrder, ctx: &TriggerContext) {
        let submit_ts = zk_engine_rs::system_time_ns();
        let mut trading_order = map_strategy_order(order, &self.source_id);
        trading_order.trigger_context = Some(ctx.to_proto(submit_ts, &order.order_id.to_string()));
        let client = Arc::clone(&self.trading_client);
        let account_id = order.account_id;
        let order_id = order.order_id;
        let exec_id = ctx.execution_id.clone();

        tokio::spawn(async move {
            match client.place_order(account_id, trading_order).await {
                Ok(ack) => {
                    if !ack.success {
                        warn!(
                            order_id,
                            execution_id = %exec_id,
                            msg = %ack.message,
                            "OMS rejected order"
                        );
                    }
                }
                Err(e) => {
                    warn!(order_id, execution_id = %exec_id, error = %e, "place_order failed");
                }
            }
        });
    }

    fn cancel_order(&mut self, cancel: &StrategyCancel, ctx: &TriggerContext) {
        let submit_ts = zk_engine_rs::system_time_ns();
        let trading_cancel = TradingCancel {
            order_id: cancel.order_id,
            exch_order_ref: String::new(),
            source_id: self.source_id.clone(),
            trigger_context: Some(ctx.to_proto(submit_ts, &cancel.order_id.to_string())),
        };
        let client = Arc::clone(&self.trading_client);
        let account_id = cancel.account_id;
        let order_id = cancel.order_id;
        let exec_id = ctx.execution_id.clone();

        tokio::spawn(async move {
            match client.cancel_order(account_id, trading_cancel).await {
                Ok(ack) => {
                    if !ack.success {
                        warn!(
                            order_id,
                            execution_id = %exec_id,
                            msg = %ack.message,
                            "OMS rejected cancel"
                        );
                    }
                }
                Err(e) => {
                    warn!(order_id, execution_id = %exec_id, error = %e, "cancel_order failed");
                }
            }
        });
    }

    fn log(&mut self, log: &StrategyLog) {
        info!(ts_ms = log.ts_ms, msg = %log.message, "strategy_log");
    }
}

/// Map `StrategyOrder` → `TradingOrder`.
fn map_strategy_order(order: &StrategyOrder, source_id: &str) -> TradingOrder {
    use zk_proto_rs::zk::common::v1::BuySellType;

    let side = if order.side == BuySellType::BsBuy as i32 {
        Side::Buy
    } else {
        Side::Sell
    };

    TradingOrder {
        order_id: order.order_id,
        instrument_id: order.symbol.clone(),
        side,
        order_type: OrderType::Limit,
        price: order.price,
        qty: order.qty,
        source_id: source_id.to_string(),
        trigger_context: None, // Set by caller after construction.
    }
}
