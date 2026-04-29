"""Tests for EventRouter."""

from __future__ import annotations

import asyncio

import pytest

from zk.exch_gw.v1 import exch_gw_pb2 as gw_pb

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


def _parse_report(event: dict) -> gw_pb.OrderReport:
    assert event["event_type"] == "order_report"
    assert "payload_bytes" in event
    report = gw_pb.OrderReport()
    report.ParseFromString(event["payload_bytes"])
    return report


def _state_entry(report: gw_pb.OrderReport) -> gw_pb.OrderStateReport:
    for entry in report.order_report_entries:
        if entry.report_type == gw_pb.ORDER_REP_TYPE_STATE:
            return entry.order_state_report
    raise AssertionError("no STATE entry in report")


def _trade_entries(report: gw_pb.OrderReport) -> list[gw_pb.TradeReport]:
    return [
        entry.trade_report
        for entry in report.order_report_entries
        if entry.report_type == gw_pb.ORDER_REP_TYPE_TRADE
    ]


def _exec_entries(report: gw_pb.OrderReport) -> list[gw_pb.ExecReport]:
    return [
        entry.exec_report
        for entry in report.order_report_entries
        if entry.report_type == gw_pb.ORDER_REP_TYPE_EXEC
    ]


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
        report = _parse_report(event)
        assert report.order_id == 1
        state = _state_entry(report)
        assert state.exch_order_status == gw_pb.EXCH_ORDER_STATUS_BOOKED
        assert state.unfilled_qty == 50.0

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

        entry = id_map.lookup_by_perm_id(5000)
        assert entry is not None
        assert entry.client_order_id == 1

        event = await asyncio.wait_for(router.next_event(), timeout=1.0)
        report = _parse_report(event)
        assert report.exch_order_ref == "5000"

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
        report = _parse_report(event)
        trades = _trade_entries(report)
        assert len(trades) == 1
        t = trades[0]
        assert t.exch_trade_id == "exec123"
        assert t.filled_qty == 25.0
        assert t.filled_price == 150.50
        assert t.order_info.buy_sell_type == 1  # BOT → Buy

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
        assert p["instrument_type"] == 7  # zk.common.v1.InstrumentType.INST_TYPE_STOCK
        assert p["qty"] == 100.0
        assert p["long_short_type"] == 1  # positive = long

    @pytest.mark.asyncio
    async def test_position_stk_forces_smart_routing(self) -> None:
        """STK position events must publish with routing='SMART' to match
        refdata's `instrument_exch`. IBKR rewrites contract.exchange to the
        primary listing exchange (ARCA, NASDAQ) on positionEvent — using that
        directly would diverge from refdata and break OMS instrument lookup.
        """
        router, _ = _make_router()
        pos = MockPosition(
            contract=MockContract(symbol="SPY", currency="USD", secType="STK", exchange="ARCA"),
            position=1.0,
        )
        router.on_position(pos)

        event = await asyncio.wait_for(router.next_event(), timeout=1.0)
        p = event["payload"][0]
        assert p["instrument"] == "SPY-USD-STK-SMART"  # NOT SPY-USD-STK-ARCA
        assert p["instrument_type"] == 7  # STOCK

    @pytest.mark.asyncio
    async def test_position_fut_keeps_contract_exchange(self) -> None:
        """Non-STK secTypes keep contract.exchange (e.g. FUT routes to a
        specific venue like CME, not SMART).
        """
        router, _ = _make_router()
        pos = MockPosition(
            contract=MockContract(symbol="ES", currency="USD", secType="FUT", exchange="CME"),
            position=2.0,
        )
        router.on_position(pos)

        event = await asyncio.wait_for(router.next_event(), timeout=1.0)
        p = event["payload"][0]
        assert p["instrument"] == "ES-USD-FUT-CME"
        assert p["instrument_type"] == 3  # FUTURE

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

        assert router._queue.empty()

    @pytest.mark.asyncio
    async def test_error_routes_order_rejection(self) -> None:
        router, id_map = _make_router()
        id_map.register(client_order_id=1, ib_order_id=100, instrument="AAPL-USD-STK-SMART")

        router.on_error(reqId=100, errorCode=201, errorString="Order rejected", contract=None)

        event = await asyncio.wait_for(router.next_event(), timeout=1.0)
        report = _parse_report(event)
        state = _state_entry(report)
        assert state.exch_order_status == gw_pb.EXCH_ORDER_STATUS_EXCH_REJECTED
        exec_reports = _exec_entries(report)
        assert len(exec_reports) == 1
        assert exec_reports[0].exec_type == gw_pb.EXCH_EXEC_TYPE_REJECTED
        assert "201" in exec_reports[0].exec_message

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
