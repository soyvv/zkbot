use dashmap::DashMap;

use crate::rest::{OkxBalanceDetail, OkxOrderData, OkxPositionData, OkxTradeData};

// Re-export proto types used by the adapter.
pub use zk_proto_rs::zk::exch_gw::v1::{
    order_report_entry::Report, ExchExecType, ExchangeOrderStatus, FeeReport, OrderIdLinkageReport,
    OrderInfo, OrderReport, OrderReportEntry, OrderReportType, OrderSourceType, OrderStateReport,
    TradeReport,
};

// ── Shared ID map ───────────────────────────────────────────────────────────

/// Bidirectional mapping between OKX clOrdId / ordId / instId.
pub struct OkxIdMap {
    /// clOrdId → ordId
    pub clord_to_ord: DashMap<String, String>,
    /// ordId → clOrdId
    pub ord_to_clord: DashMap<String, String>,
    /// ordId → instId (needed for cancel — OKX requires instId)
    pub ord_to_inst: DashMap<String, String>,
}

impl OkxIdMap {
    pub fn new() -> Self {
        Self {
            clord_to_ord: DashMap::new(),
            ord_to_clord: DashMap::new(),
            ord_to_inst: DashMap::new(),
        }
    }

    /// Record a linkage. Safe to call repeatedly for the same pair.
    pub fn record(&self, cl_ord_id: &str, ord_id: &str, inst_id: &str) {
        if !cl_ord_id.is_empty() && !ord_id.is_empty() {
            self.clord_to_ord
                .insert(cl_ord_id.to_string(), ord_id.to_string());
            self.ord_to_clord
                .insert(ord_id.to_string(), cl_ord_id.to_string());
        }
        if !ord_id.is_empty() && !inst_id.is_empty() {
            self.ord_to_inst
                .insert(ord_id.to_string(), inst_id.to_string());
        }
    }
}

// ── OKX state → proto status ────────────────────────────────────────────────

pub fn map_order_status(state: &str) -> ExchangeOrderStatus {
    match state {
        "live" => ExchangeOrderStatus::ExchOrderStatusBooked,
        "partially_filled" => ExchangeOrderStatus::ExchOrderStatusPartialFilled,
        "filled" => ExchangeOrderStatus::ExchOrderStatusFilled,
        "canceled" => ExchangeOrderStatus::ExchOrderStatusCancelled,
        _ => ExchangeOrderStatus::ExchOrderStatusUnspecified,
    }
}

pub fn map_side(side: &str) -> i32 {
    match side {
        "buy" => zk_proto_rs::zk::common::v1::BuySellType::BsBuy as i32,
        "sell" => zk_proto_rs::zk::common::v1::BuySellType::BsSell as i32,
        _ => 0,
    }
}

fn parse_f64(s: &str) -> f64 {
    s.parse::<f64>().unwrap_or(0.0)
}

fn parse_i64(s: &str) -> i64 {
    s.parse::<i64>().unwrap_or(0)
}

// ── WebSocket order event normalization ─────────────────────────────────────

