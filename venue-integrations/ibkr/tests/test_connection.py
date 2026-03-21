"""Tests for IbkrConnection state machine."""

from __future__ import annotations

import asyncio

import pytest

from ibkr.config import IbkrGwConfig
from ibkr.types import CONNECTING, DEGRADED, DISCONNECTED, LIVE, WAITING_HANDSHAKE
from tests.conftest import MockIB


def _make_config(**overrides) -> IbkrGwConfig:
    base = {
        "host": "127.0.0.1",
        "port": 7497,
        "client_id": 1,
        "mode": "paper",
        "next_valid_id_timeout_s": 2.0,
        "reconnect_delay_s": 0.1,
        "reconnect_max_delay_s": 0.5,
    }
    base.update(overrides)
    return IbkrGwConfig.from_dict(base)


class TestIbkrConnection:
    @pytest.mark.asyncio
    async def test_connect_transitions_to_live(self) -> None:
        """DISCONNECTED -> CONNECTING -> WAITING_HANDSHAKE -> LIVE."""
        # We need to import here to avoid ib_async import at module level in conftest
        from ibkr.ibkr_client import IbkrConnection

        mock_ib = MockIB()
        mock_ib._set_next_order_id(500)
        cfg = _make_config()
        system_events: list[dict] = []

        conn = IbkrConnection(cfg, system_event_cb=system_events.append, ib=mock_ib)
        assert conn.state == DISCONNECTED

        await conn.connect()
        assert conn.state == LIVE
        assert conn.next_order_id == 500

    @pytest.mark.asyncio
    async def test_allocate_order_id_increments(self) -> None:
        from ibkr.ibkr_client import IbkrConnection

        mock_ib = MockIB()
        mock_ib._set_next_order_id(100)
        cfg = _make_config()

        conn = IbkrConnection(cfg, ib=mock_ib)
        await conn.connect()

        assert conn.allocate_order_id() == 100
        assert conn.allocate_order_id() == 101
        assert conn.allocate_order_id() == 102

    @pytest.mark.asyncio
    async def test_allocate_before_handshake_raises(self) -> None:
        from ibkr.ibkr_client import IbkrConnection

        mock_ib = MockIB()
        cfg = _make_config()
        conn = IbkrConnection(cfg, ib=mock_ib)

        with pytest.raises(RuntimeError, match="nextValidId handshake not complete"):
            conn.allocate_order_id()

    @pytest.mark.asyncio
    async def test_disconnect_sets_degraded_then_disconnected(self) -> None:
        from ibkr.ibkr_client import IbkrConnection

        mock_ib = MockIB()
        mock_ib._set_next_order_id(1)
        cfg = _make_config()
        system_events: list[dict] = []

        conn = IbkrConnection(cfg, system_event_cb=system_events.append, ib=mock_ib)
        await conn.connect()
        assert conn.state == LIVE

        await conn.disconnect()
        assert conn.state == DISCONNECTED

    @pytest.mark.asyncio
    async def test_on_disconnected_pushes_system_event(self) -> None:
        from ibkr.ibkr_client import IbkrConnection

        mock_ib = MockIB()
        mock_ib._set_next_order_id(1)
        cfg = _make_config()
        system_events: list[dict] = []

        conn = IbkrConnection(cfg, system_event_cb=system_events.append, ib=mock_ib)
        conn._should_reconnect = False  # disable reconnect for this test
        await conn.connect()

        # Simulate disconnect
        mock_ib.disconnect()

        assert conn.state == DEGRADED
        assert any(e["payload"]["event_type"] == "disconnected" for e in system_events)

    @pytest.mark.asyncio
    async def test_connectivity_code_1100(self) -> None:
        from ibkr.ibkr_client import IbkrConnection

        mock_ib = MockIB()
        mock_ib._set_next_order_id(1)
        cfg = _make_config()
        system_events: list[dict] = []

        conn = IbkrConnection(cfg, system_event_cb=system_events.append, ib=mock_ib)
        conn._should_reconnect = False
        await conn.connect()

        conn._on_error(reqId=-1, errorCode=1100, errorString="Lost connection", contract=None)
        assert conn.state == DEGRADED
        assert any("[1100]" in e["payload"]["message"] for e in system_events)

    @pytest.mark.asyncio
    async def test_connectivity_code_1101(self) -> None:
        from ibkr.ibkr_client import IbkrConnection

        mock_ib = MockIB()
        mock_ib._set_next_order_id(1)
        cfg = _make_config()
        system_events: list[dict] = []

        conn = IbkrConnection(cfg, system_event_cb=system_events.append, ib=mock_ib)
        conn._should_reconnect = False
        await conn.connect()

        conn._on_error(
            reqId=-1, errorCode=1101, errorString="Restored, data lost", contract=None
        )
        # State stays LIVE (1101 is informational, reconnect handles the rest)
        assert any("[1101]" in e["payload"]["message"] for e in system_events)

    @pytest.mark.asyncio
    async def test_graceful_disconnect(self) -> None:
        from ibkr.ibkr_client import IbkrConnection

        mock_ib = MockIB()
        mock_ib._set_next_order_id(1)
        cfg = _make_config()

        conn = IbkrConnection(cfg, ib=mock_ib)
        await conn.connect()
        await conn.disconnect()
        assert conn.state == DISCONNECTED
        assert conn._should_reconnect is False
