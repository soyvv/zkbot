use serde_json::Value;
use zk_proto_rs::zk::rtmd::v1::{
    kline::KlineType, FundingRate, Kline, OrderBook, PriceLevel, TickData, TickUpdateType,
};

use crate::rest::{OkxFundingData, OkxTickerData};

fn parse_f64(s: &str) -> f64 {
    s.parse().unwrap_or(0.0)
}

fn parse_i64(s: &str) -> i64 {
    s.parse().unwrap_or(0)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

// ── WebSocket normalizers ───────────────────────────────────────────────────

/// Normalize OKX `tickers` channel push → TickData.
pub fn normalize_ws_ticker(
    data: &Value,
    instrument_code: &str,
    exchange: &str,
) -> Option<TickData> {
    let last = data["last"].as_str()?;
    Some(TickData {
        tick_type: TickUpdateType::M as i32,
        instrument_code: instrument_code.to_string(),
        exchange: exchange.to_string(),
        original_timestamp: parse_i64(data["ts"].as_str().unwrap_or("0")),
        received_timestamp: now_ms(),
        volume: parse_f64(data["vol24h"].as_str().unwrap_or("0")),
        latest_trade_price: parse_f64(last),
        latest_trade_qty: parse_f64(data["lastSz"].as_str().unwrap_or("0")),
        latest_trade_side: String::new(),
        buy_price_levels: vec![PriceLevel {
            price: parse_f64(data["bidPx"].as_str().unwrap_or("0")),
            qty: parse_f64(data["bidSz"].as_str().unwrap_or("0")),
            num_of_orders: 0,
            num_of_pos: 0,
        }],
        sell_price_levels: vec![PriceLevel {
            price: parse_f64(data["askPx"].as_str().unwrap_or("0")),
            qty: parse_f64(data["askSz"].as_str().unwrap_or("0")),
            num_of_orders: 0,
            num_of_pos: 0,
        }],
    })
}

/// Normalize OKX `books5`/`books` channel push → OrderBook.
pub fn normalize_ws_orderbook(
    data: &Value,
    instrument_code: &str,
    exchange: &str,
) -> Option<OrderBook> {
    let ts = data["ts"].as_str().unwrap_or("0");
    let bids = parse_price_levels(data["bids"].as_array()?);
    let asks = parse_price_levels(data["asks"].as_array()?);

    Some(OrderBook {
        instrument_code: instrument_code.to_string(),
        exchange: exchange.to_string(),
        timestamp_ms: parse_i64(ts),
        buy_levels: bids,
        sell_levels: asks,
    })
}

fn parse_price_levels(arr: &[Value]) -> Vec<PriceLevel> {
    arr.iter()
        .filter_map(|level| {
            let a = level.as_array()?;
            // OKX format: [price, qty, liquidated_orders, num_orders]
            Some(PriceLevel {
                price: parse_f64(a.first()?.as_str()?),
                qty: parse_f64(a.get(1)?.as_str()?),
                num_of_orders: a
                    .get(3)
                    .and_then(|v| v.as_str())
                    .map(parse_i64)
                    .unwrap_or(0),
                num_of_pos: 0,
            })
        })
        .collect()
}

/// Normalize OKX `candle{interval}` channel push → Kline.
/// OKX candle data is an array: [ts, o, h, l, c, vol, volCcy, volCcyQuote, confirm].
pub fn normalize_ws_kline(
    data: &Value,
    instrument_code: &str,
    interval: &str,
    exchange: &str,
) -> Option<Kline> {
    let arr = data.as_array()?;
    if arr.len() < 7 {
        return None;
    }

    Some(Kline {
        timestamp: parse_i64(arr[0].as_str()?),
        kline_end_timestamp: 0,
        open: parse_f64(arr[1].as_str()?),
        high: parse_f64(arr[2].as_str()?),
        low: parse_f64(arr[3].as_str()?),
        close: parse_f64(arr[4].as_str()?),
        volume: parse_f64(arr[5].as_str()?),
        amount: parse_f64(arr.get(6).and_then(|v| v.as_str()).unwrap_or("0")),
        kline_type: map_interval_to_kline_type(interval) as i32,
        symbol: instrument_code.to_string(),
        source: exchange.to_string(),
    })
}

/// Normalize OKX `funding-rate` channel push → FundingRate.
pub fn normalize_ws_funding(
    data: &Value,
    instrument_code: &str,
    exchange: &str,
) -> Option<FundingRate> {
    let funding_rate = data["fundingRate"].as_str()?;
    let funding_time = data["fundingTime"].as_str().unwrap_or("0");
    let next_funding_time = data["nextFundingTime"].as_str().unwrap_or("0");

    Some(FundingRate {
        instrument_code: instrument_code.to_string(),
        exchange: exchange.to_string(),
        curr_funding_rate_timestamp: parse_i64(funding_time),
        funding_rate: parse_f64(funding_rate),
        mark_price: parse_f64(data["markPx"].as_str().unwrap_or("0")),
        index_price: parse_f64(data["indexPx"].as_str().unwrap_or("0")),
        next_funding_rate: parse_f64(
            data["nextFundingRate"].as_str().unwrap_or("0"),
        ),
        observe_timestamp: now_ms(),
        next_funding_rate_timestamp: parse_i64(next_funding_time),
        original_message: String::new(),
        next_payment_timestamp: parse_i64(next_funding_time),
    })
}

// ── REST normalizers ────────────────────────────────────────────────────────

/// Normalize `OkxTickerData` (from REST) → `TickData`.
pub fn normalize_rest_ticker(data: &OkxTickerData, instrument_code: &str) -> TickData {
    TickData {
        tick_type: TickUpdateType::M as i32,
        instrument_code: instrument_code.to_string(),
        exchange: "okx".to_string(),
        original_timestamp: parse_i64(&data.ts),
        received_timestamp: now_ms(),
        volume: parse_f64(&data.vol24h),
        latest_trade_price: parse_f64(&data.last),
        latest_trade_qty: parse_f64(&data.last_sz),
        latest_trade_side: String::new(),
        buy_price_levels: vec![PriceLevel {
            price: parse_f64(&data.bid_px),
            qty: parse_f64(&data.bid_sz),
            num_of_orders: 0,
            num_of_pos: 0,
        }],
        sell_price_levels: vec![PriceLevel {
            price: parse_f64(&data.ask_px),
            qty: parse_f64(&data.ask_sz),
            num_of_orders: 0,
            num_of_pos: 0,
        }],
    }
}

/// Normalize `OkxFundingData` (from REST) → `FundingRate`.
pub fn normalize_rest_funding(data: &OkxFundingData, instrument_code: &str) -> FundingRate {
    FundingRate {
        instrument_code: instrument_code.to_string(),
        exchange: "okx".to_string(),
        curr_funding_rate_timestamp: parse_i64(&data.funding_time),
        funding_rate: parse_f64(&data.funding_rate),
        mark_price: 0.0,
        index_price: 0.0,
        next_funding_rate: parse_f64(&data.next_funding_rate),
        observe_timestamp: now_ms(),
        next_funding_rate_timestamp: parse_i64(&data.next_funding_time),
        original_message: String::new(),
        next_payment_timestamp: parse_i64(&data.next_funding_time),
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

pub fn map_interval_to_kline_type(interval: &str) -> KlineType {
    match interval {
        "1m" => KlineType::Kline1min,
        "5m" => KlineType::Kline5min,
        "15m" => KlineType::Kline15min,
        "30m" => KlineType::Kline30min,
        "1H" => KlineType::Kline1hour,
        "4H" => KlineType::Kline4hour,
        "1D" | "1Dutc" => KlineType::Kline1day,
        _ => KlineType::Kline1min,
    }
}

/// Map an interval string to OKX candle channel name.
pub fn okx_candle_channel(interval: &str) -> String {
    format!("candle{interval}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_ws_ticker() {
        let data = serde_json::json!({
            "last": "42000.5", "lastSz": "0.01",
            "bidPx": "41999.0", "bidSz": "1.5",
            "askPx": "42001.0", "askSz": "2.0",
            "vol24h": "50000", "ts": "1700000000000"
        });
        let tick = normalize_ws_ticker(&data, "BTC-USDT-PERP", "okx").unwrap();
        assert_eq!(tick.latest_trade_price, 42000.5);
        assert_eq!(tick.latest_trade_qty, 0.01);
        assert_eq!(tick.volume, 50000.0);
        assert_eq!(tick.buy_price_levels.len(), 1);
        assert_eq!(tick.buy_price_levels[0].price, 41999.0);
        assert_eq!(tick.sell_price_levels[0].price, 42001.0);
        assert_eq!(tick.original_timestamp, 1700000000000);
        assert_eq!(tick.instrument_code, "BTC-USDT-PERP");
    }

    #[test]
    fn test_normalize_ws_orderbook() {
        let data = serde_json::json!({
            "bids": [
                ["41999.0", "1.5", "0", "3"],
                ["41998.0", "2.0", "0", "5"]
            ],
            "asks": [
                ["42001.0", "0.8", "0", "2"]
            ],
            "ts": "1700000000000"
        });
        let ob = normalize_ws_orderbook(&data, "BTC-USDT", "okx").unwrap();
        assert_eq!(ob.buy_levels.len(), 2);
        assert_eq!(ob.sell_levels.len(), 1);
        assert_eq!(ob.buy_levels[0].price, 41999.0);
        assert_eq!(ob.buy_levels[0].qty, 1.5);
        assert_eq!(ob.buy_levels[0].num_of_orders, 3);
        assert_eq!(ob.sell_levels[0].price, 42001.0);
        assert_eq!(ob.timestamp_ms, 1700000000000);
    }

    #[test]
    fn test_normalize_ws_kline() {
        // OKX candle: [ts, o, h, l, c, vol, volCcy, volCcyQuote, confirm]
        let data = serde_json::json!([
            "1700000000000", "42000.0", "42500.0", "41800.0", "42300.0",
            "1000.5", "42150000", "42150000", "1"
        ]);
        let kline = normalize_ws_kline(&data, "BTC-USDT", "1m", "okx").unwrap();
        assert_eq!(kline.timestamp, 1700000000000);
        assert_eq!(kline.open, 42000.0);
        assert_eq!(kline.high, 42500.0);
        assert_eq!(kline.low, 41800.0);
        assert_eq!(kline.close, 42300.0);
        assert_eq!(kline.volume, 1000.5);
        assert_eq!(kline.kline_type, KlineType::Kline1min as i32);
        assert_eq!(kline.symbol, "BTC-USDT");
    }

    #[test]
    fn test_normalize_ws_funding() {
        let data = serde_json::json!({
            "fundingRate": "0.0001",
            "nextFundingRate": "0.00015",
            "fundingTime": "1700000000000",
            "nextFundingTime": "1700028800000",
            "markPx": "42100.0",
            "indexPx": "42050.0"
        });
        let fr = normalize_ws_funding(&data, "BTC-USDT-SWAP", "okx").unwrap();
        assert_eq!(fr.funding_rate, 0.0001);
        assert_eq!(fr.next_funding_rate, 0.00015);
        assert_eq!(fr.mark_price, 42100.0);
        assert_eq!(fr.index_price, 42050.0);
        assert_eq!(fr.curr_funding_rate_timestamp, 1700000000000);
        assert_eq!(fr.next_funding_rate_timestamp, 1700028800000);
    }

    #[test]
    fn test_normalize_rest_ticker() {
        let data = OkxTickerData {
            inst_id: "BTC-USDT".into(),
            last: "42000.5".into(),
            last_sz: "0.01".into(),
            bid_px: "41999.0".into(),
            bid_sz: "1.5".into(),
            ask_px: "42001.0".into(),
            ask_sz: "2.0".into(),
            vol24h: "50000".into(),
            ts: "1700000000000".into(),
        };
        let tick = normalize_rest_ticker(&data, "BTC-USDT");
        assert_eq!(tick.latest_trade_price, 42000.5);
        assert_eq!(tick.instrument_code, "BTC-USDT");
    }

    #[test]
    fn test_normalize_rest_funding() {
        let data = OkxFundingData {
            inst_id: "BTC-USDT-SWAP".into(),
            funding_rate: "0.0002".into(),
            next_funding_rate: "0.0003".into(),
            funding_time: "1700000000000".into(),
            next_funding_time: "1700028800000".into(),
        };
        let fr = normalize_rest_funding(&data, "BTC-USDT-SWAP");
        assert_eq!(fr.funding_rate, 0.0002);
        assert_eq!(fr.next_funding_rate, 0.0003);
    }

    #[test]
    fn test_map_interval_to_kline_type() {
        assert_eq!(map_interval_to_kline_type("1m"), KlineType::Kline1min);
        assert_eq!(map_interval_to_kline_type("5m"), KlineType::Kline5min);
        assert_eq!(map_interval_to_kline_type("15m"), KlineType::Kline15min);
        assert_eq!(map_interval_to_kline_type("1H"), KlineType::Kline1hour);
        assert_eq!(map_interval_to_kline_type("4H"), KlineType::Kline4hour);
        assert_eq!(map_interval_to_kline_type("1D"), KlineType::Kline1day);
    }

    #[test]
    fn test_okx_candle_channel() {
        assert_eq!(okx_candle_channel("1m"), "candle1m");
        assert_eq!(okx_candle_channel("1H"), "candle1H");
    }
}
