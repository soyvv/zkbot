"""Normalize OANDA v20 JSON responses into venue fact dicts.

order_report events carry ``payload_bytes`` containing serialized
``OrderReport`` protobuf (matching the Rust host expectation).
Balance, position, and trade query results remain plain dicts matching
the VenueAdapter fact shapes in ``zk-gw-svc/src/venue_adapter.rs``.
"""

from __future__ import annotations

import time

from zk.exch_gw.v1 import exch_gw_pb2 as gw_pb
from zk.common.v1 import common_pb2 as common_pb  # noqa: F401


# ── order status mapping ─────────────────────────────────────────────────────

_OANDA_STATUS_MAP: dict[str, int] = {
    "PENDING": gw_pb.EXCH_ORDER_STATUS_BOOKED,
    "TRIGGERED": gw_pb.EXCH_ORDER_STATUS_BOOKED,
    "FILLED": gw_pb.EXCH_ORDER_STATUS_FILLED,
    "CANCELLED": gw_pb.EXCH_ORDER_STATUS_CANCELLED,
}


def order_status_enum(oanda_state: str) -> int:
    """Map OANDA order state to ExchangeOrderStatus enum value."""
    return _OANDA_STATUS_MAP.get(oanda_state, gw_pb.EXCH_ORDER_STATUS_EXCH_REJECTED)


# String version for query results (dict-based facts).
_OANDA_STATUS_STR: dict[str, str] = {
    "PENDING": "booked",
    "TRIGGERED": "booked",
    "FILLED": "filled",
    "CANCELLED": "cancelled",
}


def order_status(oanda_state: str) -> str:
    return _OANDA_STATUS_STR.get(oanda_state, "rejected")


# ── client order id extraction ───────────────────────────────────────────────


def extract_client_order_id(txn: dict) -> int:
    """Extract the client order id from clientExtensions.id, or return 0."""
    ext = txn.get("clientExtensions") or {}
    raw = ext.get("id", "0")
    try:
        return int(raw)
    except (ValueError, TypeError):
        return 0


# ── buysell helper ───────────────────────────────────────────────────────────


def _buysell(units: float) -> int:
    """Return BuySellType enum value: 1=Buy, 2=Sell."""
    return common_pb.BS_BUY if units > 0 else common_pb.BS_SELL


# ── protobuf event builders ─────────────────────────────────────────────────


def order_create_event(txn: dict) -> dict:
    """Build an order_report event from an order-create transaction.

    Returns ``{"event_type": "order_report", "payload_bytes": b"..."}``.
    """
    order_id = extract_client_order_id(txn)
    exch_order_ref = str(txn.get("id", ""))
    instrument = txn.get("instrument", "")
    units = float(txn.get("units", 0))
    price = float(txn.get("price", 0))

    report = gw_pb.OrderReport(
        exch_order_ref=exch_order_ref,
        order_id=order_id,
        update_timestamp=_ts_ms(),
        order_report_entries=[
            gw_pb.OrderReportEntry(
                report_type=gw_pb.ORDER_REP_TYPE_LINKAGE,
                order_id_linkage_report=gw_pb.OrderIdLinkageReport(
                    exch_order_ref=exch_order_ref,
                    order_id=order_id,
                ),
            ),
            gw_pb.OrderReportEntry(
                report_type=gw_pb.ORDER_REP_TYPE_STATE,
                order_state_report=gw_pb.OrderStateReport(
                    exch_order_status=gw_pb.EXCH_ORDER_STATUS_BOOKED,
                    filled_qty=0.0,
                    unfilled_qty=abs(units),
                    avg_price=price,
                    order_info=gw_pb.OrderInfo(
                        exch_order_ref=exch_order_ref,
                        instrument=instrument,
                        buy_sell_type=_buysell(units),
                        place_qty=abs(units),
                        place_price=price,
                    ),
                ),
            ),
        ],
    )
    return {"event_type": "order_report", "payload_bytes": report.SerializeToString()}


