"""Tests for the OANDA refdata loader."""

from datetime import datetime, timezone
from unittest.mock import AsyncMock, patch

import pytest

from oanda.refdata import OandaRefdataLoader


def _make_config():
    return {
        "environment": "practice",
        "account_id": "101-001-1234567-001",
        "token": "test-token",
    }


def _make_instrument_response():
    return {
        "instruments": [
            {
                "name": "EUR_USD",
                "type": "CURRENCY",
                "displayName": "EUR/USD",
                "pipLocation": -4,
                "displayPrecision": 5,
                "tradeUnitsPrecision": 0,
                "minimumTradeSize": "1",
                "maximumOrderUnits": "100000000",
                "marginRate": "0.05",
            },
            {
                "name": "GBP_USD",
                "type": "CURRENCY",
                "displayName": "GBP/USD",
                "pipLocation": -4,
                "displayPrecision": 5,
                "tradeUnitsPrecision": 0,
                "minimumTradeSize": "1",
                "maximumOrderUnits": "50000000",
                "marginRate": "0.05",
            },
            {
                "name": "USB10Y_USD",
                "type": "CFD",
                "displayName": "US 10Y T-Note",
                "pipLocation": -2,
                "displayPrecision": 3,
                "tradeUnitsPrecision": 0,
                "minimumTradeSize": "1",
                "maximumOrderUnits": "60000",
                "marginRate": "0.05",
            },
        ]
    }


class TestLoadInstruments:
    @pytest.mark.asyncio
    async def test_loads_instruments(self):
        loader = OandaRefdataLoader(_make_config())
        mock_client = AsyncMock()
        mock_client.get_instruments.return_value = _make_instrument_response()
        mock_client.close = AsyncMock()

        with patch("oanda.refdata.OandaRestClient", return_value=mock_client):
            records = await loader.load_instruments()

        assert len(records) == 3

    @pytest.mark.asyncio
    async def test_instrument_id_format(self):
        loader = OandaRefdataLoader(_make_config())
        mock_client = AsyncMock()
        mock_client.get_instruments.return_value = _make_instrument_response()
        mock_client.close = AsyncMock()

        with patch("oanda.refdata.OandaRestClient", return_value=mock_client):
            records = await loader.load_instruments()

        eur = next(r for r in records if r["instrument_exch"] == "EUR_USD")
        assert eur["instrument_id"] == "EUR-CFD/USD@OANDA"
        assert eur["venue"] == "OANDA"
        assert eur["instrument_type"] == "CFD"
        assert eur["base_asset"] == "EUR"
        assert eur["quote_asset"] == "USD"

    @pytest.mark.asyncio
    async def test_precision_from_pip_location(self):
        loader = OandaRefdataLoader(_make_config())
        mock_client = AsyncMock()
        mock_client.get_instruments.return_value = _make_instrument_response()
        mock_client.close = AsyncMock()

        with patch("oanda.refdata.OandaRestClient", return_value=mock_client):
            records = await loader.load_instruments()

        eur = next(r for r in records if r["instrument_exch"] == "EUR_USD")
        assert eur["price_precision"] == 4
        assert abs(eur["price_tick_size"] - 0.0001) < 1e-10

        bond = next(r for r in records if r["instrument_exch"] == "USB10Y_USD")
        assert bond["price_precision"] == 2
        assert abs(bond["price_tick_size"] - 0.01) < 1e-10

    @pytest.mark.asyncio
    async def test_qty_fields(self):
        loader = OandaRefdataLoader(_make_config())
        mock_client = AsyncMock()
        mock_client.get_instruments.return_value = _make_instrument_response()
        mock_client.close = AsyncMock()

        with patch("oanda.refdata.OandaRestClient", return_value=mock_client):
            records = await loader.load_instruments()

        eur = next(r for r in records if r["instrument_exch"] == "EUR_USD")
        assert eur["qty_precision"] == 0
        assert eur["qty_lot_size"] == 1.0
        assert eur["min_order_qty"] == 1.0
        assert eur["max_order_qty"] == 100000000.0

    @pytest.mark.asyncio
    async def test_extra_properties(self):
        loader = OandaRefdataLoader(_make_config())
        mock_client = AsyncMock()
        mock_client.get_instruments.return_value = _make_instrument_response()
        mock_client.close = AsyncMock()

        with patch("oanda.refdata.OandaRestClient", return_value=mock_client):
            records = await loader.load_instruments()

        eur = next(r for r in records if r["instrument_exch"] == "EUR_USD")
        extra = eur["extra_properties"]
        assert extra["oanda_type"] == "CURRENCY"
        assert extra["margin_rate"] == "0.05"

    @pytest.mark.asyncio
    async def test_required_dict_keys(self):
        """All required refdata keys are present."""
        loader = OandaRefdataLoader(_make_config())
        mock_client = AsyncMock()
        mock_client.get_instruments.return_value = _make_instrument_response()
        mock_client.close = AsyncMock()

        required_keys = {
            "instrument_id", "instrument_exch", "venue", "instrument_type",
            "base_asset", "quote_asset", "settlement_asset", "contract_size",
            "price_precision", "qty_precision", "price_tick_size", "qty_lot_size",
            "min_notional", "max_notional", "min_order_qty", "max_order_qty",
            "max_mkt_order_qty", "extra_properties", "disabled",
        }

        with patch("oanda.refdata.OandaRestClient", return_value=mock_client):
            records = await loader.load_instruments()

        for r in records:
            assert required_keys.issubset(r.keys()), f"Missing keys: {required_keys - r.keys()}"


class TestLoadMarketSessions:
    @pytest.mark.asyncio
    async def test_returns_fx_and_cfd(self):
        loader = OandaRefdataLoader(_make_config())
        sessions = await loader.load_market_sessions()
        assert len(sessions) == 2
        markets = {s["market"] for s in sessions}
        assert markets == {"FX", "CFD"}
        for s in sessions:
            assert s["venue"] == "OANDA"
            assert s["session_state"] in ("open", "closed")

    @pytest.mark.asyncio
    async def test_saturday_closed(self):
        loader = OandaRefdataLoader(_make_config())
        # Saturday 12:00 UTC
        with patch("oanda.refdata.datetime") as mock_dt:
            mock_now = datetime(2024, 1, 13, 12, 0, tzinfo=timezone.utc)  # Saturday
            mock_dt.now.return_value = mock_now
            mock_dt.side_effect = lambda *a, **kw: datetime(*a, **kw)
            sessions = await loader.load_market_sessions()

        assert all(s["session_state"] == "closed" for s in sessions)

    @pytest.mark.asyncio
    async def test_wednesday_open(self):
        loader = OandaRefdataLoader(_make_config())
        # Wednesday 12:00 UTC
        with patch("oanda.refdata.datetime") as mock_dt:
            mock_now = datetime(2024, 1, 10, 12, 0, tzinfo=timezone.utc)  # Wednesday
            mock_dt.now.return_value = mock_now
            mock_dt.side_effect = lambda *a, **kw: datetime(*a, **kw)
            sessions = await loader.load_market_sessions()

        assert all(s["session_state"] == "open" for s in sessions)
