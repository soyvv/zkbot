use redis::aio::MultiplexedConnection;
use redis::Client;

/// Connect to Redis and return a multiplexed async connection.
pub async fn connect(url: &str) -> Result<MultiplexedConnection, redis::RedisError> {
    Client::open(url)?.get_multiplexed_tokio_connection().await
}

/// Typed Redis key constructors for all OMS state keys.
///
/// Key patterns match the schema defined in `06-data-layer.md §4`.
pub mod key {
    /// `oms:{oms_id}:order:{order_id}` — serialised order state snapshot.
    /// TTL: 1d for open orders, 1h for terminal orders (set by OMS on write).
    pub fn order(oms_id: &str, order_id: i64) -> String {
        format!("oms:{oms_id}:order:{order_id}")
    }

    /// `oms:{oms_id}:open_orders:{account_id}` — Redis set of open order IDs.
    pub fn open_orders(oms_id: &str, account_id: i64) -> String {
        format!("oms:{oms_id}:open_orders:{account_id}")
    }

    /// `oms:{oms_id}:balance:{account_id}:{asset}` — balance snapshot per asset.
    pub fn balance(oms_id: &str, account_id: i64, asset: &str) -> String {
        format!("oms:{oms_id}:balance:{account_id}:{asset}")
    }

    /// `oms:{oms_id}:position:{account_id}:{instrument}:{side}` — position snapshot.
    pub fn position(oms_id: &str, account_id: i64, instrument: &str, side: &str) -> String {
        format!("oms:{oms_id}:position:{account_id}:{instrument}:{side}")
    }

    /// `rtmd:orderbook:{venue}:{instrument_exch}` — latest orderbook snapshot (30s TTL).
    pub fn rtmd_orderbook(venue: &str, instrument_exch: &str) -> String {
        format!("rtmd:orderbook:{venue}:{instrument_exch}")
    }

    /// `rate:{service_id}:{op}` — sliding-window rate counter.
    pub fn rate(service_id: &str, op: &str) -> String {
        format!("rate:{service_id}:{op}")
    }
}

#[cfg(test)]
mod tests {
    use super::key;

    #[test]
    fn test_order_key() {
        assert_eq!(key::order("oms_dev_1", 123), "oms:oms_dev_1:order:123");
    }

    #[test]
    fn test_open_orders_key() {
        assert_eq!(
            key::open_orders("oms_dev_1", 9001),
            "oms:oms_dev_1:open_orders:9001"
        );
    }

    #[test]
    fn test_balance_key() {
        assert_eq!(
            key::balance("oms_dev_1", 9001, "BTC"),
            "oms:oms_dev_1:balance:9001:BTC"
        );
    }

    #[test]
    fn test_position_key() {
        assert_eq!(
            key::position("oms_dev_1", 9001, "BTC-USDT-PERP", "LONG"),
            "oms:oms_dev_1:position:9001:BTC-USDT-PERP:LONG"
        );
    }
}
