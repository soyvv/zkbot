"""Tests for the OANDA RTMD adaptor."""

import asyncio
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

import oanda.proto  # noqa: F401
from zk.rtmd.v1 import rtmd_pb2 as rtmd_pb

from oanda.rtmd import OandaRtmdAdaptor


def _make_config():
    return {
        "environment": "practice",
        "account_id": "101-001-1234567-001",
        "token": "test-token",
    }


def _make_subscribe_spec(instrument: str, channel_type: str, interval: str | None = None):
    channel: dict = {"type": channel_type}
    if interval:
        channel["interval"] = interval
    return {
        "stream_key": {
            "instrument_code": instrument,
            "channel": channel,
        },
        "instrument_exch": instrument,
        "venue": "oanda",
    }


class TestSubscribeUnsubscribe:
    @pytest.mark.asyncio
    async def test_subscribe_tick(self):
        adaptor = OandaRtmdAdaptor(_make_config())
        adaptor._pricing_stream = MagicMock()
        adaptor._pricing_stream.instruments = set()
        adaptor._pricing_stream.update_instruments = MagicMock()

        spec = _make_subscribe_spec("EUR_USD", "tick")
        await adaptor.subscribe(spec)

        assert ("EUR_USD", "tick") in adaptor._active_subs
        adaptor._pricing_stream.update_instruments.assert_called_once()
        active = await adaptor.snapshot_active()
        assert len(active) == 1

    @pytest.mark.asyncio
    async def test_unsubscribe_tick(self):
        adaptor = OandaRtmdAdaptor(_make_config())
        adaptor._pricing_stream = MagicMock()
        adaptor._pricing_stream.instruments = set()
        adaptor._pricing_stream.update_instruments = MagicMock()

        spec = _make_subscribe_spec("EUR_USD", "tick")
        await adaptor.subscribe(spec)
        await adaptor.unsubscribe(spec)

        assert ("EUR_USD", "tick") not in adaptor._active_subs
        active = await adaptor.snapshot_active()
        assert len(active) == 0

    @pytest.mark.asyncio
    async def test_subscribe_kline_creates_task(self):
        adaptor = OandaRtmdAdaptor(_make_config())
        adaptor._client = AsyncMock()

        spec = _make_subscribe_spec("EUR_USD", "kline", interval="1m")
        await adaptor.subscribe(spec)

        assert ("EUR_USD", "kline") in adaptor._active_subs
        assert ("EUR_USD", "M1") in adaptor._kline_tasks
        task = adaptor._kline_tasks[("EUR_USD", "M1")]
        task.cancel()
        try:
            await task
        except asyncio.CancelledError:
            pass

    @pytest.mark.asyncio
    async def test_unsubscribe_kline_cancels_task(self):
        adaptor = OandaRtmdAdaptor(_make_config())
        adaptor._client = AsyncMock()

        spec = _make_subscribe_spec("EUR_USD", "kline", interval="1m")
        await adaptor.subscribe(spec)
        assert ("EUR_USD", "M1") in adaptor._kline_tasks

        await adaptor.unsubscribe(spec)
        assert ("EUR_USD", "M1") not in adaptor._kline_tasks

    @pytest.mark.asyncio
    async def test_duplicate_subscribe_noop(self):
        adaptor = OandaRtmdAdaptor(_make_config())
        adaptor._pricing_stream = MagicMock()
        adaptor._pricing_stream.instruments = set()
        adaptor._pricing_stream.update_instruments = MagicMock()

        spec = _make_subscribe_spec("EUR_USD", "tick")
        await adaptor.subscribe(spec)
        await adaptor.subscribe(spec)

        # Should only call update_instruments once
        assert adaptor._pricing_stream.update_instruments.call_count == 1


