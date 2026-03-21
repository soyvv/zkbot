"""Tests for IbkrGatewayAdaptor with mocked IB connection."""

from __future__ import annotations

import asyncio
from unittest.mock import patch, MagicMock

import pytest

from ibkr.config import IbkrGwConfig
from ibkr.types import LIVE, DISCONNECTED
from tests.conftest import (
    MockIB,
    MockAccountValue,
    MockContract,
    MockExecution,
    MockFill,
    MockOrder,
    MockOrderStatus,
    MockPosition,
    MockTrade,
)


def _make_adaptor(config_overrides: dict | None = None) -> tuple:
    """Create an IbkrGatewayAdaptor with a MockIB injected."""
    from ibkr.gw import IbkrGatewayAdaptor

    cfg = {
        "host": "127.0.0.1",
        "port": 7497,
        "client_id": 1,
        "mode": "paper",
        "account_code": "DU123456",
    }
    if config_overrides:
        cfg.update(config_overrides)

    adaptor = IbkrGatewayAdaptor(cfg)

    # Inject MockIB
    mock_ib = MockIB()
    mock_ib._set_next_order_id(500)
    adaptor._conn._ib = mock_ib
    return adaptor, mock_ib


class TestIbkrGatewayAdaptor:
    @pytest.mark.asyncio
    async def test_connect_and_handshake(self) -> None:
        adaptor, mock_ib = _make_adaptor()
        await adaptor._conn.connect()
        adaptor._register_callbacks()
        assert adaptor._conn.state == LIVE

    @pytest.mark.asyncio
    async def test_place_order_success(self) -> None:
        adaptor, mock_ib = _make_adaptor()
        await adaptor._conn.connect()
        adaptor._register_callbacks()
        adaptor._conn._state = LIVE  # ensure state

        req = {
            "correlation_id": 1,
            "instrument": "AAPL-USD-STK-SMART",
            "buysell_type": 1,
            "qty": 100.0,
            "price": 150.25,
        }
        result = await adaptor.place_order(req)
        assert result["success"] is True
        assert result["exch_order_ref"] == "500"

        # Verify ID map
        entry = adaptor._id_map.lookup_by_client_id(1)
        assert entry is not None
        assert entry.ib_order_id == 500
        assert entry.instrument == "AAPL-USD-STK-SMART"

    @pytest.mark.asyncio
    async def test_place_order_sell(self) -> None:
        adaptor, mock_ib = _make_adaptor()
        await adaptor._conn.connect()
        adaptor._register_callbacks()
        adaptor._conn._state = LIVE

        req = {
            "correlation_id": 2,
            "instrument": "MSFT-USD-STK-SMART",
            "buysell_type": 2,
            "qty": 50.0,
            "price": 300.00,
        }
        result = await adaptor.place_order(req)
        assert result["success"] is True
        # Should have used orderId 500 (first allocation)
        assert result["exch_order_ref"] == "500"

    @pytest.mark.asyncio
    async def test_place_order_increments_order_id(self) -> None:
        adaptor, mock_ib = _make_adaptor()
        await adaptor._conn.connect()
        adaptor._register_callbacks()
        adaptor._conn._state = LIVE

        for i in range(3):
            req = {
                "correlation_id": i + 1,
                "instrument": "AAPL-USD-STK-SMART",
                "buysell_type": 1,
                "qty": 10.0,
                "price": 150.0,
            }
            result = await adaptor.place_order(req)
            assert result["exch_order_ref"] == str(500 + i)

    @pytest.mark.asyncio
    async def test_place_order_read_only_blocked(self) -> None:
        adaptor, _ = _make_adaptor({"read_only": True})
        await adaptor._conn.connect()
        adaptor._conn._state = LIVE

        req = {
            "correlation_id": 1,
            "instrument": "AAPL-USD-STK-SMART",
            "buysell_type": 1,
            "qty": 100.0,
            "price": 150.0,
        }
        result = await adaptor.place_order(req)
        assert result["success"] is False
        assert "read-only" in result["error_message"]

    @pytest.mark.asyncio
    async def test_place_order_not_connected(self) -> None:
        adaptor, _ = _make_adaptor()
        # Don't connect
        req = {
            "correlation_id": 1,
            "instrument": "AAPL-USD-STK-SMART",
            "buysell_type": 1,
            "qty": 100.0,
            "price": 150.0,
        }
        result = await adaptor.place_order(req)
        assert result["success"] is False
        assert "not connected" in result["error_message"]

    @pytest.mark.asyncio
    async def test_cancel_order_success(self) -> None:
        adaptor, mock_ib = _make_adaptor()
        await adaptor._conn.connect()
        adaptor._register_callbacks()
        adaptor._conn._state = LIVE

        # Place first
        place_req = {
            "correlation_id": 1,
            "instrument": "AAPL-USD-STK-SMART",
            "buysell_type": 1,
            "qty": 100.0,
            "price": 150.0,
        }
        await adaptor.place_order(place_req)

        # Cancel
        cancel_req = {"order_id": 1}
        result = await adaptor.cancel_order(cancel_req)
        assert result["success"] is True

    @pytest.mark.asyncio
    async def test_cancel_order_not_found(self) -> None:
        adaptor, _ = _make_adaptor()
        await adaptor._conn.connect()
        adaptor._conn._state = LIVE

        result = await adaptor.cancel_order({"order_id": 999})
        assert result["success"] is False
        assert "not found" in result["error_message"]

    @pytest.mark.asyncio
    async def test_query_balance(self) -> None:
        adaptor, mock_ib = _make_adaptor()
        await adaptor._conn.connect()
        adaptor._conn._state = LIVE

        mock_ib._account_values = [
            MockAccountValue(tag="CashBalance", value="100000.50", currency="USD", account="DU123456"),
            MockAccountValue(tag="CashBalance", value="5000.00", currency="EUR", account="DU123456"),
            MockAccountValue(tag="NetLiquidation", value="500000", currency="USD", account="DU123456"),
        ]

        result = await adaptor.query_balance({})
        assert len(result) == 2
        usd = next(b for b in result if b["asset"] == "USD")
        assert usd["total_qty"] == 100000.50
        eur = next(b for b in result if b["asset"] == "EUR")
        assert eur["total_qty"] == 5000.0

    @pytest.mark.asyncio
    async def test_query_positions(self) -> None:
        adaptor, mock_ib = _make_adaptor()
        await adaptor._conn.connect()
        adaptor._conn._state = LIVE

        mock_ib._positions = [
            MockPosition(
                account="DU123456",
                contract=MockContract(symbol="AAPL", currency="USD", secType="STK", exchange="SMART"),
                position=100.0,
                avgCost=150.0,
            ),
            MockPosition(
                account="DU123456",
                contract=MockContract(symbol="TSLA", currency="USD", secType="STK", exchange="SMART"),
                position=-50.0,
                avgCost=200.0,
            ),
        ]

        result = await adaptor.query_positions({})
        assert len(result) == 2
        aapl = next(p for p in result if "AAPL" in p["instrument"])
        assert aapl["qty"] == 100.0
        assert aapl["long_short_type"] == 1
        tsla = next(p for p in result if "TSLA" in p["instrument"])
        assert tsla["qty"] == 50.0
        assert tsla["long_short_type"] == 2

    @pytest.mark.asyncio
    async def test_query_trades(self) -> None:
        adaptor, mock_ib = _make_adaptor()
        await adaptor._conn.connect()
        adaptor._register_callbacks()
        adaptor._conn._state = LIVE

        # Register an order first
        adaptor._id_map.register(client_order_id=1, ib_order_id=500, instrument="AAPL-USD-STK-SMART")

        mock_ib._fills = [
            MockFill(
                contract=MockContract(symbol="AAPL", currency="USD", secType="STK", exchange="SMART"),
                execution=MockExecution(
                    execId="exec1", orderId=500, shares=50.0, price=150.25, side="BOT"
                ),
            ),
        ]

        result = await adaptor.query_trades({})
        assert len(result) == 1
        assert result[0]["exch_trade_id"] == "exec1"
        assert result[0]["order_id"] == 1
        assert result[0]["filled_qty"] == 50.0
        assert result[0]["buysell_type"] == 1

    @pytest.mark.asyncio
    async def test_next_event_order_report(self) -> None:
        adaptor, mock_ib = _make_adaptor()
        await adaptor._conn.connect()
        adaptor._register_callbacks()
        adaptor._conn._state = LIVE

        # Place an order to register in id_map
        adaptor._id_map.register(client_order_id=1, ib_order_id=500, instrument="AAPL-USD-STK-SMART")

        # Simulate openOrder callback
        trade = MockTrade(
            contract=MockContract(symbol="AAPL", currency="USD", secType="STK", exchange="SMART"),
            order=MockOrder(orderId=500, permId=8000),
            orderStatus=MockOrderStatus(
                orderId=500, status="Submitted", filled=0.0, remaining=100.0, permId=8000
            ),
        )
        adaptor._event_router.on_open_order(trade)

        event = await asyncio.wait_for(adaptor.next_event(), timeout=1.0)
        assert event["event_type"] == "order_report"
        assert event["payload"]["order_id"] == 1
        assert event["payload"]["exch_order_ref"] == "8000"  # permId bound
        assert event["payload"]["status"] == "booked"

    @pytest.mark.asyncio
    async def test_query_open_orders(self) -> None:
        adaptor, mock_ib = _make_adaptor()
        await adaptor._conn.connect()
        adaptor._conn._state = LIVE

        adaptor._id_map.register(client_order_id=1, ib_order_id=500, instrument="AAPL-USD-STK-SMART")

        mock_ib._open_trades = [
            MockTrade(
                contract=MockContract(symbol="AAPL", currency="USD", secType="STK", exchange="SMART"),
                order=MockOrder(orderId=500, permId=8000),
                orderStatus=MockOrderStatus(
                    orderId=500, status="Submitted", filled=0.0, remaining=100.0
                ),
            ),
        ]

        result = await adaptor.query_open_orders({})
        assert len(result) == 1
        assert result[0]["order_id"] == 1
        assert result[0]["status"] == "booked"

    @pytest.mark.asyncio
    async def test_cancel_order_by_exch_order_ref_perm_id(self) -> None:
        """Cancel using exch_order_ref (permId) when order_id is unknown."""
        adaptor, mock_ib = _make_adaptor()
        await adaptor._conn.connect()
        adaptor._register_callbacks()
        adaptor._conn._state = LIVE

        # Register an order and bind permId
        adaptor._id_map.register(client_order_id=1, ib_order_id=500, instrument="AAPL-USD-STK-SMART")
        adaptor._id_map.bind_perm_id(500, 8000)

        # Cancel by exch_order_ref (permId), without order_id
        result = await adaptor.cancel_order({"exch_order_ref": "8000"})
        assert result["success"] is True

    @pytest.mark.asyncio
    async def test_cancel_order_by_exch_order_ref_ib_order_id(self) -> None:
        """Cancel using exch_order_ref (ib_order_id) when permId not bound."""
        adaptor, mock_ib = _make_adaptor()
        await adaptor._conn.connect()
        adaptor._register_callbacks()
        adaptor._conn._state = LIVE

        adaptor._id_map.register(client_order_id=1, ib_order_id=500, instrument="AAPL-USD-STK-SMART")

        # Cancel by exch_order_ref (ib_order_id)
        result = await adaptor.cancel_order({"exch_order_ref": "500"})
        assert result["success"] is True

    @pytest.mark.asyncio
    async def test_query_balance_filters_by_account(self) -> None:
        """Only balances for configured account_code should be returned."""
        adaptor, mock_ib = _make_adaptor()
        await adaptor._conn.connect()
        adaptor._conn._state = LIVE

        mock_ib._account_values = [
            MockAccountValue(tag="CashBalance", value="100000", currency="USD", account="DU123456"),
            MockAccountValue(tag="CashBalance", value="50000", currency="USD", account="OTHER_ACCT"),
        ]

        result = await adaptor.query_balance({})
        assert len(result) == 1
        assert result[0]["total_qty"] == 100000.0

    @pytest.mark.asyncio
    async def test_query_positions_filters_by_account(self) -> None:
        """Only positions for configured account_code should be returned."""
        adaptor, mock_ib = _make_adaptor()
        await adaptor._conn.connect()
        adaptor._conn._state = LIVE

        mock_ib._positions = [
            MockPosition(
                account="DU123456",
                contract=MockContract(symbol="AAPL", currency="USD", secType="STK", exchange="SMART"),
                position=100.0,
                avgCost=150.0,
            ),
            MockPosition(
                account="OTHER_ACCT",
                contract=MockContract(symbol="TSLA", currency="USD", secType="STK", exchange="SMART"),
                position=50.0,
                avgCost=200.0,
            ),
        ]

        result = await adaptor.query_positions({})
        assert len(result) == 1
        assert "AAPL" in result[0]["instrument"]

    @pytest.mark.asyncio
    async def test_query_funding_fees_returns_empty(self) -> None:
        adaptor, _ = _make_adaptor()
        result = await adaptor.query_funding_fees({})
        assert result == []

    @pytest.mark.asyncio
    async def test_place_market_order(self) -> None:
        adaptor, mock_ib = _make_adaptor()
        await adaptor._conn.connect()
        adaptor._register_callbacks()
        adaptor._conn._state = LIVE

        req = {
            "correlation_id": 1,
            "instrument": "AAPL-USD-STK-SMART",
            "buysell_type": 1,
            "order_type": 2,  # market
            "qty": 100.0,
        }
        result = await adaptor.place_order(req)
        assert result["success"] is True

    @pytest.mark.asyncio
    async def test_reconnect_does_not_duplicate_callbacks(self) -> None:
        """Calling _register_callbacks multiple times should not duplicate handlers."""
        adaptor, mock_ib = _make_adaptor()
        await adaptor._conn.connect()
        adaptor._register_callbacks()
        adaptor._register_callbacks()  # second call

        # Count handlers — should be 1, not 2
        ib = adaptor._conn.ib
        assert len(ib.openOrderEvent._handlers) == 1
        assert len(ib.orderStatusEvent._handlers) == 1
