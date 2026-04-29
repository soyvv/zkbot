"""Live integration tests for the IBKR RTMD adaptor.

Skipped unless ZK_IBKR_HOST is set. Uses delayed market data (type=3) so the
harness runs even when the paper account has no live data entitlements.

Usage:
    source devops/scripts/load-ibkr-paper-env.sh
    uv run --project venue-integrations/ibkr pytest tests/test_live_rtmd.py -v -s
"""

from __future__ import annotations

import asyncio
import os
import time

import pytest

from zk.rtmd.v1 import rtmd_pb2 as rtmd_pb

from ibkr.rtmd import IbkrRtmdAdaptor


IBKR_HOST = os.environ.get("ZK_IBKR_HOST", "")
IBKR_PORT = int(os.environ.get("ZK_IBKR_PORT", "0") or 0)
IBKR_CLIENT_ID_BASE = int(os.environ.get("ZK_IBKR_CLIENT_ID", "17") or 17)
IBKR_MODE = os.environ.get("ZK_IBKR_MODE", "paper")

pytestmark = pytest.mark.skipif(
    not IBKR_HOST or IBKR_PORT <= 0,
    reason="ZK_IBKR_HOST/ZK_IBKR_PORT not set",
)

_INSTRUMENT = "AAPL-USD-STK-SMART"
_SYMBOL = "AAPL"

# Offset the RTMD client_id so it never collides with the GW session; add a
# monotonic counter so multiple tests within the same run get distinct ids.
# Under pytest-xdist, also shift by worker index * 50 so parallel workers
# can't compete for the same client-id against a shared TWS session.
_rtmd_client_id_counter = 0
_XDIST_WORKER = os.environ.get("PYTEST_XDIST_WORKER", "")
_WORKER_OFFSET = int(_XDIST_WORKER[2:]) * 50 if _XDIST_WORKER.startswith("gw") else 0


def _next_rtmd_client_id() -> int:
    global _rtmd_client_id_counter
    cid = IBKR_CLIENT_ID_BASE + 100 + _WORKER_OFFSET + _rtmd_client_id_counter
    _rtmd_client_id_counter += 1
    return cid


def _spec(channel: str) -> dict:
    return {
        "stream_key": {"instrument_code": _INSTRUMENT, "channel": {"type": channel}},
        "instrument_exch": _INSTRUMENT,
        "venue": "ibkr",
    }


@pytest.fixture
async def adaptor():
    a = IbkrRtmdAdaptor({
        "host": IBKR_HOST,
        "port": IBKR_PORT,
        "client_id": _next_rtmd_client_id(),
        "mode": IBKR_MODE,
        "market_data_type": 3,  # delayed
    })
    await a.connect()
    try:
        yield a
    finally:
        try:
            await a.disconnect()
        except Exception:
            pass


class TestLiveRtmd:
    @pytest.mark.asyncio
    async def test_tick_subscribe(self, adaptor):
        """subscribe -> receive >=1 tick within 15s -> unsubscribe."""
        await adaptor.subscribe(_spec("tick"))

        deadline = time.time() + 15.0
        received = False
        while time.time() < deadline:
            try:
                event = await asyncio.wait_for(adaptor.next_event(), timeout=2.0)
            except asyncio.TimeoutError:
                continue
            if event["event_type"] == "tick":
                tick = rtmd_pb.TickData()
                tick.ParseFromString(event["payload_bytes"])
                assert tick.instrument_code == _INSTRUMENT
                received = True
                print(
                    f"\nTick: bid={tick.buy_price_levels[0].price if tick.buy_price_levels else None} "
                    f"ask={tick.sell_price_levels[0].price if tick.sell_price_levels else None} "
                    f"last={tick.latest_trade_price}"
                )
                break
        assert received, "no tick received within 15s"

        await adaptor.unsubscribe(_spec("tick"))

    @pytest.mark.asyncio
    async def test_query_current_tick_snapshot(self, adaptor):
        """After a subscription runs for a moment the snapshot cache should populate."""
        await adaptor.subscribe(_spec("tick"))
        # Let at least one tick arrive.
        for _ in range(8):
            try:
                await asyncio.wait_for(adaptor.next_event(), timeout=2.0)
            except asyncio.TimeoutError:
                continue
            break
        raw = await adaptor.query_current_tick(_INSTRUMENT)
        await adaptor.unsubscribe(_spec("tick"))
        if not raw:
            pytest.skip("no tick cached — likely outside RTH with delayed data")
        tick = rtmd_pb.TickData()
        tick.ParseFromString(raw)
        assert tick.instrument_code == _INSTRUMENT

    @pytest.mark.asyncio
    async def test_query_klines(self, adaptor):
        """Historical 1m bars via reqHistoricalData.

        IBKR paper sessions with delayed data often return "HMDS query
        returned no data" (error 162) outside RTH. The call-path itself is
        what we validate; an empty response is skipped, not failed.
        """
        await adaptor.subscribe(_spec("tick"))
        try:
            result = await adaptor.query_klines(_INSTRUMENT, "1m", 10)
        finally:
            await adaptor.unsubscribe(_spec("tick"))
        if not result:
            pytest.skip("HMDS returned no data (delayed paper outside RTH)")
        kline = rtmd_pb.Kline()
        kline.ParseFromString(result[0])
        assert kline.symbol == _INSTRUMENT
        assert kline.kline_type == rtmd_pb.Kline.KLINE_1MIN
        assert kline.high >= kline.low
        print(f"\nGot {len(result)} 1m klines for {_INSTRUMENT}")