def order_fill_event(txn: dict) -> dict:
    """Build an order_report event from an ORDER_FILL transaction."""
    order_id = extract_client_order_id(txn)
    exch_order_ref = str(txn.get("orderID", ""))
    instrument = txn.get("instrument", "")
    units = float(txn.get("units", 0))
    price = float(txn.get("price", 0))
    txn_id = str(txn.get("id", ""))
    ts = _ts_ms()

    report = gw_pb.OrderReport(
        exch_order_ref=exch_order_ref,
        order_id=order_id,
        update_timestamp=ts,
        order_report_entries=[
            gw_pb.OrderReportEntry(
                report_type=gw_pb.ORDER_REP_TYPE_STATE,
                order_state_report=gw_pb.OrderStateReport(
                    exch_order_status=gw_pb.EXCH_ORDER_STATUS_FILLED,
                    filled_qty=abs(units),
                    unfilled_qty=0.0,
                    avg_price=price,
                    order_info=gw_pb.OrderInfo(
                        exch_order_ref=exch_order_ref,
                        instrument=instrument,
                        buy_sell_type=_buysell(units),
                    ),
                ),
            ),
            gw_pb.OrderReportEntry(
                report_type=gw_pb.ORDER_REP_TYPE_TRADE,
                trade_report=gw_pb.TradeReport(
                    exch_trade_id=txn_id,
                    filled_qty=abs(units),
                    filled_price=price,
                    filled_ts=ts,
                    order_info=gw_pb.OrderInfo(
                        exch_order_ref=exch_order_ref,
                        instrument=instrument,
                        buy_sell_type=_buysell(units),
                    ),
                ),
            ),
        ],
    )
    return {"event_type": "order_report", "payload_bytes": report.SerializeToString()}


def order_cancel_event(txn: dict) -> dict:
    """Build an order_report event from an ORDER_CANCEL transaction."""
    order_id = extract_client_order_id(txn)
    exch_order_ref = str(txn.get("orderID", ""))

    report = gw_pb.OrderReport(
        exch_order_ref=exch_order_ref,
        order_id=order_id,
        update_timestamp=_ts_ms(),
        order_report_entries=[
            gw_pb.OrderReportEntry(
                report_type=gw_pb.ORDER_REP_TYPE_STATE,
                order_state_report=gw_pb.OrderStateReport(
                    exch_order_status=gw_pb.EXCH_ORDER_STATUS_CANCELLED,
                ),
            ),
        ],
    )
    return {"event_type": "order_report", "payload_bytes": report.SerializeToString()}


def order_reject_event(txn: dict) -> dict:
    """Build an order_report event from a *_REJECT transaction."""
    order_id = extract_client_order_id(txn)
    reason = txn.get("rejectReason", txn.get("reason", "UNKNOWN"))

    report = gw_pb.OrderReport(
        exch_order_ref="",
        order_id=order_id,
        update_timestamp=_ts_ms(),
        order_report_entries=[
            gw_pb.OrderReportEntry(
                report_type=gw_pb.ORDER_REP_TYPE_EXEC,
                exec_report=gw_pb.ExecReport(
                    exec_type=gw_pb.EXCH_EXEC_TYPE_REJECTED,
                    exec_message=reason,
                ),
            ),
        ],
    )
    return {"event_type": "order_report", "payload_bytes": report.SerializeToString()}


def order_state_event(
    exch_order_ref: str,
    order_id: int,
    status: int,
    filled_qty: float,
    unfilled_qty: float,
    avg_price: float,
    instrument: str = "",
    buysell: int = 0,
) -> dict:
    """Build an order_report state event (used by query-after-action)."""
    report = gw_pb.OrderReport(
        exch_order_ref=exch_order_ref,
        order_id=order_id,
        update_timestamp=_ts_ms(),
        order_report_entries=[
            gw_pb.OrderReportEntry(
                report_type=gw_pb.ORDER_REP_TYPE_STATE,
                order_state_report=gw_pb.OrderStateReport(
                    exch_order_status=status,
                    filled_qty=filled_qty,
                    unfilled_qty=unfilled_qty,
                    avg_price=avg_price,
                    order_info=gw_pb.OrderInfo(
                        exch_order_ref=exch_order_ref,
                        instrument=instrument,
                        buy_sell_type=buysell,
                    ),
                ),
            ),
        ],
    )
    return {"event_type": "order_report", "payload_bytes": report.SerializeToString()}


