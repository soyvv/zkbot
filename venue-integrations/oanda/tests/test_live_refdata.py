"""Live integration tests for the OANDA refdata loader.

Skipped unless ZK_OANDA_TOKEN is set. Runs against OANDA practice environment.

Usage:
    source devops/scripts/load-oanda-token.sh
    uv run pytest tests/test_live_refdata.py -v -s
"""

from __future__ import annotations

import os

import pytest

from oanda.refdata import OandaRefdataLoader

OANDA_TOKEN = os.environ.get("ZK_OANDA_TOKEN", "")
OANDA_ACCOUNT_ID = os.environ.get("ZK_OANDA_ACCOUNT_ID", "101-003-26138765-001")

pytestmark = pytest.mark.skipif(not OANDA_TOKEN, reason="ZK_OANDA_TOKEN not set")


def _make_config():
    return {
        "environment": "practice",
        "account_id": OANDA_ACCOUNT_ID,
        "token": OANDA_TOKEN,
    }


class TestLiveRefdata:
    @pytest.mark.asyncio
    async def test_load_instruments(self):
        """Load instruments from OANDA practice environment."""
        loader = OandaRefdataLoader(_make_config())
        records = await loader.load_instruments()

        assert len(records) > 50
        instruments_by_exch = {r["instrument_exch"]: r for r in records}
        assert "EUR_USD" in instruments_by_exch

        eur = instruments_by_exch["EUR_USD"]
        assert eur["instrument_id"] == "EUR-CFD/USD@OANDA"
        assert eur["venue"] == "OANDA"
        assert eur["instrument_type"] == "CFD"
        assert eur["base_asset"] == "EUR"
        assert eur["quote_asset"] == "USD"
        assert eur["price_precision"] > 0
        assert eur["min_order_qty"] > 0
        print(f"\nLoaded {len(records)} instruments. EUR_USD: {eur}")

    @pytest.mark.asyncio
    async def test_load_market_sessions(self):
        """Load market sessions (always returns FX + CFD)."""
        loader = OandaRefdataLoader(_make_config())
        sessions = await loader.load_market_sessions()

        assert len(sessions) == 2
        markets = {s["market"] for s in sessions}
        assert markets == {"FX", "CFD"}
        for s in sessions:
            assert s["venue"] == "OANDA"
            assert s["session_state"] in ("open", "closed")
        print(f"\nMarket sessions: {sessions}")
