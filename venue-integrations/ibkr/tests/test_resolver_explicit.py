"""Tests for the ExplicitResolver pass-through and dict-form parsing."""

from __future__ import annotations

import pytest

from ibkr.resolvers import ExplicitResolver, ResolvedInstrument
from ibkr.resolvers.base import (
    INST_TYPE_ETF,
    INST_TYPE_STOCK,
    INST_TYPE_UNSPECIFIED,
)


class TestExplicitResolverStringItems:
    @pytest.mark.asyncio
    async def test_returns_items_verbatim(self) -> None:
        r = ExplicitResolver({"items": ["AAPL-USD-STK-SMART", "MSFT-USD-STK-SMART"]})
        result = await r.resolve()
        assert result == [
            ResolvedInstrument(spec="AAPL-USD-STK-SMART", instrument_type=INST_TYPE_UNSPECIFIED),
            ResolvedInstrument(spec="MSFT-USD-STK-SMART", instrument_type=INST_TYPE_UNSPECIFIED),
        ]

    @pytest.mark.asyncio
    async def test_empty_config(self) -> None:
        r = ExplicitResolver({})
        assert await r.resolve() == []

    @pytest.mark.asyncio
    async def test_none_config(self) -> None:
        r = ExplicitResolver(None)
        assert await r.resolve() == []

    @pytest.mark.asyncio
    async def test_returns_new_list_each_call(self) -> None:
        """Mutating the result must not alter the resolver's internal state."""
        r = ExplicitResolver({"items": ["A", "B"]})
        first = await r.resolve()
        first.append(ResolvedInstrument(spec="C"))
        second = await r.resolve()
        assert second == [
            ResolvedInstrument(spec="A", instrument_type=INST_TYPE_UNSPECIFIED),
            ResolvedInstrument(spec="B", instrument_type=INST_TYPE_UNSPECIFIED),
        ]


class TestExplicitResolverDictItems:
    @pytest.mark.asyncio
    async def test_dict_with_etf_type(self) -> None:
        r = ExplicitResolver({"items": [
            {"spec": "SPY-USD-STK-SMART", "instrument_type": "ETF"},
            {"spec": "QQQ-USD-STK-SMART", "instrument_type": "ETF"},
        ]})
        result = await r.resolve()
        assert all(ri.instrument_type == INST_TYPE_ETF for ri in result)
        assert {ri.spec for ri in result} == {"SPY-USD-STK-SMART", "QQQ-USD-STK-SMART"}

    @pytest.mark.asyncio
    async def test_dict_lowercase_type(self) -> None:
        r = ExplicitResolver({"items": [{"spec": "AAPL-USD-STK-SMART", "instrument_type": "stock"}]})
        result = await r.resolve()
        assert result[0].instrument_type == INST_TYPE_STOCK

    @pytest.mark.asyncio
    async def test_dict_int_type(self) -> None:
        r = ExplicitResolver({"items": [{"spec": "ES-USD-FUT-CME", "instrument_type": 3}]})
        result = await r.resolve()
        assert result[0].instrument_type == 3  # FUTURE

    @pytest.mark.asyncio
    async def test_dict_no_type_defaults_unspecified(self) -> None:
        r = ExplicitResolver({"items": [{"spec": "AAPL-USD-STK-SMART"}]})
        result = await r.resolve()
        assert result[0].instrument_type == INST_TYPE_UNSPECIFIED

    @pytest.mark.asyncio
    async def test_mixed_string_and_dict(self) -> None:
        r = ExplicitResolver({"items": [
            "AAPL-USD-STK-SMART",
            {"spec": "SPY-USD-STK-SMART", "instrument_type": "ETF"},
        ]})
        result = await r.resolve()
        assert result[0].instrument_type == INST_TYPE_UNSPECIFIED
        assert result[1].instrument_type == INST_TYPE_ETF

    def test_dict_missing_spec_raises_at_construction(self) -> None:
        with pytest.raises(ValueError, match="missing/empty 'spec'"):
            ExplicitResolver({"items": [{"instrument_type": "ETF"}]})

    def test_dict_unknown_type_raises_at_construction(self) -> None:
        with pytest.raises(ValueError, match="unknown instrument_type"):
            ExplicitResolver({"items": [{"spec": "X-USD-STK-SMART", "instrument_type": "BOGUS"}]})

    def test_non_str_non_dict_item_raises(self) -> None:
        with pytest.raises(ValueError, match="must be str or dict"):
            ExplicitResolver({"items": [42]})  # type: ignore[list-item]
