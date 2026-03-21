"""Tests for IBKR refdata loader."""

from __future__ import annotations

from datetime import datetime, timezone
from unittest.mock import AsyncMock

import pytest

from ibkr.refdata import (
    IbkrRefdataConfig,
    IbkrRefdataLoader,
    _build_canonical_id,
    _compute_session_state,
    _infer_precision,
    _parse_liquid_hours,
)
from tests.conftest import MockContract, MockContractDetails, MockIB


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _make_loader(mock_ib: MockIB, **config_overrides) -> IbkrRefdataLoader:
    """Create a loader with a mocked IB connection."""
    config = {
        "host": "127.0.0.1",
        "port": 7497,
        "client_id": 10,
        "mode": "paper",
        "universe": ["AAPL-USD-STK-SMART"],
        **config_overrides,
    }
    loader = IbkrRefdataLoader(config)

    # Wire up the mock so _ensure_connected returns our mock_ib
    mock_conn = AsyncMock()
    mock_conn.ib = mock_ib
    mock_conn.state = "live"
    loader._conn = mock_conn

    async def _fake_ensure():
        return mock_conn

    loader._ensure_connected = _fake_ensure  # type: ignore[assignment]
    loader._disconnect = AsyncMock()  # type: ignore[assignment]
    return loader


# ---------------------------------------------------------------------------
# TestIbkrRefdataConfig
# ---------------------------------------------------------------------------


class TestIbkrRefdataConfig:
    def test_from_dict_valid(self) -> None:
        cfg = IbkrRefdataConfig.from_dict({
            "host": "10.0.0.1",
            "port": 4001,
            "client_id": 5,
            "mode": "live",
            "universe": ["AAPL-USD-STK-SMART", "MSFT-USD-STK-SMART"],
        })
        assert cfg.host == "10.0.0.1"
        assert cfg.port == 4001
        assert cfg.client_id == 5
        assert cfg.mode == "live"
        assert cfg.universe == ("AAPL-USD-STK-SMART", "MSFT-USD-STK-SMART")

    def test_from_dict_defaults(self) -> None:
        cfg = IbkrRefdataConfig.from_dict({})
        assert cfg.host == "127.0.0.1"
        assert cfg.port == 7497
        assert cfg.client_id == 10
        assert cfg.mode == "paper"
        assert cfg.universe == ()
        assert cfg.read_only is True
        assert cfg.reconnect_delay_s == 2.0
        assert cfg.reconnect_max_delay_s == 60.0
        assert cfg.next_valid_id_timeout_s == 10.0

    def test_from_dict_universe_list_to_tuple(self) -> None:
        cfg = IbkrRefdataConfig.from_dict({"universe": ["A", "B"]})
        assert isinstance(cfg.universe, tuple)
        assert cfg.universe == ("A", "B")

    def test_from_dict_universe_tuple_kept(self) -> None:
        cfg = IbkrRefdataConfig.from_dict({"universe": ("X", "Y")})
        assert cfg.universe == ("X", "Y")

    def test_from_dict_ignores_unknown_keys(self) -> None:
        cfg = IbkrRefdataConfig.from_dict({"host": "h", "bogus": 42})
        assert cfg.host == "h"

    def test_invalid_mode_raises(self) -> None:
        with pytest.raises(ValueError, match="mode must be one of"):
            IbkrRefdataConfig.from_dict({"mode": "simulation"})


# ---------------------------------------------------------------------------
# TestHelpers
# ---------------------------------------------------------------------------