/// Normalize a single OKX WS "orders" channel push into a proto `OrderReport`.
///
/// `data` is one element from the WS push `data` array.
pub fn normalize_ws_order(
    data: &serde_json::Value,
    id_map: &OkxIdMap,
    gw_id: &str,
    account_id: i64,
) -> Option<OrderReport> {
    let ord_id = data["ordId"].as_str().unwrap_or_default();
    let cl_ord_id = data["clOrdId"].as_str().unwrap_or_default();
    let inst_id = data["instId"].as_str().unwrap_or_default();
    let side = data["side"].as_str().unwrap_or_default();
    let state = data["state"].as_str().unwrap_or_default();
    let sz = data["sz"].as_str().unwrap_or_default();
    let fill_sz = data["fillSz"].as_str().unwrap_or_default();
    let avg_px = data["avgPx"].as_str().unwrap_or_default();
    let u_time = data["uTime"].as_str().unwrap_or_default();
    let trade_id = data["tradeId"].as_str().unwrap_or_default();
    let fill_px = data["fillPx"].as_str().unwrap_or_default();
    let last_fill_sz = data["fillSz"].as_str().unwrap_or_default();
    let fee = data["fee"].as_str().unwrap_or_default();
    let fee_ccy = data["feeCcy"].as_str().unwrap_or_default();
    let px = data["px"].as_str().unwrap_or_default();

    if ord_id.is_empty() {
        return None;
    }

    // Record linkage
    id_map.record(cl_ord_id, ord_id, inst_id);

    let order_id = parse_i64(cl_ord_id);
    let timestamp = parse_i64(u_time);
    let total_sz = parse_f64(sz);
    let filled_qty = parse_f64(fill_sz);
    let unfilled_qty = total_sz - filled_qty;

    let mut entries = Vec::new();

    // 1. Linkage entry
    entries.push(OrderReportEntry {
        report_type: OrderReportType::OrderRepTypeLinkage as i32,
        report: Some(Report::OrderIdLinkageReport(OrderIdLinkageReport {
            exch_order_ref: ord_id.to_string(),
            order_id,
        })),
    });

    // 2. State entry
    entries.push(OrderReportEntry {
        report_type: OrderReportType::OrderRepTypeState as i32,
        report: Some(Report::OrderStateReport(OrderStateReport {
            exch_order_status: map_order_status(state) as i32,
            filled_qty,
            unfilled_qty,
            avg_price: parse_f64(avg_px),
            order_info: Some(OrderInfo {
                exch_order_ref: ord_id.to_string(),
                client_order_id: cl_ord_id.to_string(),
                instrument: inst_id.to_string(),
                exch_account_id: String::new(),
                buy_sell_type: map_side(side),
                place_qty: total_sz,
                place_price: parse_f64(px),
            }),
        })),
    });

    // 3. Trade entry (if this push contains a fill)
    if !trade_id.is_empty() && parse_f64(last_fill_sz) > 0.0 {
        entries.push(OrderReportEntry {
            report_type: OrderReportType::OrderRepTypeTrade as i32,
            report: Some(Report::TradeReport(TradeReport {
                exch_trade_id: trade_id.to_string(),
                filled_qty: parse_f64(last_fill_sz),
                filled_price: parse_f64(fill_px),
                fill_type: 0,
                filled_ts: timestamp,
                exch_pnl: 0.0,
                order_info: Some(OrderInfo {
                    exch_order_ref: ord_id.to_string(),
                    client_order_id: cl_ord_id.to_string(),
                    instrument: inst_id.to_string(),
                    exch_account_id: String::new(),
                    buy_sell_type: map_side(side),
                    place_qty: total_sz,
                    place_price: parse_f64(px),
                }),
            })),
        });
    }

    // 4. Fee entry
    let fee_val = parse_f64(fee);
    if fee_val != 0.0 && !fee_ccy.is_empty() {
        entries.push(OrderReportEntry {
            report_type: OrderReportType::OrderRepTypeFee as i32,
            report: Some(Report::FeeReport(FeeReport {
                fee_unit_type: 0,
                fee_qty: fee_val.abs(),
                fee_symbol: fee_ccy.to_string(),
                fee_type: 0,
                fee_ts: timestamp,
                inst_symbol: inst_id.to_string(),
            })),
        });
    }

    Some(OrderReport {
        exch_order_ref: ord_id.to_string(),
        order_id,
        exchange: gw_id.to_string(),
        order_report_entries: entries,
        account_id,
        update_timestamp: timestamp,
        extra_order_id: String::new(),
        order_source_type: OrderSourceType::OrderSourceTq as i32,
        gw_received_at_ns: 0,
    })
}

// ── REST response normalization (for VenueFact types) ───────────────────────

