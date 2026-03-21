"""Live integration tests for the OANDA RTMD adaptor.

Skipped unless ZK_OANDA_TOKEN is set. Runs against OANDA practice environment.

Usage:
    source devops/scripts/load-oanda-token.sh
    uv run pytest tests/test_live_rtmd.py -v -s
"""

from __future__ import annotations

import os

import pytest

import oanda.proto  # noqa: F401
from zk.rtmd.v1 import rtmd_pb2 as rtmd_pb

from oanda.rtmd import OandaRtmdAdaptor

OANDA_TOKEN = os.environ.get("ZK_OANDA_TOKEN", "")
OANDA_ACCOUNT_ID = os.environ.get("ZK_OANDA_ACCOUNT_ID", "101-003-26138765-001")

pytestmark = pytest.mark.skipif(not OANDA_TOKEN, reason="ZK_OANDA_TOKEN not set")


@pytest.fixture
async def adaptor():
    """Create and connect a live RTMD adaptor."""
    a = OandaRtmdAdaptor({
        "environment": "practice",
        "account_id": OANDA_ACCOUNT_ID,
        "token": OANDA_TOKEN,
    })
    await a.connect()
    yield a
    await a.shutdown()


class TestLiveRtmd:
    @pytest.mark.asyncio
    async def test_query_current_tick(self, adaptor):
        """Query current tick for EUR_USD via REST."""
        adaptor._instrument_map["EUR_USD"] = "EUR_USD"
        raw = await adaptor.query_current_tick("EUR_USD")
        assert isinstance(raw, bytes) and len(raw) > 0

        tick = rtmd_pb.TickData()
        tick.ParseFromString(raw)
        assert tick.instrument_code == "EUR_USD"
        assert len(tick.buy_price_levels) >= 1
        assert len(tick.sell_price_levels) >= 1
        bid = tick.buy_price_levels[0].price
        ask = tick.sell_price_levels[0].price
        assert 0.5 < bid < 2.0  # sanity: EUR/USD range
        assert ask > bid
        print(f"\nEUR_USD tick: bid={bid:.5f} ask={ask:.5f}")

    @pytest.mark.asyncio
    async def test_query_klines(self, adaptor):
        """Query recent M1 klines for EUR_USD via REST."""
        adaptor._instrument_map["EUR_USD"] = "EUR_USD"
        result = await adaptor.query_klines("EUR_USD", "1m", 5)
        assert len(result) >= 1

        for raw in result:
            kline = rtmd_pb.Kline()
            kline.ParseFromString(raw)
            assert kline.symbol == "EUR_USD"
            assert kline.open > 0
            assert kline.high >= kline.low
            assert kline.kline_type == rtmd_pb.Kline.KLINE_1MIN

        print(f"\nGot {len(result)} completed M1 klines for EUR_USD")

    @pytest.mark.asyncio
    async def test_query_klines_h1(self, adaptor):
        """Query recent H1 klines for EUR_USD."""
        adaptor._instrument_map["EUR_USD"] = "EUR_USD"
        result = await adaptor.query_klines("EUR_USD", "1h", 3)
        assert len(result) >= 1

        kline = rtmd_pb.Kline()
        kline.ParseFromString(result[0])
        assert kline.kline_type == rtmd_pb.Kline.KLINE_1HOUR
        print(f"\nGot {len(result)} completed H1 klines for EUR_USD")
