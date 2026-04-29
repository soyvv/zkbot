"""Route ib_async callbacks into an async queue of venue-fact event dicts.

``order_report`` events carry ``payload_bytes`` containing a serialized
``gw_pb.OrderReport`` protobuf (matching the Rust host expectation and the
OANDA adaptor). Balance and position events remain plain dict payloads
consistent with the VenueAdapter fact shapes in ``zk-gw-svc/src/venue_adapter.rs``.
"""

from __future__ import annotations

import asyncio
import logging
from typing import Any

from zk.common.v1 import common_pb2 as common_pb
from zk.exch_gw.v1 import exch_gw_pb2 as gw_pb

from ibkr.id_map import OrderIdMap
from ibkr.types import CONNECTIVITY_CODES, now_ms

log = logging.getLogger(__name__)


# Map from IBKR secType → proto InstrumentType int. Used by `on_position` to
# attach the type to position events so the gateway pipeline does not have to
# guess via `infer_instrument_type` (which mis-classifies four-part STK specs).
# Source: zk.common.v1.InstrumentType (UNSPECIFIED=0, SPOT=1, PERP=2, FUTURE=3,
# CFD=4, OPTION=5, ETF=6, STOCK=7). The adapter cannot distinguish ETF from
# STOCK from secType alone — emit STOCK; refdata join refines downstream.
_SECTYPE_TO_PROTO_INST_TYPE: dict[str, int] = {
    "STK":  common_pb.InstrumentType.INST_TYPE_STOCK,
    "FUT":  common_pb.InstrumentType.INST_TYPE_FUTURE,
    "OPT":  common_pb.InstrumentType.INST_TYPE_OPTION,
    "CFD":  common_pb.InstrumentType.INST_TYPE_CFD,
    "CASH": common_pb.InstrumentType.INST_TYPE_SPOT,
}


# ── status mapping ───────────────────────────────────────────────────────────

_IBKR_STATUS_ENUM: dict[str, int] = {
    "PreSubmitted": gw_pb.EXCH_ORDER_STATUS_BOOKED,
    "Submitted": gw_pb.EXCH_ORDER_STATUS_BOOKED,
    "ApiPending": gw_pb.EXCH_ORDER_STATUS_BOOKED,
    "PendingSubmit": gw_pb.EXCH_ORDER_STATUS_BOOKED,
    "PendingCancel": gw_pb.EXCH_ORDER_STATUS_BOOKED,
    "Filled": gw_pb.EXCH_ORDER_STATUS_FILLED,
    "Cancelled": gw_pb.EXCH_ORDER_STATUS_CANCELLED,
    "ApiCancelled": gw_pb.EXCH_ORDER_STATUS_CANCELLED,
    "Inactive": gw_pb.EXCH_ORDER_STATUS_EXCH_REJECTED,
}


def _status_enum(ibkr_status: str) -> int:
    return _IBKR_STATUS_ENUM.get(ibkr_status, gw_pb.EXCH_ORDER_STATUS_BOOKED)


def _action_to_buysell(action: str) -> int:
    """Map IB action ('BUY' / 'SELL') to BuySellType enum."""
    if action == "BUY":
        return common_pb.BS_BUY
    if action == "SELL":
        return common_pb.BS_SELL
    return 0


def _side_to_buysell(side: str) -> int:
    """Map IB execution side ('BOT' / 'SLD') to BuySellType enum."""
    if side == "BOT":
        return common_pb.BS_BUY
    if side == "SLD":
        return common_pb.BS_SELL
    return 0


