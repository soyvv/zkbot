"""Tests for instrument_utils proto-conversion helpers."""

from __future__ import annotations

import pytest

from zk_utils.instrument_utils import (
    InstrumentType,
    InvalidInstrumentType,
    from_proto,
    normalize,
    to_proto,
)


class TestFromProto:
    def test_int_input(self):
        assert from_proto(0) == "UNSPECIFIED"
        assert from_proto(1) == "SPOT"
        assert from_proto(2) == "PERP"
        assert from_proto(3) == "FUTURE"
        assert from_proto(4) == "CFD"
        assert from_proto(5) == "OPTION"
        assert from_proto(6) == "ETF"
        assert from_proto(7) == "STOCK"

    def test_enum_member_input(self):
        assert from_proto(InstrumentType.INST_TYPE_ETF) == "ETF"
        assert from_proto(InstrumentType.INST_TYPE_STOCK) == "STOCK"

    def test_unknown_int_raises(self):
        with pytest.raises(InvalidInstrumentType, match="unknown InstrumentType int"):
            from_proto(99)


class TestToProto:
    def test_uppercase_input(self):
        assert to_proto("SPOT") == 1
        assert to_proto("ETF") == 6
        assert to_proto("STOCK") == 7

    def test_lowercase_input(self):
        assert to_proto("spot") == 1
        assert to_proto("etf") == 6
        assert to_proto("stock") == 7

    def test_mixed_case_with_whitespace(self):
        assert to_proto("  Etf  ") == 6
        assert to_proto("Stock") == 7

    def test_unknown_name_raises(self):
        with pytest.raises(InvalidInstrumentType, match="unknown InstrumentType name"):
            to_proto("FOO")

    def test_non_str_raises(self):
        with pytest.raises(InvalidInstrumentType, match="expected str"):
            to_proto(7)  # type: ignore[arg-type]


class TestNormalize:
    def test_lowercase_to_upper(self):
        assert normalize("etf") == "ETF"
        assert normalize("stock") == "STOCK"
        assert normalize("spot") == "SPOT"

    def test_already_canonical(self):
        assert normalize("ETF") == "ETF"


class TestRoundTrip:
    @pytest.mark.parametrize("name", [
        "UNSPECIFIED", "SPOT", "PERP", "FUTURE", "CFD", "OPTION", "ETF", "STOCK",
    ])
    def test_round_trip(self, name):
        assert from_proto(to_proto(name)) == name


class TestEnumCoverage:
    def test_every_proto_value_has_canonical_name(self):
        """Regression: if the proto adds INST_TYPE_FOO, this test fails until
        the helper's _INT_TO_CANONICAL is regenerated (it's currently
        derived dynamically — so this catches malformed proto names).
        """
        for member in InstrumentType:
            name = from_proto(member.value)
            assert isinstance(name, str)
            assert name == name.upper()
            assert not name.startswith("INST_TYPE_")