class TestHelpers:
    # -- _build_canonical_id --------------------------------------------------

    def test_canonical_id_stk(self) -> None:
        c = MockContract(symbol="AAPL", currency="USD", secType="STK")
        assert _build_canonical_id(c) == "AAPL/USD@IBKR"

    def test_canonical_id_fut(self) -> None:
        c = MockContract(symbol="ES", currency="USD", secType="FUT")
        assert _build_canonical_id(c) == "ES-F/USD@IBKR"

    def test_canonical_id_opt(self) -> None:
        c = MockContract(symbol="AAPL", currency="USD", secType="OPT")
        assert _build_canonical_id(c) == "AAPL-OPT/USD@IBKR"

    def test_canonical_id_cfd(self) -> None:
        c = MockContract(symbol="IBDE40", currency="EUR", secType="CFD")
        assert _build_canonical_id(c) == "IBDE40-CFD/EUR@IBKR"

    def test_canonical_id_unknown_sectype(self) -> None:
        c = MockContract(symbol="X", currency="USD", secType="WAR")
        # Unknown secType -> no suffix
        assert _build_canonical_id(c) == "X/USD@IBKR"

    # -- _infer_precision -----------------------------------------------------

    @pytest.mark.parametrize(
        "min_tick, expected",
        [
            (0.01, 2),
            (0.001, 3),
            (0.0001, 4),
            (1.0, 0),
            (0.05, 2),   # -floor(log10(0.05)) = -floor(-1.3) = 2
            (0.5, 1),    # -floor(log10(0.5))  = -floor(-0.3) = 1
            (0.25, 1),   # -floor(log10(0.25)) = -floor(-0.6) = 1
        ],
    )
    def test_infer_precision(self, min_tick: float, expected: int) -> None:
        assert _infer_precision(min_tick) == expected

    def test_infer_precision_zero_fallback(self) -> None:
        assert _infer_precision(0) == 2

    def test_infer_precision_negative_fallback(self) -> None:
        assert _infer_precision(-0.01) == 2

    # -- _parse_liquid_hours --------------------------------------------------

    def test_parse_liquid_hours_normal(self) -> None:
        result = _parse_liquid_hours("20230901:0930-1600;20230904:0930-1600")
        assert result == [{"name": "regular", "start": "09:30", "end": "16:00"}]

    def test_parse_liquid_hours_empty(self) -> None:
        assert _parse_liquid_hours("") == []

    def test_parse_liquid_hours_closed_skipped(self) -> None:
        result = _parse_liquid_hours(
            "20230902:CLOSED;20230903:CLOSED;20230904:0930-1600"
        )
        assert len(result) == 1
        assert result[0]["start"] == "09:30"

    def test_parse_liquid_hours_dedup(self) -> None:
        result = _parse_liquid_hours(
            "20230901:0930-1600;20230904:0930-1600;20230905:0930-1600"
        )
        assert len(result) == 1

    def test_parse_liquid_hours_multiple_distinct(self) -> None:
        result = _parse_liquid_hours("20230901:0930-1600;20230901:1800-2200")
        assert len(result) == 2
        assert result[0] == {"name": "regular", "start": "09:30", "end": "16:00"}
        assert result[1] == {"name": "regular", "start": "18:00", "end": "22:00"}

    def test_parse_liquid_hours_malformed_segment_skipped(self) -> None:
        # No dash in time range -> skipped
        result = _parse_liquid_hours("20230901:0930;20230904:0930-1600")
        assert len(result) == 1

    def test_parse_liquid_hours_long_format(self) -> None:
        # IBKR long format: "YYYYMMDD:HHMM-YYYYMMDD:HHMM"
        result = _parse_liquid_hours(
            "20230901:0930-20230901:1600;20230904:0930-20230904:1600"
        )
        assert result == [{"name": "regular", "start": "09:30", "end": "16:00"}]

    def test_parse_liquid_hours_long_format_mixed(self) -> None:
        # Mix of long and short segments
        result = _parse_liquid_hours(
            "20230901:0930-20230901:1600;20230901:1800-2200"
        )
        assert len(result) == 2
        assert result[0] == {"name": "regular", "start": "09:30", "end": "16:00"}
        assert result[1] == {"name": "regular", "start": "18:00", "end": "22:00"}

    # -- _compute_session_state -----------------------------------------------

    def test_session_state_open(self) -> None:
        # Friday Sep 1 2023, 14:00 UTC — market is open (09:30–16:00 local)
        now = datetime(2023, 9, 1, 14, 0, tzinfo=timezone.utc)
        result = _compute_session_state("20230901:0930-1600", now)
        assert result == "open"

    def test_session_state_closed_outside_hours(self) -> None:
        # Friday Sep 1 2023, 08:00 UTC — before open
        now = datetime(2023, 9, 1, 8, 0, tzinfo=timezone.utc)
        result = _compute_session_state("20230901:0930-1600", now)
        assert result == "closed"

    def test_session_state_closed_wrong_date(self) -> None:
        # Saturday Sep 2 — no segment for this date
        now = datetime(2023, 9, 2, 14, 0, tzinfo=timezone.utc)
        result = _compute_session_state("20230901:0930-1600", now)
        assert result == "closed"

    def test_session_state_empty_string(self) -> None:
        now = datetime(2023, 9, 1, 14, 0, tzinfo=timezone.utc)
        assert _compute_session_state("", now) == "closed"

    def test_session_state_closed_segments(self) -> None:
        now = datetime(2023, 9, 2, 14, 0, tzinfo=timezone.utc)
        assert _compute_session_state("20230902:CLOSED", now) == "closed"

    def test_session_state_long_format(self) -> None:
        now = datetime(2023, 9, 1, 12, 0, tzinfo=timezone.utc)
        result = _compute_session_state(
            "20230901:0930-20230901:1600", now
        )
        assert result == "open"


