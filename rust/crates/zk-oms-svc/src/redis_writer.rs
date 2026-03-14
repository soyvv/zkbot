//! Redis state persistence for the OMS.
//!
//! Written after every mutation so that a restarted OMS can warm-start from
//! Redis rather than querying the exchange for all open orders.
//!
//! # Key schema (see `06-data-layer.md §4`)
//!
//! ```text
//! oms:{oms_id}:order:{order_id}             HASH  → serialised PersistedOrder
//! oms:{oms_id}:open_orders:{account_id}     SET   → {order_id, ...}
//! oms:{oms_id}:balance:{account_id}:{asset} STRING → JSON Position snapshot
//! oms:{oms_id}:position:{account_id}:{instrument}:{side} STRING → JSON Position
//! ```
//!
//! # Performance
//! Each write uses a Redis pipeline (`redis::pipe()`) to batch commands into a
//! single round-trip.  Pipelining reduces latency from ~2 RTTs to ~1 per mutation.

use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use tracing::warn;

use zk_infra_rs::redis::key;
use zk_oms_rs::models::{OmsOrder, OmsPosition};

/// Thin wrapper around a multiplexed Redis connection.
pub struct RedisWriter {
    conn: redis::aio::MultiplexedConnection,
    oms_id: String,
}

impl RedisWriter {
    pub fn new(conn: redis::aio::MultiplexedConnection, oms_id: impl Into<String>) -> Self {
        Self { conn, oms_id: oms_id.into() }
    }

    // ── Order persistence ────────────────────────────────────────────────────

    /// Persist full order state to Redis.
    ///
    /// - Open orders get a 24h TTL.
    /// - Terminal orders get a 1h TTL (`set_expire = true`).
    /// - Removes from the `open_orders` set when `set_closed = true`.
    pub async fn write_order(
        &mut self,
        order: &OmsOrder,
        set_expire: bool,
        set_closed: bool,
    ) -> Result<(), RedisWriterError> {
        let order_key  = key::order(&self.oms_id, order.order_id);
        let open_key   = key::open_orders(&self.oms_id, order.account_id);
        let order_ttl  = if set_expire { 3_600i64 } else { 86_400i64 };
        let open_ttl   = 86_400i64;

        let persisted = PersistedOrder::from_oms_order(order);
        let payload   = serde_json::to_vec(&persisted).map_err(RedisWriterError::Serialize)?;

        let mut pipe = redis::pipe();
        pipe.set(&order_key, payload.as_slice());
        pipe.expire(&order_key, order_ttl);
        if set_closed {
            pipe.srem(&open_key, order.order_id);
        } else {
            pipe.sadd(&open_key, order.order_id);
            pipe.expire(&open_key, open_ttl);
        }
        pipe.query_async::<()>(&mut self.conn).await.map_err(RedisWriterError::Redis)?;
        Ok(())
    }

    // ── Balance / position persistence ──────────────────────────────────────

    /// Persist a balance snapshot for a single asset.
    pub async fn write_balance(
        &mut self,
        account_id: i64,
        asset: &str,
        pos: &OmsPosition,
    ) -> Result<(), RedisWriterError> {
        let k       = key::balance(&self.oms_id, account_id, asset);
        let payload = serde_json::to_vec(&PersistedPosition::from(pos))
            .map_err(RedisWriterError::Serialize)?;
        self.conn
            .set_ex::<_, _, ()>(k, payload, 86_400u64)
            .await
            .map_err(RedisWriterError::Redis)?;
        Ok(())
    }

    /// Persist a position snapshot.
    pub async fn write_position(
        &mut self,
        account_id: i64,
        instrument: &str,
        side: &str,
        pos: &OmsPosition,
    ) -> Result<(), RedisWriterError> {
        let k       = key::position(&self.oms_id, account_id, instrument, side);
        let payload = serde_json::to_vec(&PersistedPosition::from(pos))
            .map_err(RedisWriterError::Serialize)?;
        self.conn
            .set_ex::<_, _, ()>(k, payload, 86_400u64)
            .await
            .map_err(RedisWriterError::Redis)?;
        Ok(())
    }

    // ── Warm-start loader ────────────────────────────────────────────────────

