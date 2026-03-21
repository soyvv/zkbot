"""Shared test fixtures for IBKR venue integration tests."""

from __future__ import annotations

import asyncio
from dataclasses import dataclass, field
from typing import Any
from unittest.mock import AsyncMock, MagicMock

import pytest


# ---------------------------------------------------------------------------
# Minimal mock objects that mimic ib_async types used by the adaptor.
# These avoid importing ib_async in pure unit tests.
# ---------------------------------------------------------------------------


@dataclass
class MockContract:
    symbol: str = ""
    secType: str = ""
    exchange: str = ""
    currency: str = ""
    conId: int = 0


@dataclass
class MockOrder:
    orderId: int = 0
    action: str = ""
    totalQuantity: float = 0.0
    lmtPrice: float = 0.0
    orderType: str = "LMT"
    permId: int = 0


@dataclass
class MockOrderStatus:
    orderId: int = 0
    status: str = ""
    filled: float = 0.0
    remaining: float = 0.0
    avgFillPrice: float = 0.0
    permId: int = 0
    lastFillPrice: float = 0.0


@dataclass
class MockTrade:
    contract: MockContract = field(default_factory=MockContract)
    order: MockOrder = field(default_factory=MockOrder)
    orderStatus: MockOrderStatus = field(default_factory=MockOrderStatus)
    fills: list = field(default_factory=list)


@dataclass
class MockExecution:
    execId: str = ""
    orderId: int = 0
    shares: float = 0.0
    price: float = 0.0
    side: str = ""
    time: str = ""
    permId: int = 0


@dataclass
class MockCommissionReport:
    execId: str = ""
    commission: float = 0.0
    currency: str = "USD"


@dataclass
class MockFill:
    contract: MockContract = field(default_factory=MockContract)
    execution: MockExecution = field(default_factory=MockExecution)
    commissionReport: MockCommissionReport = field(default_factory=MockCommissionReport)
    time: float = 0.0


@dataclass
class MockPosition:
    account: str = ""
    contract: MockContract = field(default_factory=MockContract)
    position: float = 0.0
    avgCost: float = 0.0


@dataclass
class MockAccountValue:
    tag: str = ""
    value: str = ""
    currency: str = ""
    account: str = ""


class _MockClient:
    """Minimal mock of ib_async.client.Client."""

    def __init__(self, mock_ib: "MockIB"):
        self._mock_ib = mock_ib

    def getReqId(self) -> int:
        oid = self._mock_ib._next_order_id
        return oid


class MockIB:
    """Minimal mock of ib_async.IB for unit testing."""

    def __init__(self):
        self._connected = False
        self._next_order_id = 100
        self.client = _MockClient(self)
        # Event callbacks (lists of callables, mimicking ib_async Event)
        self.connectedEvent = _MockEvent()
        self.disconnectedEvent = _MockEvent()
        self.errorEvent = _MockEvent()
        self.openOrderEvent = _MockEvent()
        self.orderStatusEvent = _MockEvent()
        self.execDetailsEvent = _MockEvent()
        self.positionEvent = _MockEvent()
        self.accountValueEvent = _MockEvent()
        self.newOrderEvent = _MockEvent()
        # Stored state
        self._open_trades: list[MockTrade] = []
        self._fills: list[MockFill] = []
        self._positions: list[MockPosition] = []
        self._account_values: list[MockAccountValue] = []

    async def connectAsync(self, host: str, port: int, clientId: int, **kwargs) -> None:
        self._connected = True
        self.connectedEvent.emit()

    def disconnect(self) -> None:
        self._connected = False
        self.disconnectedEvent.emit()

    def isConnected(self) -> bool:
        return self._connected

    def placeOrder(self, contract, order) -> MockTrade:
        trade = MockTrade(contract=contract, order=order)
        self._open_trades.append(trade)
        return trade

    def cancelOrder(self, order, **kwargs) -> MockTrade:
        return MockTrade(order=order)

    def openTrades(self) -> list[MockTrade]:
        return list(self._open_trades)

    def openOrders(self) -> list[MockOrder]:
        return [t.order for t in self._open_trades]

    def fills(self) -> list[MockFill]:
        return list(self._fills)

    def positions(self) -> list[MockPosition]:
        return list(self._positions)

    def accountValues(self) -> list[MockAccountValue]:
        return list(self._account_values)

    async def reqOpenOrdersAsync(self) -> list[MockTrade]:
        return self._open_trades

    def reqAccountUpdates(self, subscribe: bool, account: str = "") -> None:
        pass

    def reqPositions(self) -> None:
        pass

    # Helper to set nextValidId for tests
    def _set_next_order_id(self, oid: int) -> None:
        self._next_order_id = oid


class _MockEvent:
    """Minimal event emitter matching ib_async Event interface."""

    def __init__(self):
        self._handlers: list = []

    def __iadd__(self, handler):
        self._handlers.append(handler)
        return self

    def __isub__(self, handler):
        self._handlers.remove(handler)
        return self

    def emit(self, *args, **kwargs):
        for h in self._handlers:
            h(*args, **kwargs)


@pytest.fixture
def mock_ib() -> MockIB:
    return MockIB()


@pytest.fixture
def sample_gw_config() -> dict:
    return {
        "host": "127.0.0.1",
        "port": 7497,
        "client_id": 1,
        "account_code": "DU123456",
        "mode": "paper",
        "read_only": False,
    }
