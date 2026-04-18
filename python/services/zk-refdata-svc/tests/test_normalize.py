"""Tests for instrument normalization and precision utilities."""

import pytest

from zk_refdata_svc.normalize.instruments import (
    float_precision,
    int_precision,
    is_close,
    normalize_instrument,
)


class TestNormalizeInstrument:
    def test_defaults_contract_size(self):
        rec = normalize_instrument({"instrument_id": "X"})
        assert rec["contract_size"] == 1.0

    def test_defaults_numeric_fields_to_zero(self):
        rec = normalize_instrument({"instrument_id": "X"})
        assert rec["min_notional"] == 0.0
        assert rec["max_notional"] == 0.0
        assert rec["min_order_qty"] == 0.0
        assert rec["max_order_qty"] == 0.0
        assert rec["max_mkt_order_qty"] == 0.0

    def test_coerces_precision_to_int(self):
        rec = normalize_instrument({
            "instrument_id": "X",
            "price_precision": "8",
            "qty_precision": 5.0,
        })
        assert rec["price_precision"] == 8
        assert rec["qty_precision"] == 5
        assert isinstance(rec["price_precision"], int)

    def test_stringifies_extra_properties(self):
        rec = normalize_instrument({
            "instrument_id": "X",
            "extra_properties": {"max_leverage": 100, "margin_type": "cross"},
        })
        assert rec["extra_properties"]["max_leverage"] == "100"
        assert rec["extra_properties"]["margin_type"] == "cross"

    def test_empty_extra_properties(self):
        rec = normalize_instrument({"instrument_id": "X"})
        assert rec["extra_properties"] == {}

    def test_sets_disabled_default(self):
        rec = normalize_instrument({"instrument_id": "X"})
        assert rec["disabled"] is False


class TestIntPrecision:
    def test_tick_0_01(self):
        assert int_precision(0.01) == 2

    def test_tick_0_001(self):
        assert int_precision(0.001) == 3

    def test_tick_1(self):
        assert int_precision(1.0) == 0

    def test_tick_0_returns_0(self):
        assert int_precision(0) == 0

    def test_tick_negative_returns_0(self):
        assert int_precision(-0.01) == 0


class TestFloatPrecision:
    def test_precision_2(self):
        assert abs(float_precision(2) - 0.01) < 1e-12

    def test_precision_0(self):
        assert float_precision(0) == 1.0

    def test_precision_negative(self):
        assert float_precision(-1) == 1.0


class TestIsClose:
    def test_equal(self):
        assert is_close(1.0, 1.0)

    def test_close_within_tolerance(self):
        assert is_close(1.0, 1.0 + 1e-14)

    def test_not_close(self):
        assert not is_close(1.0, 1.01)