# ── query normalization (dict-based, no protobuf) ───────────────────────────


def normalize_balance(summary: dict) -> list[dict]:
    """Normalize account summary → list of VenueBalanceFact dicts."""
    acct = summary.get("account", {})
    return [
        {
            "asset": acct.get("currency", "USD"),
            "total_qty": float(acct.get("balance", 0)),
            "avail_qty": float(acct.get("marginAvailable", 0)),
            "frozen_qty": float(acct.get("marginUsed", 0)),
        }
    ]


def normalize_order(order: dict) -> dict:
    """Normalize a single OANDA order → VenueOrderFact dict."""
    units = float(order.get("units", 0))
    filled = float(order.get("filledUnits", 0) or 0)
    state = order.get("state", "PENDING")

    return {
        "order_id": extract_client_order_id(order),
        "exch_order_ref": str(order.get("id", "")),
        "instrument": order.get("instrument", ""),
        "status": order_status(state),
        "status_enum": order_status_enum(state),
        "filled_qty": abs(filled),
        "unfilled_qty": abs(units) - abs(filled),
        "avg_price": float(order.get("price", 0)),
        "timestamp": _ts_ms(),
    }


def normalize_orders(orders_resp: dict) -> list[dict]:
    """Normalize OANDA orders response → list of VenueOrderFact dicts."""
    orders = orders_resp.get("orders", [])
    return [normalize_order(o) for o in orders]


def normalize_trade(trade: dict) -> dict:
    """Normalize a single OANDA trade → VenueTradeFact dict."""
    units = float(trade.get("currentUnits", trade.get("initialUnits", 0)))
    return {
        "exch_trade_id": str(trade.get("id", "")),
        "order_id": 0,
        "exch_order_ref": "",
        "instrument": trade.get("instrument", ""),
        "buysell_type": 1 if units > 0 else 2,
        "filled_qty": abs(units),
        "filled_price": float(trade.get("price", 0)),
        "timestamp": _ts_ms(),
    }


def normalize_trades(trades_resp: dict) -> list[dict]:
    """Normalize OANDA trades response → list of VenueTradeFact dicts."""
    trades = trades_resp.get("trades", [])
    return [normalize_trade(t) for t in trades]


_INST_TYPE_CFD = 4  # proto InstrumentType.INST_TYPE_CFD


def normalize_positions(positions_resp: dict, account_id: int) -> list[dict]:
    """Normalize OANDA positions → list of VenuePositionFact dicts.

    OANDA positions have long/short sub-positions on the same instrument.
    Emit one fact per side with non-zero units.
    All OANDA positions are CFDs (instrument_type=4).
    """
    results: list[dict] = []
    for pos in positions_resp.get("positions", []):
        instrument = pos.get("instrument", "")
        long_units = float(pos.get("long", {}).get("units", 0))
        short_units = float(pos.get("short", {}).get("units", 0))

        if long_units != 0:
            results.append({
                "instrument": instrument,
                "long_short_type": 1,  # LsLong
                "qty": abs(long_units),
                "avail_qty": abs(long_units),
                "frozen_qty": 0.0,
                "account_id": account_id,
                "instrument_type": _INST_TYPE_CFD,
            })
        if short_units != 0:
            results.append({
                "instrument": instrument,
                "long_short_type": 2,  # LsShort
                "qty": abs(short_units),
                "avail_qty": abs(short_units),
                "frozen_qty": 0.0,
                "account_id": account_id,
                "instrument_type": _INST_TYPE_CFD,
            })
    return results


# ── helpers ──────────────────────────────────────────────────────────────────


def _ts_ms() -> int:
    return int(time.time() * 1000)


