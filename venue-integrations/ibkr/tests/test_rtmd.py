"""Tests for the IBKR RTMD adaptor."""

from __future__ import annotations

import asyncio
from datetime import datetime, timezone

import pytest

from ibkr.rtmd import IbkrRtmdAdaptor, IbkrRtmdConfig, _compute_duration_str
from zk.rtmd.v1.rtmd_pb2 import Kline, OrderBook, TickData
from tests.conftest import MockBarData, MockContract, MockDOMLevel, MockIB, MockTicker


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _make_adaptor(mock_ib: MockIB, **config_overrides) -> IbkrRtmdAdaptor:
    """Create an adaptor with a mocked IB connection."""
    config = {
        "host": "127.0.0.1",
        "port": 7497,
        "client_id": 5,
        "mode": "paper",
        **config_overrides,
    }
    adaptor = IbkrRtmdAdaptor(config)
    # Replace the real connection's IB with our mock
    adaptor._conn._ib = mock_ib
    adaptor._conn._state = "live"
    adaptor._conn._next_order_id = 100
    adaptor._conn._handshake_event.set()
    return adaptor


def _tick_spec(instrument_code: str = "AAPL/USD@IBKR", instrument_exch: str = "AAPL") -> dict:
    return {
        "stream_key": {"instrument_code": instrument_code, "channel": {"type": "tick"}},
        "instrument_exch": instrument_exch,
        "venue": "ibkr",
    }


def _ob_spec(
    instrument_code: str = "AAPL/USD@IBKR",
    instrument_exch: str = "AAPL",
    depth: int = 5,
) -> dict:
    return {
        "stream_key": {
            "instrument_code": instrument_code,
            "channel": {"type": "order_book", "depth": depth},
        },
        "instrument_exch": instrument_exch,
        "venue": "ibkr",
    }


def _funding_spec(
    instrument_code: str = "AAPL/USD@IBKR", instrument_exch: str = "AAPL"
) -> dict:
    return {
        "stream_key": {"instrument_code": instrument_code, "channel": {"type": "funding"}},
        "instrument_exch": instrument_exch,
        "venue": "ibkr",
    }


# ---------------------------------------------------------------------------
# Config tests
# ---------------------------------------------------------------------------

class TestIbkrRtmdConfig:

    def test_from_dict_valid(self):
        cfg = IbkrRtmdConfig.from_dict({
            "host": "10.0.0.1",
            "port": 4002,
            "client_id": 10,
            "mode": "live",
        })
        assert cfg.host == "10.0.0.1"
        assert cfg.port == 4002
        assert cfg.client_id == 10
        assert cfg.mode == "live"

    def test_from_dict_defaults(self):
        cfg = IbkrRtmdConfig.from_dict({
            "host": "127.0.0.1",
            "port": 7497,
            "client_id": 0,
            "mode": "paper",
        })
        assert cfg.market_data_type == 1
        assert cfg.read_only is True
        assert cfg.max_msg_rate == 40.0

    def test_from_dict_ignores_unknown_keys(self):
        cfg = IbkrRtmdConfig.from_dict({
            "host": "127.0.0.1",
            "port": 7497,
            "client_id": 0,
            "mode": "paper",
            "bogus_key": "should_be_ignored",
        })
        assert cfg.host == "127.0.0.1"
        assert not hasattr(cfg, "bogus_key")

    def test_invalid_mode(self):
        with pytest.raises(ValueError, match="mode must be one of"):
            IbkrRtmdConfig.from_dict({
                "host": "127.0.0.1",
                "port": 7497,
                "client_id": 0,
                "mode": "sandbox",
            })

    def test_invalid_port(self):
        with pytest.raises(ValueError, match="port must be >= 1"):
            IbkrRtmdConfig.from_dict({
                "host": "127.0.0.1",
                "port": 0,
                "client_id": 0,
                "mode": "paper",
            })

    def test_invalid_client_id(self):
        with pytest.raises(ValueError, match="client_id must be >= 0"):
            IbkrRtmdConfig.from_dict({
                "host": "127.0.0.1",
                "port": 7497,
                "client_id": -1,
                "mode": "paper",
            })

    def test_missing_required_field(self):
        with pytest.raises(TypeError):
            IbkrRtmdConfig.from_dict({"host": "127.0.0.1", "port": 7497})


