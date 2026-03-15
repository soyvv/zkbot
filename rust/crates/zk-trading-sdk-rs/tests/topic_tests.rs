use zk_trading_sdk_rs::rtmd_sub::{funding_topic, kline_topic, orderbook_topic, tick_topic};
use zk_trading_sdk_rs::stream::{balance_update_topic, order_update_topic, position_update_topic};

// ── stream.rs ────────────────────────────────────────────────────────────────

#[test]
fn test_order_update_topic_format() {
    assert_eq!(
        order_update_topic("oms_dev_1", 9001),
        "zk.oms.oms_dev_1.order_update.9001"
    );
}

#[test]
fn test_balance_update_topic_format() {
    // Current OMS runtime publishes bare topic without asset suffix (api_contracts.md line 267).
    // The asset param is accepted but ignored until the OMS migrates to per-asset topics.
    assert_eq!(
        balance_update_topic("oms_dev_1", "USDT"),
        "zk.oms.oms_dev_1.balance_update"
    );
}

#[test]
fn test_position_update_topic_format() {
    assert_eq!(
        position_update_topic("oms_dev_1", "BTC-USDT-PERP"),
        "zk.oms.oms_dev_1.position_update.BTC-USDT-PERP"
    );
}

// ── rtmd_sub.rs ──────────────────────────────────────────────────────────────

#[test]
fn test_tick_topic_format() {
    assert_eq!(
        tick_topic("MOCK", "BTC-USDT"),
        "zk.rtmd.tick.MOCK.BTC-USDT"
    );
}

#[test]
fn test_kline_topic_format() {
    assert_eq!(
        kline_topic("MOCK", "BTC-USDT", "1m"),
        "zk.rtmd.kline.MOCK.BTC-USDT.1m"
    );
}

#[test]
fn test_funding_topic_format() {
    assert_eq!(
        funding_topic("MOCK", "BTC-USDT"),
        "zk.rtmd.funding.MOCK.BTC-USDT"
    );
}

#[test]
fn test_orderbook_topic_format() {
    assert_eq!(
        orderbook_topic("MOCK", "BTC-USDT"),
        "zk.rtmd.orderbook.MOCK.BTC-USDT"
    );
}
