"""Tests for lifecycle diff engine."""

import pytest

from zk_refdata_svc.lifecycle.diff import DiffResult, check_duplicates, diff_instruments


def _make_rec(**overrides) -> dict:
    base = {
        "instrument_id": "BTC/USDT@EX",
        "instrument_exch": "BTCUSDT",
        "venue": "EX",
        "instrument_type": "SPOT",
        "base_asset": "BTC",
        "quote_asset": "USDT",
        "settlement_asset": None,
        "contract_size": 1.0,
        "price_tick_size": 0.01,
        "qty_lot_size": 0.001,
        "min_notional": 10.0,
        "max_notional": 1_000_000.0,
        "min_order_qty": 0.001,
        "max_order_qty": 9999.0,
        "max_mkt_order_qty": 100.0,
        "price_precision": 2,
        "qty_precision": 3,
        "disabled": False,
    }
    base.update(overrides)
    return base


class TestDiffInstruments:
    def test_added_instrument(self):
        current = {}
        fetched = {"BTC/USDT@EX": _make_rec()}
        result = diff_instruments(current, fetched)
        assert len(result.added) == 1
        assert result.added[0]["instrument_id"] == "BTC/USDT@EX"
        assert len(result.changed) == 0
        assert len(result.disabled) == 0

    def test_changed_instrument_numeric(self):
        current = {"BTC/USDT@EX": _make_rec()}
        fetched = {"BTC/USDT@EX": _make_rec(price_tick_size=0.001)}
        result = diff_instruments(current, fetched)
        assert len(result.added) == 0
        assert len(result.changed) == 1
        assert len(result.disabled) == 0

    def test_changed_instrument_string(self):
        current = {"BTC/USDT@EX": _make_rec()}
        fetched = {"BTC/USDT@EX": _make_rec(instrument_type="PERP")}
        result = diff_instruments(current, fetched)
        assert len(result.changed) == 1

    def test_changed_instrument_int(self):
        current = {"BTC/USDT@EX": _make_rec()}
        fetched = {"BTC/USDT@EX": _make_rec(price_precision=4)}
        result = diff_instruments(current, fetched)
        assert len(result.changed) == 1

    def test_disabled_instrument(self):
        current = {"BTC/USDT@EX": _make_rec()}
        fetched = {}
        result = diff_instruments(current, fetched)
        assert len(result.disabled) == 1
        assert result.disabled[0] == "BTC/USDT@EX"

    def test_already_disabled_not_re_disabled(self):
        current = {"BTC/USDT@EX": _make_rec(disabled=True)}
        fetched = {}
        result = diff_instruments(current, fetched)
        assert len(result.disabled) == 0

    def test_unchanged_instrument(self):
        rec = _make_rec()
        current = {"BTC/USDT@EX": rec.copy()}
        fetched = {"BTC/USDT@EX": rec.copy()}
        result = diff_instruments(current, fetched)
        assert len(result.added) == 0
        assert len(result.changed) == 0
        assert len(result.disabled) == 0

    def test_float_tolerance_no_change(self):
        """Tiny float differences within tolerance are not flagged."""
        current = {"BTC/USDT@EX": _make_rec(price_tick_size=0.01)}
        fetched = {"BTC/USDT@EX": _make_rec(price_tick_size=0.01 + 1e-14)}
        result = diff_instruments(current, fetched)
        assert len(result.changed) == 0

    def test_mixed_adds_changes_disables(self):
        current = {
            "A/B@EX": _make_rec(instrument_id="A/B@EX"),
            "C/D@EX": _make_rec(instrument_id="C/D@EX"),
        }
        fetched = {
            "A/B@EX": _make_rec(instrument_id="A/B@EX", price_tick_size=0.1),
            "E/F@EX": _make_rec(instrument_id="E/F@EX"),
        }
        result = diff_instruments(current, fetched)
        assert len(result.added) == 1
        assert len(result.changed) == 1
        assert len(result.disabled) == 1


    def test_extra_properties_change_detected(self):
        current = {"BTC/USDT@EX": _make_rec(extra_properties={"max_leverage": "100"})}
        fetched = {"BTC/USDT@EX": _make_rec(extra_properties={"max_leverage": "125"})}
        result = diff_instruments(current, fetched)
        assert len(result.changed) == 1

    def test_extra_properties_no_change(self):
        current = {"BTC/USDT@EX": _make_rec(extra_properties={"margin_type": "cross"})}
        fetched = {"BTC/USDT@EX": _make_rec(extra_properties={"margin_type": "cross"})}
        result = diff_instruments(current, fetched)
        assert len(result.changed) == 0


class TestCheckDuplicates:
    def test_no_duplicates(self):
        check_duplicates([_make_rec(instrument_id="A"), _make_rec(instrument_id="B")])

    def test_duplicates_raise(self):
        with pytest.raises(ValueError, match="duplicate"):
            check_duplicates([_make_rec(instrument_id="A"), _make_rec(instrument_id="A")])