    /// Load all persisted orders from Redis for this OMS instance.
    ///
    /// Called at startup before `OmsCore::init_state`.  Orders whose Redis
    /// TTL has expired (i.e. terminal orders older than 1h) are silently skipped.
    pub async fn load_orders(
        &mut self,
    ) -> Result<Vec<PersistedOrder>, RedisWriterError> {
        // Collect all open order IDs across accounts via SCAN, then load each.
        // Pattern: oms:{oms_id}:open_orders:*
        let pattern = format!("oms:{}:open_orders:*", self.oms_id);
        let mut cursor = 0u64;
        let mut order_ids: Vec<i64> = Vec::new();

        loop {
            let (next_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(100u64)
                .query_async(&mut self.conn)
                .await
                .map_err(RedisWriterError::Redis)?;

            for key_str in keys {
                let ids: Vec<i64> = self
                    .conn
                    .smembers(&key_str)
                    .await
                    .map_err(RedisWriterError::Redis)?;
                order_ids.extend(ids);
            }
            cursor = next_cursor;
            if cursor == 0 { break; }
        }

        let mut orders = Vec::with_capacity(order_ids.len());
        for order_id in order_ids {
            let k = key::order(&self.oms_id, order_id);
            let payload: Option<Vec<u8>> = self
                .conn
                .get(&k)
                .await
                .map_err(RedisWriterError::Redis)?;
            if let Some(bytes) = payload {
                match serde_json::from_slice::<PersistedOrder>(&bytes) {
                    Ok(o) => orders.push(o),
                    Err(e) => warn!(order_id, error = %e, "failed to deserialise persisted order"),
                }
            }
        }
        Ok(orders)
    }
}

// ── Persisted data types ─────────────────────────────────────────────────────

/// Serialised form of `OmsOrder` stored in Redis.
///
/// Captures the minimum fields needed to reconstruct a warm-start `OmsOrder`.
/// Using JSON for human readability; proto encoding can replace it for perf.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedOrder {
    pub order_id:          i64,
    pub account_id:        i64,
    pub is_from_external:  bool,
    pub exch_order_ref:    Option<String>,
    // Core order state fields (flattened from proto Order)
    pub instrument:        String,
    pub buy_sell_type:     i32,
    pub open_close_type:   i32,
    pub price:             f64,
    pub qty:               f64,
    pub filled_qty:        f64,
    pub filled_avg_price:  f64,
    pub order_status:      i32,
    pub gw_key:            String,
    pub source_id:         String,
    pub created_at:        i64,
    pub updated_at:        i64,
    pub error_msg:         String,
    pub instrument_exch:   String,
}

impl PersistedOrder {
    pub fn from_oms_order(o: &OmsOrder) -> Self {
        let s = &o.order_state;
        Self {
            order_id:         o.order_id,
            account_id:       o.account_id,
            is_from_external: o.is_from_external,
            exch_order_ref:   o.exch_order_ref.clone(),
            instrument:       s.instrument.clone(),
            buy_sell_type:    s.buy_sell_type,
            open_close_type:  s.open_close_type,
            price:            s.price,
            qty:              s.qty,
            filled_qty:       s.filled_qty,
            filled_avg_price: s.filled_avg_price,
            order_status:     s.order_status,
            gw_key:           s.gw_key.clone(),
            source_id:        s.source_id.clone(),
            created_at:       s.created_at,
            updated_at:       s.updated_at,
            error_msg:        s.error_msg.clone(),
            instrument_exch:  s.instrument_exch.clone(),
        }
    }

    /// Reconstruct an `OmsOrder` from this persisted snapshot.
    ///
    /// Used at warm-start.  Sets `oms_req`, `gw_req`, `cancel_req` to `None`
    /// (they are not persisted) and leaves trade lists empty.
    pub fn into_oms_order(self) -> OmsOrder {
        use zk_proto_rs::zk::oms::v1::Order;
        let mut order_state = Order::default();
        order_state.order_id        = self.order_id;
        order_state.account_id      = self.account_id;
        order_state.exch_order_ref  = self.exch_order_ref.clone().unwrap_or_default();
        order_state.instrument      = self.instrument;
        order_state.buy_sell_type   = self.buy_sell_type;
        order_state.open_close_type = self.open_close_type;
        order_state.price           = self.price;
        order_state.qty             = self.qty;
        order_state.filled_qty      = self.filled_qty;
        order_state.filled_avg_price = self.filled_avg_price;
        order_state.order_status    = self.order_status;
        order_state.gw_key          = self.gw_key;
        order_state.source_id       = self.source_id;
        order_state.created_at      = self.created_at;
        order_state.updated_at      = self.updated_at;
        order_state.error_msg       = self.error_msg;
        order_state.instrument_exch = self.instrument_exch;

        OmsOrder {
            is_from_external:        self.is_from_external,
            order_id:                self.order_id,
            account_id:              self.account_id,
            exch_order_ref:          self.exch_order_ref,
            oms_req:                 None,
            gw_req:                  None,
            cancel_req:              None,
            order_state,
            trades:                  vec![],
            acc_trades_filled_qty:   0.0,
            acc_trades_value:        0.0,
            order_inferred_trades:   vec![],
            exec_msgs:               vec![],
            fees:                    vec![],
            cancel_attempts:         0,
        }
    }
}

/// Serialised form of `OmsPosition` stored in Redis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedPosition {
    pub account_id:    i64,
    pub symbol:        String,
    pub symbol_exch:   Option<String>,
    pub is_short:      bool,
    pub total_qty:     f64,
    pub frozen_qty:    f64,
    pub avail_qty:     f64,
    pub is_from_exch:  bool,
}

impl From<&OmsPosition> for PersistedPosition {
    fn from(p: &OmsPosition) -> Self {
        Self {
            account_id:   p.account_id,
            symbol:       p.symbol.clone(),
            symbol_exch:  p.symbol_exch.clone(),
            is_short:     p.is_short,
            total_qty:    p.position_state.total_qty,
            frozen_qty:   p.position_state.frozen_qty,
            avail_qty:    p.position_state.avail_qty,
            is_from_exch: p.position_state.is_from_exch,
        }
    }
}

// ── Error type ───────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum RedisWriterError {
    #[error("Redis error: {0}")]
    Redis(#[from] redis::RedisError),
    #[error("Serialisation error: {0}")]
    Serialize(#[from] serde_json::Error),
}
