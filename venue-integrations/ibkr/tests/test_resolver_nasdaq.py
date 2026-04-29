"""Tests for the NasdaqSymbolDirectoryResolver."""

from __future__ import annotations

from pathlib import Path

import pytest

from ibkr.resolvers import (
    NasdaqSymbolDirectoryResolver,
    ResolvedInstrument,
    UniverseFetchError,
)
from ibkr.resolvers import nasdaq_symbol_directory as mod

FIXTURES = Path(__file__).parent / "fixtures"
NASDAQ_SAMPLE = (FIXTURES / "nasdaqlisted_sample.txt").read_text()
OTHER_SAMPLE = (FIXTURES / "otherlisted_sample.txt").read_text()


def _make_resolver(tmp_path: Path, **overrides) -> NasdaqSymbolDirectoryResolver:
    cfg = {
        "cache_dir": str(tmp_path),
        "max_candidates": 1000,
        **overrides,
    }
    return NasdaqSymbolDirectoryResolver(cfg)


def _patch_fetch_success(monkeypatch) -> list[str]:
    """Patch _http_fetch so it returns sample content; record URLs fetched."""
    called: list[str] = []

    async def fake_fetch(self, url: str) -> str:
        called.append(url)
        if "nasdaqlisted" in url:
            return NASDAQ_SAMPLE
        return OTHER_SAMPLE

    monkeypatch.setattr(
        NasdaqSymbolDirectoryResolver, "_http_fetch", fake_fetch, raising=True
    )
    return called


def _patch_fetch_failure(monkeypatch) -> None:
    async def fake_fetch(self, url: str) -> str:
        raise RuntimeError("simulated network failure")

    monkeypatch.setattr(
        NasdaqSymbolDirectoryResolver, "_http_fetch", fake_fetch, raising=True
    )


# ---------------------------------------------------------------------------
# Config validation
# ---------------------------------------------------------------------------


class TestConfig:
    def test_invalid_class_share_norm_raises(self) -> None:
        with pytest.raises(ValueError, match="class_share_normalization"):
            NasdaqSymbolDirectoryResolver({"class_share_normalization": "dash"})

    def test_both_filters_off_raises(self) -> None:
        with pytest.raises(ValueError, match="include_etfs"):
            NasdaqSymbolDirectoryResolver(
                {"include_etfs": False, "include_stocks": False}
            )


# ---------------------------------------------------------------------------
# Parse + filter + normalize
# ---------------------------------------------------------------------------