/// Represents a venue-level balance fact.
#[derive(Debug, Clone)]
pub struct BalanceFact {
    pub asset: String,
    pub total_qty: f64,
    pub avail_qty: f64,
    pub frozen_qty: f64,
}

#[derive(Debug, Clone)]
pub struct OrderFact {
    pub order_id: i64,
    pub exch_order_ref: String,
    pub instrument: String,
    pub status: ExchangeOrderStatus,
    pub filled_qty: f64,
    pub unfilled_qty: f64,
    pub avg_price: f64,
    pub timestamp: i64,
}

#[derive(Debug, Clone)]
pub struct TradeFact {
    pub exch_trade_id: String,
    pub order_id: i64,
    pub exch_order_ref: String,
    pub instrument: String,
    pub buysell_type: i32,
    pub filled_qty: f64,
    pub filled_price: f64,
    pub timestamp: i64,
}

#[derive(Debug, Clone)]
pub struct PositionFact {
    pub instrument: String,
    pub long_short_type: i32,
    pub qty: f64,
    pub avail_qty: f64,
}

pub fn normalize_rest_balance(data: &OkxBalanceDetail) -> BalanceFact {
    let total = parse_f64(&data.cash_bal);
    let avail = parse_f64(&data.avail_bal);
    let frozen = parse_f64(&data.frozen_bal);
    BalanceFact {
        asset: data.ccy.clone(),
        total_qty: total,
        avail_qty: avail,
        frozen_qty: frozen,
    }
}

pub fn normalize_rest_order(data: &OkxOrderData, id_map: &OkxIdMap) -> OrderFact {
    id_map.record(&data.cl_ord_id, &data.ord_id, &data.inst_id);

    let total_sz = parse_f64(&data.sz);
    let filled = parse_f64(&data.fill_sz);
    OrderFact {
        order_id: parse_i64(&data.cl_ord_id),
        exch_order_ref: data.ord_id.clone(),
        instrument: data.inst_id.clone(),
        status: map_order_status(&data.state),
        filled_qty: filled,
        unfilled_qty: total_sz - filled,
        avg_price: parse_f64(&data.avg_px),
        timestamp: parse_i64(&data.u_time),
    }
}

pub fn normalize_rest_trade(data: &OkxTradeData, id_map: &OkxIdMap) -> TradeFact {
    id_map.record(&data.cl_ord_id, &data.ord_id, &data.inst_id);

    TradeFact {
        exch_trade_id: data.trade_id.clone(),
        order_id: parse_i64(&data.cl_ord_id),
        exch_order_ref: data.ord_id.clone(),
        instrument: data.inst_id.clone(),
        buysell_type: map_side(&data.side),
        filled_qty: parse_f64(&data.fill_sz),
        filled_price: parse_f64(&data.fill_px),
        timestamp: parse_i64(&data.ts),
    }
}

pub fn normalize_rest_position(data: &OkxPositionData) -> PositionFact {
    let ls = match data.pos_side.as_str() {
        "long" => zk_proto_rs::zk::common::v1::LongShortType::LsLong as i32,
        "short" => zk_proto_rs::zk::common::v1::LongShortType::LsShort as i32,
        _ => 0,
    };
    PositionFact {
        instrument: data.inst_id.clone(),
        long_short_type: ls,
        qty: parse_f64(&data.pos),
        avail_qty: parse_f64(&data.avail_pos),
    }
}

/// Normalize OKX WS "account" channel push into balance facts.
pub fn normalize_ws_account(data: &serde_json::Value) -> Vec<BalanceFact> {
    let details = match data["details"].as_array() {
        Some(arr) => arr,
        None => return vec![],
    };

    details
        .iter()
        .filter_map(|d| {
            let ccy = d["ccy"].as_str()?;
            Some(BalanceFact {
                asset: ccy.to_string(),
                total_qty: parse_f64(d["cashBal"].as_str().unwrap_or("0")),
                avail_qty: parse_f64(d["availBal"].as_str().unwrap_or("0")),
                frozen_qty: parse_f64(d["frozenBal"].as_str().unwrap_or("0")),
            })
        })
        .collect()
}