# ---------------------------------------------------------------------------
# Adaptor tests
# ---------------------------------------------------------------------------

class TestIbkrRtmdAdaptor:

    async def test_subscribe_tick(self):
        mock_ib = MockIB()
        adaptor = _make_adaptor(mock_ib)
        await adaptor.subscribe(_tick_spec())
        assert "AAPL" in mock_ib._tickers
        assert adaptor.instrument_exch_for("AAPL/USD@IBKR") == "AAPL"

    async def test_subscribe_orderbook_gated_off(self):
        """order_book subscribe is rejected when enable_depth=false (default)."""
        mock_ib = MockIB()
        adaptor = _make_adaptor(mock_ib)
        await adaptor.subscribe(_ob_spec(depth=10))
        # reqMktDepth is NOT called and no subscription is tracked.
        assert "AAPL" not in mock_ib._depth_tickers
        assert "AAPL/USD@IBKR:order_book" not in adaptor._subscriptions

    async def test_subscribe_orderbook_enabled(self):
        """With enable_depth=true, reqMktDepth is issued and depth is tracked."""
        mock_ib = MockIB()
        adaptor = _make_adaptor(mock_ib, enable_depth=True, depth_rows=10)
        await adaptor.subscribe(_ob_spec(depth=10))
        assert "AAPL" in mock_ib._depth_tickers
        assert "AAPL/USD@IBKR:order_book" in adaptor._subscriptions
        # conId tracked for routing pendingTickersEvent -> order_book.
        assert any(cid for cid in adaptor._depth_conids)

    async def test_subscribe_funding_noop(self):
        mock_ib = MockIB()
        adaptor = _make_adaptor(mock_ib)
        await adaptor.subscribe(_funding_spec())
        # Funding is a no-op: no tickers, no depth, no subscriptions tracked
        assert "AAPL" not in mock_ib._tickers
        assert "AAPL" not in mock_ib._depth_tickers
        assert len(adaptor._subscriptions) == 0

    async def test_unsubscribe_tick(self):
        mock_ib = MockIB()
        adaptor = _make_adaptor(mock_ib)
        spec = _tick_spec()
        await adaptor.subscribe(spec)
        assert "AAPL" in mock_ib._tickers

        await adaptor.unsubscribe(spec)
        assert "AAPL" not in mock_ib._tickers
        assert len(adaptor._subscriptions) == 0
        assert "AAPL/USD@IBKR" not in adaptor._instrument_map

    async def test_unsubscribe_orderbook_enabled(self):
        """Unsubscribing order_book cancels reqMktDepth when enabled."""
        mock_ib = MockIB()
        adaptor = _make_adaptor(mock_ib, enable_depth=True)
        spec = _ob_spec()
        await adaptor.subscribe(spec)
        assert "AAPL" in mock_ib._depth_tickers

        await adaptor.unsubscribe(spec)
        assert "AAPL" not in mock_ib._depth_tickers
        assert len(adaptor._subscriptions) == 0
        assert not adaptor._depth_conids

    async def test_unsubscribe_preserves_other_subs(self):
        """Unsubscribing tick should keep instrument_map if orderbook sub remains."""
        mock_ib = MockIB()
        adaptor = _make_adaptor(mock_ib, enable_depth=True)
        tick = _tick_spec()
        ob = _ob_spec()
        await adaptor.subscribe(tick)
        await adaptor.subscribe(ob)

        await adaptor.unsubscribe(tick)
        # instrument_map should be preserved since orderbook sub still exists
        assert adaptor.instrument_exch_for("AAPL/USD@IBKR") == "AAPL"
        assert len(adaptor._subscriptions) == 1

    async def test_snapshot_active(self):
        mock_ib = MockIB()
        adaptor = _make_adaptor(mock_ib)
        spec_aapl = _tick_spec("AAPL/USD@IBKR", "AAPL")
        spec_msft = _tick_spec("MSFT/USD@IBKR", "MSFT")
        await adaptor.subscribe(spec_aapl)
        await adaptor.subscribe(spec_msft)

        active = await adaptor.snapshot_active()
        assert len(active) == 2
        codes = {s["stream_key"]["instrument_code"] for s in active}
        assert codes == {"AAPL/USD@IBKR", "MSFT/USD@IBKR"}

    async def test_instrument_exch_for(self):
        mock_ib = MockIB()
        adaptor = _make_adaptor(mock_ib)
        assert adaptor.instrument_exch_for("AAPL/USD@IBKR") is None

        await adaptor.subscribe(_tick_spec())
        assert adaptor.instrument_exch_for("AAPL/USD@IBKR") == "AAPL"

    async def test_tick_callback_enqueues_event(self):
        mock_ib = MockIB()
        adaptor = _make_adaptor(mock_ib)
        await adaptor.subscribe(_tick_spec())

        # Build a ticker with a conId matching what qualifyContractsAsync produces
        con_id = hash("AAPL") % 100000
        ticker = MockTicker(
            contract=MockContract(
                symbol="AAPL", secType="STK", exchange="SMART", currency="USD", conId=con_id,
            ),
            bid=150.0,
            bidSize=100.0,
            ask=150.05,
            askSize=200.0,
            last=150.02,
            lastSize=50.0,
            volume=1_000_000.0,
        )
        adaptor._on_pending_tickers([ticker])

        event = adaptor._event_queue.get_nowait()
        assert event["event_type"] == "tick"
        assert isinstance(event["payload_bytes"], bytes)
        assert len(event["payload_bytes"]) > 0

        tick_data = TickData()
        tick_data.ParseFromString(event["payload_bytes"])
        assert tick_data.instrument_code == "AAPL/USD@IBKR"
        assert tick_data.latest_trade_price == pytest.approx(150.02)
        assert tick_data.volume == pytest.approx(1_000_000.0)
        assert len(tick_data.buy_price_levels) == 1
        assert tick_data.buy_price_levels[0].price == pytest.approx(150.0)
        assert len(tick_data.sell_price_levels) == 1
        assert tick_data.sell_price_levels[0].price == pytest.approx(150.05)

    async def test_tick_callback_fallback_by_symbol(self):
        """Ticker without conId match falls back to symbol-based lookup."""
        mock_ib = MockIB()
        adaptor = _make_adaptor(mock_ib)
        await adaptor.subscribe(_tick_spec())

        ticker = MockTicker(
            contract=MockContract(
                symbol="AAPL", secType="STK", exchange="SMART", currency="USD", conId=0,
            ),
            last=99.0,
            lastSize=10.0,
        )
        adaptor._on_pending_tickers([ticker])

        event = adaptor._event_queue.get_nowait()
        tick_data = TickData()
        tick_data.ParseFromString(event["payload_bytes"])
        assert tick_data.instrument_code == "AAPL/USD@IBKR"
        assert tick_data.latest_trade_price == pytest.approx(99.0)

    async def test_tick_callback_unknown_ignored(self):
        """Tickers for unknown instruments should be silently ignored."""
        mock_ib = MockIB()
        adaptor = _make_adaptor(mock_ib)

        ticker = MockTicker(
            contract=MockContract(symbol="ZZZZZ", conId=999999),
            last=1.0,
        )
        adaptor._on_pending_tickers([ticker])
        assert adaptor._event_queue.empty()

    async def test_orderbook_callback_enqueues_event(self):
        """DOM ticker update should emit an order_book event and cache it."""
        mock_ib = MockIB()
        adaptor = _make_adaptor(mock_ib, enable_depth=True, depth_rows=3)
        await adaptor.subscribe(_ob_spec(depth=3))

        con_id = hash("AAPL") % 100000
        ticker = MockTicker(
            contract=MockContract(
                symbol="AAPL", secType="STK", exchange="SMART", currency="USD", conId=con_id,
            ),
            domBids=[
                MockDOMLevel(price=150.00, size=100.0),
                MockDOMLevel(price=149.99, size=200.0),
            ],
            domAsks=[
                MockDOMLevel(price=150.05, size=150.0),
                MockDOMLevel(price=150.06, size=250.0),
            ],
        )
        adaptor._on_pending_tickers([ticker])

        event = adaptor._event_queue.get_nowait()
        assert event["event_type"] == "order_book"
        assert isinstance(event["payload_bytes"], bytes)

        ob = OrderBook()
        ob.ParseFromString(event["payload_bytes"])
        assert ob.instrument_code == "AAPL/USD@IBKR"
        assert len(ob.buy_levels) == 2
        assert ob.buy_levels[0].price == pytest.approx(150.00)
        assert ob.buy_levels[0].qty == pytest.approx(100.0)
        assert len(ob.sell_levels) == 2
        assert ob.sell_levels[0].price == pytest.approx(150.05)

        # Snapshot cache populated.
        cached = await adaptor.query_current_orderbook("AAPL/USD@IBKR")
        assert len(cached) > 0

    async def test_query_current_tick(self):
        mock_ib = MockIB()
        adaptor = _make_adaptor(mock_ib)
        await adaptor.subscribe(_tick_spec())

        # No tick received yet
        assert await adaptor.query_current_tick("AAPL/USD@IBKR") == b""

        # Simulate a tick
        con_id = hash("AAPL") % 100000
        ticker = MockTicker(
            contract=MockContract(symbol="AAPL", conId=con_id),
            last=200.0,
            lastSize=5.0,
        )
        adaptor._on_pending_tickers([ticker])

        cached = await adaptor.query_current_tick("AAPL/USD@IBKR")
        assert len(cached) > 0
        tick_data = TickData()
        tick_data.ParseFromString(cached)
        assert tick_data.latest_trade_price == pytest.approx(200.0)

    async def test_query_current_funding(self):
        mock_ib = MockIB()
        adaptor = _make_adaptor(mock_ib)
        result = await adaptor.query_current_funding("AAPL/USD@IBKR")
        assert result == b""

    async def test_query_klines(self):
        mock_ib = MockIB()
        adaptor = _make_adaptor(mock_ib)
        # Pre-populate contract cache (normally done by subscribe + qualify)
        contract = MockContract(
            symbol="AAPL", secType="STK", exchange="SMART", currency="USD", conId=12345,
        )
        adaptor._contract_cache["AAPL/USD@IBKR"] = contract

        bar_date = datetime(2024, 1, 15, 10, 0, tzinfo=timezone.utc)
        mock_ib._historical_bars["AAPL"] = [
            MockBarData(
                date=bar_date, open=150.0, high=151.0, low=149.5, close=150.5, volume=10000,
            ),
        ]

        result = await adaptor.query_klines("AAPL/USD@IBKR", "1m", 100)
        assert len(result) == 1

        kline = Kline()
        kline.ParseFromString(result[0])
        assert kline.open == pytest.approx(150.0)
        assert kline.high == pytest.approx(151.0)
        assert kline.low == pytest.approx(149.5)
        assert kline.close == pytest.approx(150.5)
        assert kline.volume == pytest.approx(10000.0)
        assert kline.symbol == "AAPL/USD@IBKR"
        assert kline.source == "IBKR"
        # timestamp should be bar_date in ms
        expected_ts = int(bar_date.timestamp() * 1000)
        assert kline.timestamp == expected_ts
        assert kline.kline_end_timestamp == expected_ts + 60_000

    async def test_query_klines_no_contract(self):
        mock_ib = MockIB()
        adaptor = _make_adaptor(mock_ib)
        result = await adaptor.query_klines("UNKNOWN/USD@IBKR", "1m", 100)
        assert result == []

    async def test_query_klines_invalid_interval(self):
        mock_ib = MockIB()
        adaptor = _make_adaptor(mock_ib)
        adaptor._contract_cache["AAPL/USD@IBKR"] = MockContract(symbol="AAPL")
        result = await adaptor.query_klines("AAPL/USD@IBKR", "3m", 100)
        assert result == []

    async def test_query_klines_multiple_bars(self):
        mock_ib = MockIB()
        adaptor = _make_adaptor(mock_ib)
        contract = MockContract(symbol="AAPL", secType="STK", exchange="SMART", currency="USD")
        adaptor._contract_cache["AAPL/USD@IBKR"] = contract

        t1 = datetime(2024, 1, 15, 10, 0, tzinfo=timezone.utc)
        t2 = datetime(2024, 1, 15, 10, 1, tzinfo=timezone.utc)
        mock_ib._historical_bars["AAPL"] = [
            MockBarData(date=t1, open=100.0, high=101.0, low=99.0, close=100.5, volume=500),
            MockBarData(date=t2, open=100.5, high=102.0, low=100.0, close=101.5, volume=600),
        ]

        result = await adaptor.query_klines("AAPL/USD@IBKR", "1m", 100)
        assert len(result) == 2

    async def test_reconnect_resubscribes(self):
        mock_ib = MockIB()
        adaptor = _make_adaptor(mock_ib, enable_depth=True)

        await adaptor.subscribe(_tick_spec("AAPL/USD@IBKR", "AAPL"))
        await adaptor.subscribe(_ob_spec("MSFT/USD@IBKR", "MSFT", depth=5))
        assert "AAPL" in mock_ib._tickers
        assert "MSFT" in mock_ib._depth_tickers

        # Clear mock state to verify re-registration
        mock_ib._tickers.clear()
        mock_ib._depth_tickers.clear()

        adaptor._on_reconnect()

        # Both tick and order_book subs should be re-issued.
        assert "AAPL" in mock_ib._tickers
        assert "MSFT" in mock_ib._depth_tickers
        assert any(cid for cid in adaptor._depth_conids)

    async def test_reconnect_registers_callbacks(self):
        mock_ib = MockIB()
        adaptor = _make_adaptor(mock_ib)
        adaptor._on_reconnect()
        # Callback should be registered — verify by emitting
        con_id = hash("AAPL") % 100000
        adaptor._instrument_map["AAPL/USD@IBKR"] = "AAPL"
        adaptor._conid_to_instrument[con_id] = "AAPL/USD@IBKR"

        ticker = MockTicker(
            contract=MockContract(symbol="AAPL", conId=con_id),
            last=42.0,
            lastSize=1.0,
        )
        mock_ib.pendingTickersEvent.emit([ticker])
        assert not adaptor._event_queue.empty()


# ---------------------------------------------------------------------------
# _compute_duration_str
# ---------------------------------------------------------------------------

class TestComputeDurationStr:

    def test_sub_day(self):
        # 60 bars * 60s = 3600s
        assert _compute_duration_str("1m", 60) == "3600 S"

    def test_exactly_one_day(self):
        # 1440 bars * 60s = 86400s
        assert _compute_duration_str("1m", 1440) == "86400 S"

    def test_multi_day(self):
        # 2880 bars * 60s = 172800s = 2 days
        assert _compute_duration_str("1m", 2880) == "2 D"

    def test_daily_bars(self):
        # 30 bars * 86400s = 30 days
        assert _compute_duration_str("1d", 30) == "30 D"

    def test_year_scale(self):
        # 400 daily bars = 400 days > 365 -> 2 Y
        assert _compute_duration_str("1d", 400) == "2 Y"
