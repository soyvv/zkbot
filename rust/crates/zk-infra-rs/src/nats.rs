use async_nats::Client;

/// Connect to NATS server at `url`. Retries are handled by the async-nats client.
pub async fn connect(url: &str) -> Result<Client, async_nats::ConnectError> {
    async_nats::connect(url).await
}

/// Typed NATS subject constructors.
///
/// All subjects follow the pattern documented in `05-api-contracts.md`.
/// Using functions rather than string literals prevents typos and centralises
/// any future subject renames.
pub mod subject {
    // ── Gateway ───────────────────────────────────────────────────────────────

    /// `zk.gw.{gw_id}.report` — gateway OrderReport stream
    pub fn gw_report(gw_id: &str) -> String {
        format!("zk.gw.{gw_id}.report")
    }

    /// `zk.gw.{gw_id}.balance` — gateway BalanceUpdate stream
    pub fn gw_balance(gw_id: &str) -> String {
        format!("zk.gw.{gw_id}.balance")
    }

    // ── OMS ───────────────────────────────────────────────────────────────────

    /// `zk.oms.{oms_id}.order_update.{account_id}` — per-account order events
    pub fn oms_order_update(oms_id: &str, account_id: i64) -> String {
        format!("zk.oms.{oms_id}.order_update.{account_id}")
    }

    /// `zk.oms.{oms_id}.balance_update` — OMS balance/position updates
    pub fn oms_balance_update(oms_id: &str) -> String {
        format!("zk.oms.{oms_id}.balance_update")
    }

    /// `zk.oms.{oms_id}.position_update` — OMS position updates
    pub fn oms_position_update(oms_id: &str) -> String {
        format!("zk.oms.{oms_id}.position_update")
    }

    // ── RTMD ──────────────────────────────────────────────────────────────────

    /// `zk.rtmd.{venue}.tick.{instrument_exch}`
    pub fn rtmd_tick(venue: &str, instrument_exch: &str) -> String {
        format!("zk.rtmd.{venue}.tick.{instrument_exch}")
    }

    /// `zk.rtmd.{venue}.kline.{instrument_exch}.{interval}`
    pub fn rtmd_kline(venue: &str, instrument_exch: &str, interval: &str) -> String {
        format!("zk.rtmd.{venue}.kline.{instrument_exch}.{interval}")
    }

    /// `zk.rtmd.{venue}.orderbook.{instrument_exch}`
    pub fn rtmd_orderbook(venue: &str, instrument_exch: &str) -> String {
        format!("zk.rtmd.{venue}.orderbook.{instrument_exch}")
    }

    /// `zk.rtmd.{venue}.funding.{instrument_exch}`
    pub fn rtmd_funding(venue: &str, instrument_exch: &str) -> String {
        format!("zk.rtmd.{venue}.funding.{instrument_exch}")
    }

    // ── Control ───────────────────────────────────────────────────────────────

    /// `zk.control.{service_type}.{service_id}` — admin control commands
    pub fn control(service_type: &str, service_id: &str) -> String {
        format!("zk.control.{service_type}.{service_id}")
    }

    // ── Registry bootstrap ────────────────────────────────────────────────────

    /// `zk.bootstrap.{instance_type}.{logical_id}` — Pilot bootstrap request
    pub fn bootstrap_request(instance_type: &str, logical_id: &str) -> String {
        format!("zk.bootstrap.{instance_type}.{logical_id}")
    }
}

#[cfg(test)]
mod tests {
    use super::subject;

    #[test]
    fn test_gw_subjects() {
        assert_eq!(subject::gw_report("gw_mock_1"), "zk.gw.gw_mock_1.report");
        assert_eq!(subject::gw_balance("gw_mock_1"), "zk.gw.gw_mock_1.balance");
    }

    #[test]
    fn test_oms_subjects() {
        assert_eq!(
            subject::oms_order_update("oms_dev_1", 9001),
            "zk.oms.oms_dev_1.order_update.9001"
        );
        assert_eq!(
            subject::oms_balance_update("oms_dev_1"),
            "zk.oms.oms_dev_1.balance_update"
        );
        assert_eq!(
            subject::oms_position_update("oms_dev_1"),
            "zk.oms.oms_dev_1.position_update"
        );
    }

    #[test]
    fn test_rtmd_subjects() {
        assert_eq!(
            subject::rtmd_tick("OKX", "BTC-USDT"),
            "zk.rtmd.OKX.tick.BTC-USDT"
        );
        assert_eq!(
            subject::rtmd_kline("OKX", "BTC-USDT", "1m"),
            "zk.rtmd.OKX.kline.BTC-USDT.1m"
        );
        assert_eq!(
            subject::rtmd_funding("OKX", "BTC-USDT-SWAP"),
            "zk.rtmd.OKX.funding.BTC-USDT-SWAP"
        );
    }
}