/// Normalize OKX WS "positions" channel push into position facts.
pub fn normalize_ws_positions(data: &serde_json::Value) -> Vec<PositionFact> {
    // OKX positions channel pushes a single object per position update.
    // The data item has instId, posSide, pos, availPos fields.
    let inst_id = match data["instId"].as_str() {
        Some(s) if !s.is_empty() => s,
        _ => return vec![],
    };

    let pos_side = data["posSide"].as_str().unwrap_or("net");
    let ls = match pos_side {
        "long" => zk_proto_rs::zk::common::v1::LongShortType::LsLong as i32,
        "short" => zk_proto_rs::zk::common::v1::LongShortType::LsShort as i32,
        _ => 0,
    };

    vec![PositionFact {
        instrument: inst_id.to_string(),
        long_short_type: ls,
        qty: parse_f64(data["pos"].as_str().unwrap_or("0")),
        avail_qty: parse_f64(data["availPos"].as_str().unwrap_or("0")),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map_order_status() {
        assert_eq!(map_order_status("live"), ExchangeOrderStatus::ExchOrderStatusBooked);
        assert_eq!(map_order_status("filled"), ExchangeOrderStatus::ExchOrderStatusFilled);
        assert_eq!(map_order_status("canceled"), ExchangeOrderStatus::ExchOrderStatusCancelled);
        assert_eq!(map_order_status("unknown"), ExchangeOrderStatus::ExchOrderStatusUnspecified);
    }

    #[test]
    fn test_normalize_ws_order_basic() {
        let id_map = OkxIdMap::new();
        let data = serde_json::json!({
            "ordId": "123456",
            "clOrdId": "99",
            "instId": "BTC-USDT",
            "side": "buy",
            "state": "live",
            "sz": "0.01",
            "fillSz": "0",
            "avgPx": "0",
            "uTime": "1700000000000",
            "tradeId": "",
            "fillPx": "",
            "fee": "0",
            "feeCcy": "",
            "px": "40000"
        });

        let report = normalize_ws_order(&data, &id_map, "gw_okx_1", 9001).unwrap();
        assert_eq!(report.exch_order_ref, "123456");
        assert_eq!(report.order_id, 99);
        assert_eq!(report.order_report_entries.len(), 2); // linkage + state, no trade
        assert_eq!(id_map.clord_to_ord.get("99").unwrap().as_str(), "123456");
    }

    #[test]
    fn test_normalize_ws_order_with_fill() {
        let id_map = OkxIdMap::new();
        let data = serde_json::json!({
            "ordId": "123456",
            "clOrdId": "99",
            "instId": "BTC-USDT",
            "side": "buy",
            "state": "filled",
            "sz": "0.01",
            "fillSz": "0.01",
            "avgPx": "42000",
            "uTime": "1700000000000",
            "tradeId": "T001",
            "fillPx": "42000",
            "fee": "-0.5",
            "feeCcy": "USDT",
            "px": "42000"
        });

        let report = normalize_ws_order(&data, &id_map, "gw_okx_1", 9001).unwrap();
        // linkage + state + trade + fee = 4
        assert_eq!(report.order_report_entries.len(), 4);
    }

    #[test]
    fn test_normalize_ws_positions() {
        let data = serde_json::json!({
            "instId": "BTC-USDT-SWAP",
            "posSide": "long",
            "pos": "1.5",
            "availPos": "1.0"
        });

        let facts = normalize_ws_positions(&data);
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].instrument, "BTC-USDT-SWAP");
        assert_eq!(facts[0].qty, 1.5);
        assert_eq!(facts[0].avail_qty, 1.0);
    }
}