# ── RTMD normalization ───────────────────────────────────────────────────────

from zk.rtmd.v1 import rtmd_pb2 as rtmd_pb  # noqa: E402


def parse_oanda_time(time_str: str) -> int:
    """Parse OANDA RFC3339 timestamp to epoch milliseconds.

    OANDA uses nanosecond-precision timestamps like:
    ``2024-01-15T12:30:00.123456789Z``
    """
    from datetime import datetime

    # Strip trailing nanoseconds beyond microseconds
    if "." in time_str:
        base, frac = time_str.rstrip("Z").split(".")
        frac = frac[:6].ljust(6, "0")
        time_str = f"{base}.{frac}Z"
    dt = datetime.fromisoformat(time_str.replace("Z", "+00:00"))
    return int(dt.timestamp() * 1000)


_GRANULARITY_MAP: dict[str, str] = {
    "1m": "M1", "5m": "M5", "15m": "M15", "30m": "M30",
    "1h": "H1", "4h": "H4", "1d": "D",
}

_KLINE_TYPE_MAP: dict[str, int] = {
    "1m": rtmd_pb.Kline.KLINE_1MIN,
    "5m": rtmd_pb.Kline.KLINE_5MIN,
    "15m": rtmd_pb.Kline.KLINE_15MIN,
    "30m": rtmd_pb.Kline.KLINE_30MIN,
    "1h": rtmd_pb.Kline.KLINE_1HOUR,
    "4h": rtmd_pb.Kline.KLINE_4HOUR,
    "1d": rtmd_pb.Kline.KLINE_1DAY,
}

_INTERVAL_FROM_GRANULARITY: dict[str, str] = {v: k for k, v in _GRANULARITY_MAP.items()}


def oanda_granularity(interval: str) -> str:
    """Map standard interval string to OANDA granularity code."""
    return _GRANULARITY_MAP.get(interval, interval)


def kline_type_from_interval(interval: str) -> int:
    """Map interval string to KlineType proto enum value."""
    return _KLINE_TYPE_MAP.get(interval, rtmd_pb.Kline.KLINE_1MIN)


def build_tick_data(price_msg: dict, venue: str = "oanda") -> bytes:
    """Convert OANDA PRICE stream message to serialized TickData proto bytes."""
    instrument = price_msg.get("instrument", "")
    bids = price_msg.get("bids", [])
    asks = price_msg.get("asks", [])
    orig_ts = parse_oanda_time(price_msg["time"]) if "time" in price_msg else _ts_ms()

    buy_levels = [
        rtmd_pb.PriceLevel(price=float(b["price"]), qty=float(b.get("liquidity", 0)))
        for b in bids
    ]
    sell_levels = [
        rtmd_pb.PriceLevel(price=float(a["price"]), qty=float(a.get("liquidity", 0)))
        for a in asks
    ]

    tick = rtmd_pb.TickData(
        tick_type=rtmd_pb.OB,
        instrument_code=instrument,
        exchange=venue,
        original_timestamp=orig_ts,
        received_timestamp=_ts_ms(),
        buy_price_levels=buy_levels,
        sell_price_levels=sell_levels,
    )
    return tick.SerializeToString()


def build_kline(
    candle: dict,
    instrument: str,
    granularity: str,
    venue: str = "oanda",
) -> bytes:
    """Convert OANDA candle dict to serialized Kline proto bytes."""
    mid = candle.get("mid", {})
    ts = parse_oanda_time(candle["time"]) if "time" in candle else _ts_ms()
    interval = _INTERVAL_FROM_GRANULARITY.get(granularity, "1m")

    kline = rtmd_pb.Kline(
        timestamp=ts,
        open=float(mid.get("o", 0)),
        high=float(mid.get("h", 0)),
        close=float(mid.get("c", 0)),
        low=float(mid.get("l", 0)),
        volume=float(candle.get("volume", 0)),
        kline_type=kline_type_from_interval(interval),
        symbol=instrument,
        source=venue,
    )
    return kline.SerializeToString()