class TestParseAndNormalize:
    @pytest.mark.asyncio
    async def test_default_accepts_stocks_and_etfs_excludes_tests(
        self, monkeypatch, tmp_path
    ) -> None:
        _patch_fetch_success(monkeypatch)
        r = _make_resolver(tmp_path)
        result = [ri.spec for ri in await r.resolve()]

        # AAPL, MSFT, QQQ (ETF), ZYBT from nasdaq + A, SPY (ETF), BRK A, BRK B from other
        assert "AAPL-USD-STK-SMART" in result
        assert "MSFT-USD-STK-SMART" in result
        assert "QQQ-USD-STK-SMART" in result  # ETF
        assert "ZYBT-USD-STK-SMART" in result
        assert "A-USD-STK-SMART" in result
        assert "SPY-USD-STK-SMART" in result
        assert "BRK A-USD-STK-SMART" in result
        assert "BRK B-USD-STK-SMART" in result
        # Test issues excluded by default
        assert not any(spec.startswith("ZXZZT") for spec in result)
        assert not any(spec.startswith("ZXIET") for spec in result)
        # Duplicates deduped (AAPL appears twice in fixture)
        assert sum(1 for s in result if s == "AAPL-USD-STK-SMART") == 1
        # Header row and File Creation Time row ignored
        assert not any("Symbol" in s for s in result)
        assert not any("File Creation" in s for s in result)

    @pytest.mark.asyncio
    async def test_etfs_only(self, monkeypatch, tmp_path) -> None:
        _patch_fetch_success(monkeypatch)
        r = _make_resolver(tmp_path, include_etfs=True, include_stocks=False)
        result = [ri.spec for ri in await r.resolve()]
        assert set(result) == {"QQQ-USD-STK-SMART", "SPY-USD-STK-SMART"}

    @pytest.mark.asyncio
    async def test_stocks_only(self, monkeypatch, tmp_path) -> None:
        _patch_fetch_success(monkeypatch)
        r = _make_resolver(tmp_path, include_etfs=False, include_stocks=True)
        result = [ri.spec for ri in await r.resolve()]
        assert "QQQ-USD-STK-SMART" not in result
        assert "SPY-USD-STK-SMART" not in result
        assert "AAPL-USD-STK-SMART" in result

    @pytest.mark.asyncio
    async def test_include_test_issues(self, monkeypatch, tmp_path) -> None:
        _patch_fetch_success(monkeypatch)
        r = _make_resolver(tmp_path, exclude_test_issues=False)
        result = [ri.spec for ri in await r.resolve()]
        assert "ZXZZT-USD-STK-SMART" in result
        assert "ZXIET-USD-STK-SMART" in result

    @pytest.mark.asyncio
    async def test_excludes_warrants_and_rights_by_default(
        self, monkeypatch, tmp_path
    ) -> None:
        _patch_fetch_success(monkeypatch)
        r = _make_resolver(tmp_path)
        result = [ri.spec for ri in await r.resolve()]
        # ACONW (Warrant) and ABCDR (Rights) are filtered by Security Name
        assert "ACONW-USD-STK-SMART" not in result
        assert "ABCDR-USD-STK-SMART" not in result

    @pytest.mark.asyncio
    async def test_include_derivatives(self, monkeypatch, tmp_path) -> None:
        _patch_fetch_success(monkeypatch)
        r = _make_resolver(tmp_path, exclude_derivatives=False)
        result = [ri.spec for ri in await r.resolve()]
        assert "ACONW-USD-STK-SMART" in result
        assert "ABCDR-USD-STK-SMART" in result

    @pytest.mark.asyncio
    async def test_exchange_allowlist(self, monkeypatch, tmp_path) -> None:
        _patch_fetch_success(monkeypatch)
        # Only accept NYSE ('N'); otherlisted SPY is 'P', ZXIET is 'V'
        r = _make_resolver(tmp_path, exchanges=["N", "NASDAQ"])
        result = [ri.spec for ri in await r.resolve()]
        # nasdaq file always tagged 'NASDAQ' (in allowlist) -> AAPL/MSFT/QQQ/ZYBT in
        # otherlisted: 'N' rows kept; 'P' (SPY) and 'V' (ZXIET, also test) dropped
        assert "A-USD-STK-SMART" in result
        assert "BRK A-USD-STK-SMART" in result
        assert "SPY-USD-STK-SMART" not in result
        assert "AAPL-USD-STK-SMART" in result

    @pytest.mark.asyncio
    async def test_class_share_dot_kept(self, monkeypatch, tmp_path) -> None:
        _patch_fetch_success(monkeypatch)
        r = _make_resolver(tmp_path, class_share_normalization="dot")
        result = [ri.spec for ri in await r.resolve()]
        assert "BRK.A-USD-STK-SMART" in result
        assert "BRK.B-USD-STK-SMART" in result
        assert "BRK A-USD-STK-SMART" not in result

    @pytest.mark.asyncio
    async def test_max_candidates_truncates(self, monkeypatch, tmp_path, caplog) -> None:
        _patch_fetch_success(monkeypatch)
        r = _make_resolver(tmp_path, max_candidates=3)
        with caplog.at_level("WARNING"):
            result = [ri.spec for ri in await r.resolve()]
        assert len(result) == 3
        assert any("truncating" in rec.message for rec in caplog.records)

    @pytest.mark.asyncio
    async def test_currency_and_routing_override(self, monkeypatch, tmp_path) -> None:
        _patch_fetch_success(monkeypatch)
        r = _make_resolver(
            tmp_path,
            include_etfs=False,
            currency="CAD",
            routing="ARCA",
        )
        result = [ri.spec for ri in await r.resolve()]
        # Every spec should use overridden currency/routing
        assert all(s.split("-")[1] == "CAD" for s in result)
        assert all(s.split("-")[3] == "ARCA" for s in result)

    @pytest.mark.asyncio
    async def test_instrument_type_propagates(self, monkeypatch, tmp_path) -> None:
        from ibkr.resolvers.base import INST_TYPE_ETF, INST_TYPE_STOCK

        _patch_fetch_success(monkeypatch)
        r = _make_resolver(tmp_path)
        result = await r.resolve()
        by_symbol = {ri.spec.split("-", 1)[0]: ri for ri in result}
        assert isinstance(by_symbol["QQQ"], ResolvedInstrument)
        assert by_symbol["QQQ"].instrument_type == INST_TYPE_ETF   # nasdaqlisted ETF=Y
        assert by_symbol["SPY"].instrument_type == INST_TYPE_ETF   # otherlisted  ETF=Y
        assert by_symbol["AAPL"].instrument_type == INST_TYPE_STOCK
        assert by_symbol["MSFT"].instrument_type == INST_TYPE_STOCK