class EventRouter:
    """Bridge between ib_async event callbacks and the ``next_event()`` contract.

    Callbacks from ib_async are synchronous. This router normalizes them into
    bridge-shaped event dicts and enqueues them for async consumption.
    """

    def __init__(
        self, id_map: OrderIdMap, queue_size: int = 4096, account_code: str = ""
    ) -> None:
        self._queue: asyncio.Queue[dict] = asyncio.Queue(maxsize=queue_size)
        self._id_map = id_map
        self._account_code = account_code

    async def next_event(self) -> dict:
        """Block until the next event is available."""
        return await self._queue.get()

    # ------------------------------------------------------------------
    # ib_async callback handlers
    # ------------------------------------------------------------------

    def on_open_order(self, trade: Any) -> None:
        """Handle openOrderEvent — first source of permId binding."""
        order = trade.order
        contract = trade.contract
        order_status = trade.orderStatus

        if order.permId:
            self._id_map.bind_perm_id(order.orderId, order.permId)

        entry = self._id_map.lookup_by_ib_order_id(order.orderId)
        if entry is None:
            log.debug("on_open_order: unknown orderId=%d (external order?)", order.orderId)
            return

        exch_ref = str(entry.perm_id) if entry.perm_id else str(order.orderId)
        instrument = _instrument_from_contract(contract)
        buysell = _action_to_buysell(getattr(order, "action", ""))

        report = gw_pb.OrderReport(
            exch_order_ref=exch_ref,
            order_id=entry.client_order_id,
            update_timestamp=now_ms(),
            order_report_entries=[
                gw_pb.OrderReportEntry(
                    report_type=gw_pb.ORDER_REP_TYPE_LINKAGE,
                    order_id_linkage_report=gw_pb.OrderIdLinkageReport(
                        exch_order_ref=exch_ref,
                        order_id=entry.client_order_id,
                    ),
                ),
                gw_pb.OrderReportEntry(
                    report_type=gw_pb.ORDER_REP_TYPE_STATE,
                    order_state_report=gw_pb.OrderStateReport(
                        exch_order_status=_status_enum(order_status.status),
                        filled_qty=float(order_status.filled),
                        unfilled_qty=float(order_status.remaining),
                        avg_price=float(order_status.avgFillPrice),
                        order_info=gw_pb.OrderInfo(
                            exch_order_ref=exch_ref,
                            instrument=instrument,
                            buy_sell_type=buysell,
                            place_qty=float(getattr(order, "totalQuantity", 0)),
                            place_price=float(getattr(order, "lmtPrice", 0)),
                        ),
                    ),
                ),
            ],
        )
        self._enqueue({"event_type": "order_report", "payload_bytes": report.SerializeToString()})

    def on_order_status(self, trade: Any) -> None:
        """Handle orderStatusEvent — status changes for known orders."""
        order = trade.order
        order_status = trade.orderStatus

        entry = self._id_map.lookup_by_ib_order_id(order.orderId)
        if entry is None:
            return

        if order.permId and entry.perm_id is None:
            self._id_map.bind_perm_id(order.orderId, order.permId)
            entry = self._id_map.lookup_by_ib_order_id(order.orderId)

        exch_ref = str(entry.perm_id) if entry.perm_id else str(order.orderId)

        report = gw_pb.OrderReport(
            exch_order_ref=exch_ref,
            order_id=entry.client_order_id,
            update_timestamp=now_ms(),
            order_report_entries=[
                gw_pb.OrderReportEntry(
                    report_type=gw_pb.ORDER_REP_TYPE_STATE,
                    order_state_report=gw_pb.OrderStateReport(
                        exch_order_status=_status_enum(order_status.status),
                        filled_qty=float(order_status.filled),
                        unfilled_qty=float(order_status.remaining),
                        avg_price=float(order_status.avgFillPrice),
                        order_info=gw_pb.OrderInfo(
                            exch_order_ref=exch_ref,
                            instrument=entry.instrument,
                            buy_sell_type=_action_to_buysell(getattr(order, "action", "")),
                        ),
                    ),
                ),
            ],
        )
        self._enqueue({"event_type": "order_report", "payload_bytes": report.SerializeToString()})

    def on_exec_details(self, trade: Any, fill: Any) -> None:
        """Handle execDetailsEvent — fill/trade information."""
        execution = fill.execution
        contract = fill.contract

        entry = self._id_map.lookup_by_ib_order_id(execution.orderId)
        if entry is None:
            log.debug(
                "on_exec_details: unknown orderId=%d execId=%s",
                execution.orderId,
                execution.execId,
            )
            return

        exch_ref = str(entry.perm_id) if entry.perm_id else str(execution.orderId)
        instrument = _instrument_from_contract(contract)
        buysell = _side_to_buysell(execution.side)
        ts = now_ms()
        shares = float(execution.shares)
        price = float(execution.price)

        report = gw_pb.OrderReport(
            exch_order_ref=exch_ref,
            order_id=entry.client_order_id,
            update_timestamp=ts,
            order_report_entries=[
                gw_pb.OrderReportEntry(
                    report_type=gw_pb.ORDER_REP_TYPE_TRADE,
                    trade_report=gw_pb.TradeReport(
                        exch_trade_id=execution.execId,
                        filled_qty=shares,
                        filled_price=price,
                        filled_ts=ts,
                        order_info=gw_pb.OrderInfo(
                            exch_order_ref=exch_ref,
                            instrument=instrument,
                            buy_sell_type=buysell,
                        ),
                    ),
                ),
            ],
        )
        self._enqueue({"event_type": "order_report", "payload_bytes": report.SerializeToString()})

    def on_position(self, position: Any) -> None:
        """Handle positionEvent — position snapshot."""
        if self._account_code and getattr(position, "account", "") != self._account_code:
            return
        contract = position.contract
        qty = float(position.position)
        long_short = 1 if qty >= 0 else 2  # 1=long, 2=short
        # Set instrument_type from secType so the GW pipeline does not have to
        # guess via string heuristics. STK is reported as STOCK; refdata join
        # downstream refines STOCK→ETF where applicable.
        instrument_type = _SECTYPE_TO_PROTO_INST_TYPE.get(
            getattr(contract, "secType", ""),
            common_pb.InstrumentType.INST_TYPE_UNSPECIFIED,
        )

        event = {
            "event_type": "position",
            "payload": [
                {
                    "instrument": _instrument_from_contract(contract),
                    "instrument_type": int(instrument_type),
                    "long_short_type": long_short,
                    "qty": abs(qty),
                    "avail_qty": abs(qty),
                    "frozen_qty": 0.0,
                    "account_id": 0,
                }
            ],
        }
        self._enqueue(event)

    def on_account_value(self, value: Any) -> None:
        """Handle accountValueEvent — balance snapshot.

        Filters to relevant balance tags and batches into a balance event.
        """
        if value.tag not in ("CashBalance", "TotalCashBalance"):
            return
        if self._account_code and getattr(value, "account", "") != self._account_code:
            return

        event = {
            "event_type": "balance",
            "payload": [
                {
                    "asset": value.currency,
                    "total_qty": float(value.value),
                    "avail_qty": float(value.value),
                    "frozen_qty": 0.0,
                }
            ],
        }
        self._enqueue(event)

    def on_error(self, reqId: int, errorCode: int, errorString: str, contract: Any) -> None:
        """Route errors: order-specific → rejection, connectivity → system event."""
        if errorCode in CONNECTIVITY_CODES:
            event = {
                "event_type": "system",
                "payload": {
                    "event_type": _system_event_type(errorCode),
                    "message": f"[{errorCode}] {errorString}",
                    "timestamp": now_ms(),
                },
            }
            self._enqueue(event)
            return

        if reqId > 0:
            entry = self._id_map.lookup_by_ib_order_id(reqId)
            if entry is not None:
                exch_ref = str(entry.perm_id or reqId)
                report = gw_pb.OrderReport(
                    exch_order_ref=exch_ref,
                    order_id=entry.client_order_id,
                    update_timestamp=now_ms(),
                    order_report_entries=[
                        gw_pb.OrderReportEntry(
                            report_type=gw_pb.ORDER_REP_TYPE_STATE,
                            order_state_report=gw_pb.OrderStateReport(
                                exch_order_status=gw_pb.EXCH_ORDER_STATUS_EXCH_REJECTED,
                                order_info=gw_pb.OrderInfo(
                                    exch_order_ref=exch_ref,
                                    instrument=entry.instrument,
                                ),
                            ),
                        ),
                        gw_pb.OrderReportEntry(
                            report_type=gw_pb.ORDER_REP_TYPE_EXEC,
                            exec_report=gw_pb.ExecReport(
                                exec_type=gw_pb.EXCH_EXEC_TYPE_REJECTED,
                                exec_message=f"[{errorCode}] {errorString}",
                            ),
                        ),
                    ],
                )
                self._enqueue(
                    {"event_type": "order_report", "payload_bytes": report.SerializeToString()}
                )
                return

        log.warning("IB error reqId=%d code=%d: %s", reqId, errorCode, errorString)

    # ------------------------------------------------------------------
    # Internal
    # ------------------------------------------------------------------

    def _enqueue(self, event: dict) -> None:
        """Non-blocking enqueue. Drop oldest if full."""
        try:
            self._queue.put_nowait(event)
        except asyncio.QueueFull:
            try:
                self._queue.get_nowait()
            except asyncio.QueueEmpty:
                pass
            try:
                self._queue.put_nowait(event)
            except asyncio.QueueFull:
                log.error("event queue still full after drop, losing event")


