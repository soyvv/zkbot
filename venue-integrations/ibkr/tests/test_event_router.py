"""Tests for EventRouter."""

from __future__ import annotations

import asyncio

import pytest

from ibkr.ibkr_normalize import EventRouter
from ibkr.id_map import OrderIdMap
from tests.conftest import (
    MockAccountValue,
    MockContract,
    MockExecution,
    MockFill,
    MockOrder,
    MockOrderStatus,
    MockPosition,
    MockTrade,
)


def _make_router() -> tuple[EventRouter, OrderIdMap]:
    id_map = OrderIdMap()
    router = EventRouter(id_map, queue_size=64)
    return router, id_map


class TestEventRouter:
    @pytest.mark.asyncio
    async def test_order_status_callback_enqueues_order_report(self) -> None:
        router, id_map = _make_router()
        id_map.register(client_order_id=1, ib_order_id=100, instrument="AAPL-USD-STK-SMART")

        trade = MockTrade(
            order=MockOrder(orderId=100, permId=5000),
            orderStatus=MockOrderStatus(
                orderId=100, status="Submitted", filled=0.0, remaining=50.0, permId=5000
            ),
        )
        router.on_order_status(trade)

        event = await asyncio.wait_for(router.next_event(), timeout=1.0)
        assert event["event_type"] == "order_report"
        assert event["payload"]["order_id"] == 1
        assert event["payload"]["status"] == "booked"
        assert event["payload"]["unfilled_qty"] == 50.0

    @pytest.mark.asyncio
    async def test_open_order_binds_perm_id(self) -> None:
        router, id_map = _make_router()
        id_map.register(client_order_id=1, ib_order_id=100, instrument="AAPL-USD-STK-SMART")

        trade = MockTrade(
            contract=MockContract(symbol="AAPL", currency="USD", secType="STK", exchange="SMART"),
            order=MockOrder(orderId=100, permId=5000),
            orderStatus=MockOrderStatus(orderId=100, status="PreSubmitted"),
        )
        router.on_open_order(trade)

        # permId should now be bound
        entry = id_map.lookup_by_perm_id(5000)
        assert entry is not None
        assert entry.client_order_id == 1

        event = await asyncio.wait_for(router.next_event(), timeout=1.0)
        assert event["payload"]["exch_order_ref"] == "5000"

    @pytest.mark.asyncio
    async def test_exec_details_enqueues_trade(self) -> None:
        router, id_map = _make_router()
        id_map.register(client_order_id=1, ib_order_id=100, instrument="AAPL-USD-STK-SMART")
        id_map.bind_perm_id(100, 5000)

        fill = MockFill(
            contract=MockContract(symbol="AAPL", currency="USD", secType="STK", exchange="SMART"),
            execution=MockExecution(
                execId="exec123", orderId=100, shares=25.0, price=150.50, side="BOT", permId=5000
            ),
        )
        trade = MockTrade(order=MockOrder(orderId=100, permId=5000))
        router.on_exec_details(trade, fill)

        event = await asyncio.wait_for(router.next_event(), timeout=1.0)
        assert event["event_type"] == "order_report"
        payload = event["payload"]
        assert len(payload["trades"]) == 1
        t = payload["trades"][0]
        assert t["exch_trade_id"] == "exec123"
        assert t["filled_qty"] == 25.0
        assert t["filled_price"] == 150.50
        assert t["buysell_type"] == 1  # BOT -> buy

    @pytest.mark.asyncio
    async def test_position_callback_enqueues_position_event(self) -> None:
        router, _ = _make_router()
        pos = MockPosition(
            account="DU123",
            contract=MockContract(symbol="MSFT", currency="USD", secType="STK", exchange="SMART"),
            position=100.0,
            avgCost=250.0,
        )
        router.on_position(pos)

        event = await asyncio.wait_for(router.next_event(), timeout=1.0)
        assert event["event_type"] == "position"
        p = event["payload"][0]
        assert p["instrument"] == "MSFT-USD-STK-SMART"
        assert p["qty"] == 100.0
        assert p["long_short_type"] == 1  # positive = long

    @pytest.mark.asyncio
    async def test_position_short(self) -> None:
        router, _ = _make_router()
        pos = MockPosition(
            contract=MockContract(symbol="TSLA", currency="USD", secType="STK", exchange="SMART"),
            position=-50.0,
        )
        router.on_position(pos)

        event = await asyncio.wait_for(router.next_event(), timeout=1.0)
        p = event["payload"][0]
        assert p["long_short_type"] == 2  # negative = short
        assert p["qty"] == 50.0  # absolute value

    @pytest.mark.asyncio
    async def test_account_value_balance(self) -> None:
        router, _ = _make_router()
        val = MockAccountValue(tag="CashBalance", value="100000.50", currency="USD", account="DU1")
        router.on_account_value(val)

        event = await asyncio.wait_for(router.next_event(), timeout=1.0)
        assert event["event_type"] == "balance"
        b = event["payload"][0]
        assert b["asset"] == "USD"
        assert b["total_qty"] == 100000.50

    @pytest.mark.asyncio
    async def test_account_value_non_cash_ignored(self) -> None:
        router, _ = _make_router()
        val = MockAccountValue(tag="NetLiquidation", value="500000", currency="USD")
        router.on_account_value(val)

        # Queue should be empty
        assert router._queue.empty()

    @pytest.mark.asyncio
    async def test_error_routes_order_rejection(self) -> None:
        router, id_map = _make_router()
        id_map.register(client_order_id=1, ib_order_id=100, instrument="AAPL-USD-STK-SMART")

        router.on_error(reqId=100, errorCode=201, errorString="Order rejected", contract=None)

        event = await asyncio.wait_for(router.next_event(), timeout=1.0)
        assert event["event_type"] == "order_report"
        assert event["payload"]["status"] == "rejected"
        assert event["payload"]["error_code"] == 201

    @pytest.mark.asyncio
    async def test_error_routes_connectivity_as_system(self) -> None:
        router, _ = _make_router()
        router.on_error(reqId=-1, errorCode=1100, errorString="Connectivity lost", contract=None)

        event = await asyncio.wait_for(router.next_event(), timeout=1.0)
        assert event["event_type"] == "system"
        assert event["payload"]["event_type"] == "disconnected"

    @pytest.mark.asyncio
    async def test_queue_full_drops_oldest(self) -> None:
        id_map = OrderIdMap()
        router = EventRouter(id_map, queue_size=2)

        # Enqueue 3 events — first should be dropped
        router._enqueue({"event_type": "system", "payload": {"n": 1}})
        router._enqueue({"event_type": "system", "payload": {"n": 2}})
        router._enqueue({"event_type": "system", "payload": {"n": 3}})

        e1 = await asyncio.wait_for(router.next_event(), timeout=1.0)
        e2 = await asyncio.wait_for(router.next_event(), timeout=1.0)
        assert e1["payload"]["n"] == 2  # 1 was dropped
        assert e2["payload"]["n"] == 3

    @pytest.mark.asyncio
    async def test_unknown_order_id_ignored(self) -> None:
        router, _ = _make_router()
        trade = MockTrade(
            order=MockOrder(orderId=999),
            orderStatus=MockOrderStatus(orderId=999, status="Submitted"),
        )
        router.on_order_status(trade)
        assert router._queue.empty()
