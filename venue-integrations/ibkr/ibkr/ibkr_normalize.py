"""Route ib_async callbacks into an async queue of Phase 12A event dicts.

TODO: order_report events should use ``payload_bytes`` (protobuf-serialized OrderReport)
once the Python proto package exists. Currently uses dict ``payload`` per the bridge doc's
"start with dict/list" recommendation (13-python-venue-bridge.md §3).
"""

from __future__ import annotations

import asyncio
import logging
from typing import Any

from ibkr.id_map import OrderIdMap
from ibkr.types import IBKR_STATUS_MAP, CONNECTIVITY_CODES, now_ms

log = logging.getLogger(__name__)


class EventRouter:
    """Bridge between ib_async event callbacks and the ``next_event()`` contract.

    Callbacks from ib_async are synchronous. This router normalizes them into
    Phase 12A-shaped event dicts and enqueues them for async consumption.
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

        # Bind permId if we know this ib_order_id
        if order.permId:
            self._id_map.bind_perm_id(order.orderId, order.permId)

        entry = self._id_map.lookup_by_ib_order_id(order.orderId)
        if entry is None:
            log.debug("on_open_order: unknown orderId=%d (external order?)", order.orderId)
            return

        status = IBKR_STATUS_MAP.get(order_status.status, "booked")
        exch_ref = str(entry.perm_id) if entry.perm_id else str(order.orderId)

        event = {
            "event_type": "order_report",
            "payload": {
                "order_id": entry.client_order_id,
                "exch_order_ref": exch_ref,
                "instrument": _instrument_from_contract(contract),
                "status": status,
                "filled_qty": float(order_status.filled),
                "unfilled_qty": float(order_status.remaining),
                "avg_price": float(order_status.avgFillPrice),
                "timestamp": now_ms(),
            },
        }
        self._enqueue(event)

    def on_order_status(self, trade: Any) -> None:
        """Handle orderStatusEvent — status changes for known orders."""
        order = trade.order
        order_status = trade.orderStatus

        entry = self._id_map.lookup_by_ib_order_id(order.orderId)
        if entry is None:
            return

        # Bind permId if available and not yet bound
        if order.permId and entry.perm_id is None:
            self._id_map.bind_perm_id(order.orderId, order.permId)
            entry = self._id_map.lookup_by_ib_order_id(order.orderId)

        status = IBKR_STATUS_MAP.get(order_status.status, "booked")
        exch_ref = str(entry.perm_id) if entry.perm_id else str(order.orderId)

        event = {
            "event_type": "order_report",
            "payload": {
                "order_id": entry.client_order_id,
                "exch_order_ref": exch_ref,
                "instrument": entry.instrument,
                "status": status,
                "filled_qty": float(order_status.filled),
                "unfilled_qty": float(order_status.remaining),
                "avg_price": float(order_status.avgFillPrice),
                "timestamp": now_ms(),
            },
        }
        self._enqueue(event)

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
        buysell = 1 if execution.side == "BOT" else 2

        event = {
            "event_type": "order_report",
            "payload": {
                "order_id": entry.client_order_id,
                "exch_order_ref": exch_ref,
                "instrument": _instrument_from_contract(contract),
                "status": "partially_filled",  # gateway layer determines final fill status
                "filled_qty": float(execution.shares),
                "unfilled_qty": 0.0,  # individual exec doesn't carry remaining
                "avg_price": float(execution.price),
                "timestamp": now_ms(),
                "trades": [
                    {
                        "exch_trade_id": execution.execId,
                        "order_id": entry.client_order_id,
                        "exch_order_ref": exch_ref,
                        "instrument": _instrument_from_contract(contract),
                        "buysell_type": buysell,
                        "filled_qty": float(execution.shares),
                        "filled_price": float(execution.price),
                        "timestamp": now_ms(),
                    }
                ],
            },
        }
        self._enqueue(event)

    def on_position(self, position: Any) -> None:
        """Handle positionEvent — position snapshot."""
        if self._account_code and getattr(position, "account", "") != self._account_code:
            return
        contract = position.contract
        qty = float(position.position)
        long_short = 1 if qty >= 0 else 2  # 1=long, 2=short

        event = {
            "event_type": "position",
            "payload": [
                {
                    "instrument": _instrument_from_contract(contract),
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
        # Only emit for cash balance tags
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
        """Route errors: order-specific -> rejection, connectivity -> system event."""
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

        # Order-specific error: look up by reqId (which is the ib_order_id)
        if reqId > 0:
            entry = self._id_map.lookup_by_ib_order_id(reqId)
            if entry is not None:
                event = {
                    "event_type": "order_report",
                    "payload": {
                        "order_id": entry.client_order_id,
                        "exch_order_ref": str(entry.perm_id or reqId),
                        "instrument": entry.instrument,
                        "status": "rejected",
                        "filled_qty": 0.0,
                        "unfilled_qty": 0.0,
                        "avg_price": 0.0,
                        "timestamp": now_ms(),
                        "error_code": errorCode,
                        "error_message": errorString,
                    },
                }
                self._enqueue(event)
                return

        # Generic error — log but don't enqueue unless it's important
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
    """Best-effort instrument string from an IB contract object."""
    symbol = getattr(contract, "symbol", "") or ""
    currency = getattr(contract, "currency", "") or ""
    sec_type = getattr(contract, "secType", "") or ""
    exchange = getattr(contract, "exchange", "") or ""
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