# ---------------------------------------------------------------------------
# TestIbkrRefdataLoader
# ---------------------------------------------------------------------------


class TestIbkrRefdataLoader:
    async def test_load_instruments_empty_universe(self) -> None:
        loader = IbkrRefdataLoader({"universe": []})
        result = await loader.load_instruments()
        assert result == []

    async def test_load_instruments_stk(self) -> None:
        mock_ib = MockIB()
        aapl = MockContract(
            symbol="AAPL", currency="USD", secType="STK", exchange="SMART", conId=12345
        )
        mock_ib._contract_details["AAPL"] = [
            MockContractDetails(
                contract=aapl,
                minTick=0.01,
                liquidHours="20230901:0930-1600;20230904:0930-1600",
                timeZoneId="US/Eastern",
                multiplier="",
            )
        ]

        loader = _make_loader(mock_ib, universe=["AAPL-USD-STK-SMART"])
        result = await loader.load_instruments()

        assert len(result) == 1
        r = result[0]
        assert r["instrument_id"] == "AAPL/USD@IBKR"
        assert r["instrument_id_exchange"] == "AAPL"
        assert r["exchange_name"] == "IBKR"
        assert r["instrument_exch"] == "AAPL"
        assert r["venue"] == "IBKR"
        assert r["instrument_type"] == "spot"
        assert r["base_asset"] == "AAPL"
        assert r["quote_asset"] == "USD"
        assert r["price_precision"] == 2
        assert r["price_tick_size"] == 0.01
        assert r["qty_precision"] == 0
        assert r["qty_lot_size"] == 1.0
        assert r["min_order_qty"] == 1.0
        assert r["contract_size"] == 1.0
        assert r["disabled"] is False
        assert r["currency"] == "USD"
        assert r["asset_class"] == "spot"

    async def test_load_instruments_future(self) -> None:
        mock_ib = MockIB()
        es = MockContract(
            symbol="ES", currency="USD", secType="FUT", exchange="CME", conId=99999
        )
        mock_ib._contract_details["ES"] = [
            MockContractDetails(
                contract=es,
                minTick=0.25,
                liquidHours="20230901:0930-1600",
                timeZoneId="US/Central",
                multiplier="50",
            )
        ]

        loader = _make_loader(mock_ib, universe=["ES-USD-FUT-CME"])
        result = await loader.load_instruments()

        assert len(result) == 1
        r = result[0]
        assert r["instrument_id"] == "ES-F/USD@IBKR"
        assert r["instrument_type"] == "future"
        assert r["contract_size"] == 50.0
        assert r["price_precision"] == 1  # 0.25 -> 1
        assert r["price_tick_size"] == 0.25

    async def test_load_instruments_multiple(self) -> None:
        mock_ib = MockIB()
        for sym, sec, cur, exch in [
            ("AAPL", "STK", "USD", "SMART"),
            ("MSFT", "STK", "USD", "SMART"),
        ]:
            c = MockContract(symbol=sym, currency=cur, secType=sec, exchange=exch)
            mock_ib._contract_details[sym] = [
                MockContractDetails(contract=c, minTick=0.01)
            ]

        loader = _make_loader(
            mock_ib, universe=["AAPL-USD-STK-SMART", "MSFT-USD-STK-SMART"]
        )
        result = await loader.load_instruments()
        assert len(result) == 2
        ids = {r["instrument_id"] for r in result}
        assert ids == {"AAPL/USD@IBKR", "MSFT/USD@IBKR"}

    async def test_load_instruments_no_details_skipped(self) -> None:
        mock_ib = MockIB()
        # No contract details registered -> empty list from reqContractDetailsAsync
        loader = _make_loader(mock_ib, universe=["NOPE-USD-STK-SMART"])
        result = await loader.load_instruments()
        assert result == []

    async def test_load_instruments_disconnects(self) -> None:
        mock_ib = MockIB()
        mock_ib._contract_details["AAPL"] = [
            MockContractDetails(
                contract=MockContract(symbol="AAPL", currency="USD", secType="STK"),
                minTick=0.01,
            )
        ]
        loader = _make_loader(mock_ib, universe=["AAPL-USD-STK-SMART"])
        await loader.load_instruments()
        loader._disconnect.assert_awaited_once()

    # -- load_market_sessions -------------------------------------------------

    async def test_load_market_sessions_empty_universe(self) -> None:
        loader = IbkrRefdataLoader({"universe": []})
        result = await loader.load_market_sessions()
        assert result == []

    async def test_load_market_sessions(self) -> None:
        mock_ib = MockIB()
        aapl = MockContract(
            symbol="AAPL", currency="USD", secType="STK", exchange="SMART"
        )
        mock_ib._contract_details["AAPL"] = [
            MockContractDetails(
                contract=aapl,
                minTick=0.01,
                liquidHours="20230901:0930-1600;20230904:0930-1600",
                timeZoneId="US/Eastern",
            )
        ]

        loader = _make_loader(mock_ib, universe=["AAPL-USD-STK-SMART"])
        # Populate cache via load_instruments first
        await loader.load_instruments()

        # Reset disconnect mock to verify it's called again
        loader._disconnect.reset_mock()
        sessions = await loader.load_market_sessions()

        assert len(sessions) == 1
        s = sessions[0]
        assert s["venue"] == "IBKR"
        assert s["market"] == "spot"
        assert s["session_state"] in ("open", "closed")
        loader._disconnect.assert_awaited_once()

    async def test_load_market_sessions_populates_cache_if_empty(self) -> None:
        """load_market_sessions calls load_instruments when cache is empty."""
        mock_ib = MockIB()
        aapl = MockContract(
            symbol="AAPL", currency="USD", secType="STK", exchange="NYSE"
        )
        mock_ib._contract_details["AAPL"] = [
            MockContractDetails(
                contract=aapl,
                minTick=0.01,
                liquidHours="20230901:0930-1600",
                timeZoneId="US/Eastern",
            )
        ]

        loader = _make_loader(mock_ib, universe=["AAPL-USD-STK-SMART"])
        # Call load_market_sessions directly (cache is empty)
        sessions = await loader.load_market_sessions()
        assert len(sessions) == 1
        assert sessions[0]["venue"] == "IBKR"
        assert sessions[0]["market"] == "spot"
        assert sessions[0]["session_state"] in ("open", "closed")

    async def test_load_market_sessions_dedup_markets(self) -> None:
        """Multiple instruments of same secType produce one session entry."""
        mock_ib = MockIB()
        for sym in ("AAPL", "MSFT"):
            c = MockContract(
                symbol=sym, currency="USD", secType="STK", exchange="SMART"
            )
            mock_ib._contract_details[sym] = [
                MockContractDetails(
                    contract=c,
                    minTick=0.01,
                    liquidHours="20230901:0930-1600",
                    timeZoneId="US/Eastern",
                )
            ]

        loader = _make_loader(
            mock_ib, universe=["AAPL-USD-STK-SMART", "MSFT-USD-STK-SMART"]
        )
        await loader.load_instruments()
        sessions = await loader.load_market_sessions()
        assert len(sessions) == 1
        assert sessions[0]["market"] == "spot"

    async def test_load_market_sessions_multiple_sectypes(self) -> None:
        """Different secTypes produce separate session entries."""
        mock_ib = MockIB()
        aapl = MockContract(
            symbol="AAPL", currency="USD", secType="STK", exchange="SMART"
        )
        es = MockContract(
            symbol="ES", currency="USD", secType="FUT", exchange="CME"
        )
        mock_ib._contract_details["AAPL"] = [
            MockContractDetails(contract=aapl, minTick=0.01)
        ]
        mock_ib._contract_details["ES"] = [
            MockContractDetails(contract=es, minTick=0.25)
        ]

        loader = _make_loader(
            mock_ib, universe=["AAPL-USD-STK-SMART", "ES-USD-FUT-CME"]
        )
        await loader.load_instruments()
        sessions = await loader.load_market_sessions()
        assert len(sessions) == 2
        markets = {s["market"] for s in sessions}
        assert markets == {"spot", "future"}