# ---------------------------------------------------------------------------
# Fetch + cache
# ---------------------------------------------------------------------------


class TestFetchCache:
    @pytest.mark.asyncio
    async def test_success_writes_cache_atomically(self, monkeypatch, tmp_path) -> None:
        _patch_fetch_success(monkeypatch)
        r = _make_resolver(tmp_path)
        await r.resolve()
        assert (tmp_path / "nasdaqlisted.txt").read_text() == NASDAQ_SAMPLE
        assert (tmp_path / "otherlisted.txt").read_text() == OTHER_SAMPLE
        # No leftover tmp files
        assert list(tmp_path.glob("*.tmp")) == []

    @pytest.mark.asyncio
    async def test_fetch_failure_falls_back_to_cache(
        self, monkeypatch, tmp_path
    ) -> None:
        # Pre-seed cache
        (tmp_path / "nasdaqlisted.txt").write_text(NASDAQ_SAMPLE)
        (tmp_path / "otherlisted.txt").write_text(OTHER_SAMPLE)
        _patch_fetch_failure(monkeypatch)

        r = _make_resolver(tmp_path)
        result = [ri.spec for ri in await r.resolve()]
        assert "AAPL-USD-STK-SMART" in result

    @pytest.mark.asyncio
    async def test_fetch_failure_and_no_cache_raises(
        self, monkeypatch, tmp_path
    ) -> None:
        _patch_fetch_failure(monkeypatch)
        r = _make_resolver(tmp_path)
        with pytest.raises(UniverseFetchError, match="no cache copy"):
            await r.resolve()

    @pytest.mark.asyncio
    async def test_fetch_failure_partial_cache_raises(
        self, monkeypatch, tmp_path
    ) -> None:
        # Only one of the two files cached — must still raise.
        (tmp_path / "nasdaqlisted.txt").write_text(NASDAQ_SAMPLE)
        _patch_fetch_failure(monkeypatch)
        r = _make_resolver(tmp_path)
        with pytest.raises(UniverseFetchError):
            await r.resolve()


# ---------------------------------------------------------------------------
# Malformed-row tolerance
# ---------------------------------------------------------------------------


class TestMalformed:
    @pytest.mark.asyncio
    async def test_skips_short_rows(self, monkeypatch, tmp_path) -> None:
        garbled = (
            "Symbol|Security Name|Market Category|Test Issue|Financial Status|"
            "Round Lot Size|ETF|NextShares\n"
            "AAPL|Apple Inc. - Common Stock|Q|N|N|100|N|N\n"
            "BROKEN_ROW|only|three|cols\n"
            "MSFT|Microsoft Corporation - Common Stock|Q|N|N|100|N|N\n"
            "File Creation Time: X|||||||\n"
        )

        async def fake_fetch(self, url):
            if "nasdaqlisted" in url:
                return garbled
            return OTHER_SAMPLE

        monkeypatch.setattr(
            NasdaqSymbolDirectoryResolver, "_http_fetch", fake_fetch, raising=True
        )
        r = _make_resolver(tmp_path)
        result = [ri.spec for ri in await r.resolve()]
        assert "AAPL-USD-STK-SMART" in result
        assert "MSFT-USD-STK-SMART" in result
        assert not any("BROKEN_ROW" in s for s in result)
