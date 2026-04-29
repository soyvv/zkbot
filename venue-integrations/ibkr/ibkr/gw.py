"""IBKR gateway adaptor — Phase 12A Python contract implementation.

This adaptor follows the dict-based async contract defined in
docs/system-redesign-plan/plan/13-python-venue-bridge.md and is designed
to be loaded by the shared PyO3 bridge when Phase 12A lands.

Key design decisions:
- orderId is used for session-scoped submission; permId is the durable ID
- Hybrid callback-plus-query mode for correctness
- Token-bucket rate limiter at 40 msg/s (below IBKR's 50/s ceiling)
- No orders dispatched before nextValidId handshake completes
"""

from __future__ import annotations

import asyncio
import logging
from typing import Any

import ib_async

from ibkr.config import IbkrGwConfig
from ibkr.ibkr_client import IbkrConnection
from ibkr.ibkr_contracts import ContractTranslator
from ibkr.ibkr_normalize import EventRouter
from ibkr.id_map import OrderIdMap
from ibkr.rate_limiter import TokenBucketRateLimiter
from ibkr.types import IBKR_STATUS_MAP, LIVE, now_ms

log = logging.getLogger(__name__)


class IbkrGatewayAdaptor:
    """IBKR trading gateway adaptor.

    Implements the Phase 12A Python venue adaptor contract:
    - ``__init__(config: dict)``
    - ``async connect() -> None``
    - ``async place_order(req: dict) -> dict``
    - ``async cancel_order(req: dict) -> dict``
    - ``async query_balance(req: dict) -> list[dict]``
    - ``async query_order(req: dict) -> list[dict]``
    - ``async query_open_orders(req: dict) -> list[dict]``
    - ``async query_trades(req: dict) -> list[dict]``
    - ``async query_funding_fees(req: dict) -> list[dict]``
    - ``async query_positions(req: dict) -> list[dict]``
    - ``async next_event() -> dict``
    """

    def __init__(self, config: dict) -> None:
        self._config = IbkrGwConfig.from_dict(config)
        self._id_map = OrderIdMap()
        self._rate_limiter = TokenBucketRateLimiter(rate=self._config.max_msg_rate)
        self._event_router = EventRouter(self._id_map, account_code=self._config.account_code or "")
        self._contract_translator = ContractTranslator()
        self._conn = IbkrConnection(
            self._config,
            system_event_cb=self._event_router._enqueue,
            on_reconnect_cb=self._on_reconnect,
        )

    async def connect(self) -> None:
        """Connect to TWS/IB Gateway, wait for handshake, register callbacks."""
        await self._conn.connect()
        self._register_callbacks()

        # Subscribe to account updates and positions for streaming events.
        # The sync `reqAccountUpdates` / `reqPositions` wrappers spin their own
        # loop and cannot be called from inside an active asyncio task; use the
        # *Async variants instead.
        account = self._config.account_code or ""
        try:
            await asyncio.wait_for(
                self._conn.ib.reqAccountUpdatesAsync(account), timeout=10.0
            )
        except asyncio.TimeoutError:
            log.warning("reqAccountUpdatesAsync timed out; continuing without snapshot")
        try:
            await asyncio.wait_for(self._conn.ib.reqPositionsAsync(), timeout=10.0)
        except asyncio.TimeoutError:
            log.warning("reqPositionsAsync timed out; continuing without snapshot")
        log.info("IBKR adaptor connected and subscribed (account=%s)", account or "<all>")

    async def place_order(self, req: dict) -> dict:
        """Place an order on IBKR.

        Expected req keys:
        - correlation_id (int): upstream order ID
        - instrument (str): e.g. "AAPL-USD-STK-SMART"
        - buysell_type (int): 1=buy, 2=sell
        - order_type (int): 1=limit (default)
        - price (float): limit price
        - qty (float): order quantity
        """
        if self._config.read_only:
            return {"success": False, "error_message": "adaptor is in read-only mode"}

        if self._conn.state != LIVE:
            return {
                "success": False,
                "error_message": f"not connected (state={self._conn.state})",
            }

        await self._rate_limiter.acquire()

        correlation_id = req["correlation_id"]
        instrument = req["instrument"]

        # Allocate session-scoped orderId
        ib_order_id = self._conn.allocate_order_id()

        # Build IB contract and order
        contract = self._contract_translator.to_ib_contract(instrument)
        action = "BUY" if req.get("buysell_type", 1) == 1 else "SELL"

        # order_type: 1=limit (default), 2=market
        order_type = req.get("order_type", 1)
        if order_type == 2:
            order = ib_async.MarketOrder(action=action, totalQuantity=req["qty"])
        else:
            order = ib_async.LimitOrder(
                action=action,
                totalQuantity=req["qty"],
                lmtPrice=req.get("price", 0.0),
            )
        order.orderId = ib_order_id

        # Register in ID map before sending (so callbacks can find it)
        self._id_map.register(
            client_order_id=correlation_id,
            ib_order_id=ib_order_id,
            instrument=instrument,
        )

        try:
            self._conn.ib.placeOrder(contract, order)
        except Exception as e:
            self._id_map.remove(correlation_id)
            log.error("placeOrder failed: %s", e)
            return {"success": False, "error_message": str(e)}

        log.info(
            "placed order: correlation_id=%d ib_order_id=%d %s %s qty=%.2f price=%s",
            correlation_id,
            ib_order_id,
            action,
            instrument,
            req["qty"],
            req.get("price", "MKT"),
        )

        # exch_order_ref starts as orderId; updated to permId once openOrder fires
        return {"success": True, "exch_order_ref": str(ib_order_id)}

    async def cancel_order(self, req: dict) -> dict:
        """Cancel an order on IBKR.

        Expected req keys:
        - order_id (int): client_order_id (correlation_id)
        - exch_order_ref (str, optional): permId or orderId as string
        """
        if self._config.read_only:
            return {"success": False, "error_message": "adaptor is in read-only mode"}

        if self._conn.state != LIVE:
            return {
                "success": False,
                "error_message": f"not connected (state={self._conn.state})",
            }

        await self._rate_limiter.acquire()

        order_id = req.get("order_id", 0)
        exch_ref = req.get("exch_order_ref")
        entry = self._id_map.lookup_by_client_id(order_id) if order_id else None

        # Fall back to exch_order_ref (permId or ib_order_id) if client_id lookup fails
        if entry is None and exch_ref:
            try:
                ref_int = int(exch_ref)
            except (ValueError, TypeError):
                ref_int = None
            if ref_int is not None:
                entry = self._id_map.lookup_by_perm_id(ref_int)
                if entry is None:
                    entry = self._id_map.lookup_by_ib_order_id(ref_int)

        if entry is None or entry.ib_order_id is None:
            return {"success": False, "error_message": f"order {order_id} not found in id map"}

        order = ib_async.Order(orderId=entry.ib_order_id)
        try:
            self._conn.ib.cancelOrder(order)
        except Exception as e:
            log.error("cancelOrder failed: %s", e)
            return {"success": False, "error_message": str(e)}

        log.info(
            "cancel requested: order_id=%d ib_order_id=%d", order_id, entry.ib_order_id
        )
        return {"success": True}

    async def query_balance(self, req: dict) -> list[dict]:
        """Query account balances from cached account values."""
        await self._rate_limiter.acquire()
        values = self._conn.ib.accountValues()
        account = self._config.account_code

        result: list[dict] = []
        seen: set[str] = set()
        for v in values:
            if v.tag != "CashBalance":
                continue
            if account and getattr(v, "account", "") != account:
                continue
            if v.currency in seen:
                continue
            seen.add(v.currency)
            try:
                qty = float(v.value)
            except (ValueError, TypeError):
                continue
            result.append({
                "asset": v.currency,
                "total_qty": qty,
                "avail_qty": qty,
                "frozen_qty": 0.0,
            })
        return result

    async def query_order(self, req: dict) -> list[dict]:
        """Query a specific order by client_order_id or exch_order_ref."""
        await self._rate_limiter.acquire()
        trades = self._conn.ib.openTrades()
        return self._filter_and_convert_trades(trades, req)

    async def query_open_orders(self, req: dict) -> list[dict]:
        """Query all open orders (may trigger a fresh request to TWS).

        Optional req filters: order_id, exch_order_ref, instrument.
        """
        await self._rate_limiter.acquire()
        await self._conn.ib.reqOpenOrdersAsync()
        trades = self._conn.ib.openTrades()
        return self._filter_and_convert_trades(trades, req)

    async def query_trades(self, req: dict) -> list[dict]:
        """Query recent fills/executions.

        Optional req filters: order_id, exch_order_ref, instrument.
        """
        await self._rate_limiter.acquire()
        fills = self._conn.ib.fills()
        filter_oid = req.get("order_id")
        filter_ref = req.get("exch_order_ref")
        filter_inst = req.get("instrument")

        result: list[dict] = []
        for f in fills:
            ex = f.execution
            entry = self._id_map.lookup_by_ib_order_id(ex.orderId)
            client_oid = entry.client_order_id if entry else 0
            exch_ref = str(entry.perm_id or ex.orderId) if entry else str(ex.orderId)
            inst = self._contract_translator.from_ib_contract(f.contract)

            # Apply filters
            if filter_oid is not None and client_oid != filter_oid:
                continue
            if filter_ref is not None and exch_ref != filter_ref:
                continue
            if filter_inst is not None and inst != filter_inst:
                continue

            buysell = 1 if ex.side == "BOT" else 2
            result.append({
                "exch_trade_id": ex.execId,
                "order_id": client_oid,
                "exch_order_ref": exch_ref,
                "instrument": inst,
                "buysell_type": buysell,
                "filled_qty": float(ex.shares),
                "filled_price": float(ex.price),
                "timestamp": now_ms(),
            })
        return result

    async def query_funding_fees(self, req: dict) -> list[dict]:
        """Query funding fees. IBKR does not have perpetual funding fees — returns empty."""
        return []

    async def query_positions(self, req: dict) -> list[dict]:
        """Query current positions."""
        await self._rate_limiter.acquire()
        positions = self._conn.ib.positions()
        account = self._config.account_code

        result: list[dict] = []
        for p in positions:
            if account and getattr(p, "account", "") != account:
                continue
            qty = float(p.position)
            if qty == 0:
                continue
            long_short = 1 if qty > 0 else 2
            result.append({
                "instrument": self._contract_translator.from_ib_contract(p.contract),
                "long_short_type": long_short,
                "qty": abs(qty),
                "avail_qty": abs(qty),
                "frozen_qty": 0.0,
                "account_id": 0,
            })
        return result

    async def next_event(self) -> dict:
        """Block until the next venue event is available."""
        return await self._event_router.next_event()

    # ------------------------------------------------------------------
    # Internal helpers
    # ------------------------------------------------------------------

    def _register_callbacks(self) -> None:
        """Register event callbacks. Safe to call multiple times (deduplicates)."""
        ib = self._conn.ib
        router = self._event_router
        # Remove first to prevent duplicate registration on reconnect
        for event, handler in [
            (ib.openOrderEvent, router.on_open_order),
            (ib.orderStatusEvent, router.on_order_status),
            (ib.execDetailsEvent, router.on_exec_details),
            (ib.positionEvent, router.on_position),
            (ib.accountValueEvent, router.on_account_value),
            (ib.errorEvent, router.on_error),
        ]:
            try:
                event -= handler
            except ValueError:
                pass
            event += handler

    def _on_reconnect(self) -> None:
        """Re-establish subscriptions after a successful reconnect."""
        self._register_callbacks()
        account = self._config.account_code or ""
        try:
            loop = asyncio.get_event_loop()
            loop.create_task(self._conn.ib.reqAccountUpdatesAsync(account))
            loop.create_task(self._conn.ib.reqPositionsAsync())
        except RuntimeError:
            self._conn.ib.reqAccountUpdates(account)
            self._conn.ib.reqPositions()
        log.info("post-reconnect: resubscribed to account updates and positions")

    def _trade_to_order_fact(self, trade: Any) -> dict:
        """Convert an ib_async Trade to a VenueOrderFact dict."""
        order = trade.order
        os = trade.orderStatus
        entry = self._id_map.lookup_by_ib_order_id(order.orderId)
        client_oid = entry.client_order_id if entry else 0
        exch_ref = str(entry.perm_id or order.orderId) if entry else str(order.orderId)
        status = IBKR_STATUS_MAP.get(os.status, "booked")
        instrument = (
            entry.instrument
            if entry
            else self._contract_translator.from_ib_contract(trade.contract)
        )

        return {
            "order_id": client_oid,
            "exch_order_ref": exch_ref,
            "instrument": instrument,
            "status": status,
            "filled_qty": float(os.filled),
            "unfilled_qty": float(os.remaining),
            "avg_price": float(os.avgFillPrice),
            "timestamp": now_ms(),
        }

    def _filter_and_convert_trades(self, trades: list, req: dict) -> list[dict]:
        """Filter trades by query criteria and convert to order fact dicts."""
        order_id = req.get("order_id")
        exch_ref = req.get("exch_order_ref")
        instrument = req.get("instrument")

        result: list[dict] = []
        for t in trades:
            entry = self._id_map.lookup_by_ib_order_id(t.order.orderId)
            if order_id is not None and (entry is None or entry.client_order_id != order_id):
                continue
            if exch_ref is not None:
                perm_str = str(entry.perm_id) if entry and entry.perm_id else None
                oid_str = str(t.order.orderId)
                if exch_ref != perm_str and exch_ref != oid_str:
                    continue
            if instrument is not None:
                inst = (
                    entry.instrument
                    if entry
                    else self._contract_translator.from_ib_contract(t.contract)
                )
                if inst != instrument:
                    continue
            result.append(self._trade_to_order_fact(t))
        return result
