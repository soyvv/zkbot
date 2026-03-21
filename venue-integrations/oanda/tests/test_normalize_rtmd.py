"""Tests for OANDA RTMD normalization functions."""

import oanda.proto  # noqa: F401
from zk.rtmd.v1 import rtmd_pb2 as rtmd_pb

from oanda import oanda_normalize as norm


class TestParseOandaTime:
    def test_basic(self):
        ts = norm.parse_oanda_time("2024-01-15T12:30:00.000000000Z")
        assert ts == 1705321800000

    def test_fractional_seconds(self):
        ts = norm.parse_oanda_time("2024-01-15T12:30:00.123456789Z")
        assert ts == 1705321800123

    def test_no_fractional(self):
        ts = norm.parse_oanda_time("2024-01-15T12:30:00Z")
        assert ts == 1705321800000


class TestOandaGranularity:
    def test_known_intervals(self):
        assert norm.oanda_granularity("1m") == "M1"
        assert norm.oanda_granularity("5m") == "M5"
        assert norm.oanda_granularity("15m") == "M15"
        assert norm.oanda_granularity("30m") == "M30"
        assert norm.oanda_granularity("1h") == "H1"
        assert norm.oanda_granularity("4h") == "H4"
        assert norm.oanda_granularity("1d") == "D"

    def test_unknown_passthrough(self):
        assert norm.oanda_granularity("W") == "W"


class TestKlineType:
    def test_known_intervals(self):
        assert norm.kline_type_from_interval("1m") == rtmd_pb.Kline.KLINE_1MIN
        assert norm.kline_type_from_interval("1h") == rtmd_pb.Kline.KLINE_1HOUR
        assert norm.kline_type_from_interval("1d") == rtmd_pb.Kline.KLINE_1DAY

    def test_unknown_defaults_1min(self):
        assert norm.kline_type_from_interval("unknown") == rtmd_pb.Kline.KLINE_1MIN


class TestBuildTickData:
    def test_roundtrip(self):
        price_msg = {
            "type": "PRICE",
            "instrument": "EUR_USD",
            "time": "2024-01-15T12:30:00.000000000Z",
            "bids": [{"price": "1.09850", "liquidity": 1000000}],
            "asks": [{"price": "1.09870", "liquidity": 1000000}],
        }
        raw = norm.build_tick_data(price_msg)
        tick = rtmd_pb.TickData()
        tick.ParseFromString(raw)

        assert tick.tick_type == rtmd_pb.OB
        assert tick.instrument_code == "EUR_USD"
        assert tick.exchange == "oanda"
        assert tick.original_timestamp == 1705321800000
        assert len(tick.buy_price_levels) == 1
        assert abs(tick.buy_price_levels[0].price - 1.09850) < 1e-6
        assert len(tick.sell_price_levels) == 1
        assert abs(tick.sell_price_levels[0].price - 1.09870) < 1e-6

    def test_multiple_levels(self):
        price_msg = {
            "instrument": "GBP_USD",
            "time": "2024-01-15T12:30:00.000000000Z",
            "bids": [
                {"price": "1.27000", "liquidity": 500000},
                {"price": "1.26990", "liquidity": 1000000},
            ],
            "asks": [
                {"price": "1.27010", "liquidity": 500000},
            ],
        }
        raw = norm.build_tick_data(price_msg)
        tick = rtmd_pb.TickData()
        tick.ParseFromString(raw)
        assert len(tick.buy_price_levels) == 2
        assert len(tick.sell_price_levels) == 1


class TestBuildKline:
    def test_roundtrip(self):
        candle = {
            "time": "2024-01-15T12:30:00.000000000Z",
            "mid": {"o": "1.09850", "h": "1.09870", "l": "1.09840", "c": "1.09860"},
            "volume": 42,
            "complete": True,
        }
        raw = norm.build_kline(candle, "EUR_USD", "M1")
        kline = rtmd_pb.Kline()
        kline.ParseFromString(raw)

        assert kline.timestamp == 1705321800000
        assert abs(kline.open - 1.09850) < 1e-6
        assert abs(kline.high - 1.09870) < 1e-6
        assert abs(kline.low - 1.09840) < 1e-6
        assert abs(kline.close - 1.09860) < 1e-6
        assert kline.volume == 42
        assert kline.kline_type == rtmd_pb.Kline.KLINE_1MIN
        assert kline.symbol == "EUR_USD"
        assert kline.source == "oanda"

    def test_different_granularities(self):
        candle = {
            "time": "2024-01-15T12:00:00.000000000Z",
            "mid": {"o": "1.0", "h": "1.1", "l": "0.9", "c": "1.05"},
            "volume": 100,
        }
        raw_h1 = norm.build_kline(candle, "EUR_USD", "H1")
        kline_h1 = rtmd_pb.Kline()
        kline_h1.ParseFromString(raw_h1)
        assert kline_h1.kline_type == rtmd_pb.Kline.KLINE_1HOUR

        raw_d = norm.build_kline(candle, "EUR_USD", "D")
        kline_d = rtmd_pb.Kline()
        kline_d.ParseFromString(raw_d)
        assert kline_d.kline_type == rtmd_pb.Kline.KLINE_1DAY