def _instrument_from_contract(contract: Any) -> str:
    """Best-effort instrument string from an IB contract object.

    For STK secType the routing destination is forced to ``SMART`` to match
    refdata's `instrument_exch` shape (e.g. ``SPY-USD-STK-SMART``). IBKR
    rewrites ``contract.exchange`` to the primary listing exchange (ARCA,
    NASDAQ, …) on positionEvent / executionDetails, so reusing it here would
    diverge from refdata and break OMS instrument lookup. Other secTypes
    (FUT, OPT, CFD, CASH) keep ``contract.exchange`` since their routing is
    venue-specific (e.g. ``ES-USD-FUT-CME``).
    """
    symbol = getattr(contract, "symbol", "") or ""
    currency = getattr(contract, "currency", "") or ""
    sec_type = getattr(contract, "secType", "") or ""
    exchange = getattr(contract, "exchange", "") or ""
    if sec_type == "STK":
        exchange = "SMART"
    return f"{symbol}-{currency}-{sec_type}-{exchange}"


def _system_event_type(error_code: int) -> str:
    """Map IBKR connectivity code to VenueSystemEventType string."""
    from ibkr.types import (
        CONNECTIVITY_LOST,
        CONNECTIVITY_RESTORED_DATA_LOST,
        CONNECTIVITY_RESTORED_DATA_OK,
        SOCKET_PORT_RESET,
    )

    if error_code == CONNECTIVITY_LOST:
        return "disconnected"
    elif error_code in (CONNECTIVITY_RESTORED_DATA_LOST, CONNECTIVITY_RESTORED_DATA_OK):
        return "connected"
    elif error_code == SOCKET_PORT_RESET:
        return "reconnecting"
    return "error"