class TestInstrumentMapping:
    def test_instrument_exch_for_mapped(self):
        adaptor = OandaRtmdAdaptor(_make_config())
        adaptor._instrument_map["EUR_USD"] = "EUR_USD"
        assert adaptor.instrument_exch_for("EUR_USD") == "EUR_USD"

    def test_instrument_exch_for_unmapped(self):
        adaptor = OandaRtmdAdaptor(_make_config())
        assert adaptor.instrument_exch_for("UNKNOWN") is None


class TestNextEvent:
    @pytest.mark.asyncio
    async def test_next_event_returns_queued_event(self):
        adaptor = OandaRtmdAdaptor(_make_config())
        event = {"event_type": "tick", "payload_bytes": b"\x00"}
        await adaptor._event_queue.put(event)

        result = await adaptor.next_event()
        assert result == event


class TestQueryCurrentTick:
    @pytest.mark.asyncio
    async def test_query_tick_returns_proto_bytes(self):
        adaptor = OandaRtmdAdaptor(_make_config())
        adaptor._instrument_map["EUR_USD"] = "EUR_USD"
        adaptor._client = AsyncMock()
        adaptor._client.get_pricing.return_value = {
            "prices": [{
                "instrument": "EUR_USD",
                "time": "2024-01-15T12:30:00.000000000Z",
                "bids": [{"price": "1.09850", "liquidity": 1000000}],
                "asks": [{"price": "1.09870", "liquidity": 1000000}],
            }]
        }

        result = await adaptor.query_current_tick("EUR_USD")
        assert isinstance(result, bytes)

        tick = rtmd_pb.TickData()
        tick.ParseFromString(result)
        assert tick.instrument_code == "EUR_USD"
        assert len(tick.buy_price_levels) == 1

    @pytest.mark.asyncio
    async def test_query_tick_no_data_raises(self):
        adaptor = OandaRtmdAdaptor(_make_config())
        adaptor._instrument_map["EUR_USD"] = "EUR_USD"
        adaptor._client = AsyncMock()
        adaptor._client.get_pricing.return_value = {"prices": []}

        with pytest.raises(ValueError, match="No pricing data"):
            await adaptor.query_current_tick("EUR_USD")


class TestQueryKlines:
    @pytest.mark.asyncio
    async def test_query_klines_returns_list_of_bytes(self):
        adaptor = OandaRtmdAdaptor(_make_config())
        adaptor._instrument_map["EUR_USD"] = "EUR_USD"
        adaptor._client = AsyncMock()
        adaptor._client.get_candles.return_value = {
            "candles": [
                {
                    "time": "2024-01-15T12:30:00.000000000Z",
                    "mid": {"o": "1.09850", "h": "1.09870", "l": "1.09840", "c": "1.09860"},
                    "volume": 42,
                    "complete": True,
                },
                {
                    "time": "2024-01-15T12:31:00.000000000Z",
                    "mid": {"o": "1.09860", "h": "1.09880", "l": "1.09850", "c": "1.09875"},
                    "volume": 38,
                    "complete": True,
                },
                {
                    "time": "2024-01-15T12:32:00.000000000Z",
                    "mid": {"o": "1.09875", "h": "1.09890", "l": "1.09860", "c": "1.09885"},
                    "volume": 15,
                    "complete": False,  # should be excluded
                },
            ]
        }

        result = await adaptor.query_klines("EUR_USD", "1m", 10)
        assert len(result) == 2
        for raw in result:
            kline = rtmd_pb.Kline()
            kline.ParseFromString(raw)
            assert kline.symbol == "EUR_USD"


class TestUnsupported:
    @pytest.mark.asyncio
    async def test_orderbook_not_implemented(self):
        adaptor = OandaRtmdAdaptor(_make_config())
        with pytest.raises(NotImplementedError):
            await adaptor.query_current_orderbook("EUR_USD")

    @pytest.mark.asyncio
    async def test_funding_not_implemented(self):
        adaptor = OandaRtmdAdaptor(_make_config())
        with pytest.raises(NotImplementedError):
            await adaptor.query_current_funding("EUR_USD")
