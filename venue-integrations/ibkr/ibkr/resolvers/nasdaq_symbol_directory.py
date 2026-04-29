"""Nasdaq Trader Symbol Directory resolver.

Reads the two official pipe-delimited files published at
https://nasdaqtrader.com/dynamic/SymDir/ :

- ``nasdaqlisted.txt`` — Nasdaq-listed stocks and ETFs
- ``otherlisted.txt``  — NYSE/AMEX/ARCA/CBOE/IEX stocks and ETFs

HTTPS fetch is tried first; successful content is written to a local cache
directory. If fetch fails, cached copies are used. If fetch fails AND no cache
copy exists, :class:`UniverseFetchError` is raised so the refdata refresh run is
marked failed (leaving DB state intact).
"""

from __future__ import annotations

import asyncio
import logging
import os
import ssl
import urllib.error
import urllib.request
import uuid
from pathlib import Path

from ibkr.resolvers.base import (
    INST_TYPE_ETF,
    INST_TYPE_STOCK,
    ResolvedInstrument,
    UniverseFetchError,
)

log = logging.getLogger(__name__)

_DEFAULT_NASDAQLISTED_URL = "https://nasdaqtrader.com/dynamic/SymDir/nasdaqlisted.txt"
_DEFAULT_OTHERLISTED_URL = "https://nasdaqtrader.com/dynamic/SymDir/otherlisted.txt"
_DEFAULT_CACHE_DIR = "~/.cache/zkbot/ibkr-universe"

# nasdaqlisted.txt columns (pipe-delimited, 1-based for readability):
# 1 Symbol | 2 Security Name | 3 Market Category | 4 Test Issue
# 5 Financial Status | 6 Round Lot Size | 7 ETF | 8 NextShares
_NASDAQLISTED_COLS = 8
_NASDAQ_IDX = {
    "symbol": 0,
    "name": 1,
    "test_issue": 3,
    "etf": 6,
}

# otherlisted.txt columns:
# 1 ACT Symbol | 2 Security Name | 3 Exchange | 4 CQS Symbol
# 5 ETF | 6 Round Lot Size | 7 Test Issue | 8 NASDAQ Symbol
_OTHERLISTED_COLS = 8
_OTHER_IDX = {
    "symbol": 0,
    "name": 1,
    "exchange": 2,
    "etf": 4,
    "test_issue": 6,
}

# Substrings in Security Name that mark non-common-stock listings IBKR rejects
# under STK/SMART (warrants, rights, units, when-issued tickers).
_DERIVATIVE_NAME_TOKENS = (
    "warrant",
    "warrants",
    "rights",
    "right ",
    " right",
    "units",
    "unit ",
    " unit",
    "when issued",
    "when-issued",
)


class NasdaqSymbolDirectoryResolver:
    def __init__(self, config: dict | None = None) -> None:
        cfg = config or {}
        self._nasdaqlisted_url: str = cfg.get("nasdaqlisted_url", _DEFAULT_NASDAQLISTED_URL)
        self._otherlisted_url: str = cfg.get("otherlisted_url", _DEFAULT_OTHERLISTED_URL)
        self._cache_dir: Path = Path(
            os.path.expanduser(cfg.get("cache_dir", _DEFAULT_CACHE_DIR))
        )
        self._http_timeout_s: float = float(cfg.get("http_timeout_s", 30.0))
        self._include_etfs: bool = bool(cfg.get("include_etfs", True))
        self._include_stocks: bool = bool(cfg.get("include_stocks", True))
        self._exclude_test_issues: bool = bool(cfg.get("exclude_test_issues", True))
        self._exclude_derivatives: bool = bool(cfg.get("exclude_derivatives", True))
        self._exchanges: frozenset[str] | None = (
            frozenset(cfg["exchanges"]) if cfg.get("exchanges") else None
        )
        self._currency: str = cfg.get("currency", "USD")
        self._routing: str = cfg.get("routing", "SMART")
        self._max_candidates: int | None = cfg.get("max_candidates", 5000)
        self._class_share_normalization: str = cfg.get("class_share_normalization", "space")

        if self._class_share_normalization not in {"space", "dot"}:
            raise ValueError(
                f"class_share_normalization must be 'space' or 'dot', "
                f"got {self._class_share_normalization!r}"
            )
        if not (self._include_etfs or self._include_stocks):
            raise ValueError("at least one of include_etfs / include_stocks must be true")

    # -- public entrypoint ----------------------------------------------------

    async def resolve(self) -> list[ResolvedInstrument]:
        content_nasdaq, content_other, used_cache = await self._load_sources()
        specs = self._parse_and_normalize(content_nasdaq, content_other)

        # A successful fetch with zero emissions is a legitimate (but loud)
        # outcome — maybe filters are too strict. A cache-fallback path with
        # zero emissions is suspicious: a corrupt on-disk cache would silently
        # look like "all current DB rows should be disabled". Treat as failure
        # so the refresh run is marked failed and DB state is preserved.
        if not specs and used_cache:
            raise UniverseFetchError(
                f"fetch failed and cache at {self._cache_dir} produced 0 candidates "
                f"(cache likely corrupt)"
            )
        if not specs:
            log.warning(
                "nasdaq_symbol_directory: 0 candidates after parse/filter — "
                "check include_etfs/include_stocks/exchanges filters"
            )
        if self._max_candidates is not None and len(specs) > self._max_candidates:
            log.warning(
                "nasdaq_symbol_directory: truncating %d candidates to max_candidates=%d",
                len(specs),
                self._max_candidates,
            )
            specs = specs[: self._max_candidates]
        log.info("nasdaq_symbol_directory: emitted %d candidate specs", len(specs))
        return specs

    # -- fetch / cache --------------------------------------------------------

    async def _load_sources(self) -> tuple[str, str, bool]:
        """Return (nasdaq_content, other_content, used_cache).

        Fetches both files over HTTPS; on failure falls back to the on-disk
        cache. Raises :class:`UniverseFetchError` if fetch fails and no cache
        copy exists.
        """
        try:
            nasdaq = await self._http_fetch(self._nasdaqlisted_url)
            other = await self._http_fetch(self._otherlisted_url)
        except Exception as e:  # fetch error (network, HTTP 4xx/5xx, timeout)
            log.warning(
                "nasdaq_symbol_directory: fetch failed (%s); falling back to cache at %s",
                e,
                self._cache_dir,
            )
            nasdaq_cached = self._read_cache("nasdaqlisted.txt")
            other_cached = self._read_cache("otherlisted.txt")
            if nasdaq_cached is None or other_cached is None:
                raise UniverseFetchError(
                    f"fetch failed ({e}) and no cache copy available in {self._cache_dir}"
                ) from e
            return nasdaq_cached, other_cached, True

        # Both fetches succeeded — refresh cache.
        self._write_cache("nasdaqlisted.txt", nasdaq)
        self._write_cache("otherlisted.txt", other)
        return nasdaq, other, False

    async def _http_fetch(self, url: str) -> str:
        """Blocking fetch executed in a thread so asyncio remains responsive."""
        timeout = self._http_timeout_s

        def _blocking() -> str:
            ctx = ssl.create_default_context()
            req = urllib.request.Request(
                url, headers={"User-Agent": "zkbot-ibkr-refdata/1.0"}
            )
            with urllib.request.urlopen(req, timeout=timeout, context=ctx) as resp:
                return resp.read().decode("utf-8", errors="replace")

        try:
            return await asyncio.to_thread(_blocking)
        except urllib.error.URLError as e:
            raise RuntimeError(f"fetch {url} failed: {e}") from e

    def _read_cache(self, filename: str) -> str | None:
        path = self._cache_dir / filename
        if not path.exists():
            return None
        try:
            return path.read_text(encoding="utf-8")
        except OSError as e:
            log.warning("nasdaq_symbol_directory: cache read failed for %s: %s", path, e)
            return None

    def _write_cache(self, filename: str, content: str) -> None:
        self._cache_dir.mkdir(parents=True, exist_ok=True)
        path = self._cache_dir / filename
        # Per-call unique tmp suffix so concurrent resolvers don't race on a
        # shared *.tmp path.
        tmp = path.with_suffix(f"{path.suffix}.{uuid.uuid4().hex}.tmp")
        try:
            tmp.write_text(content, encoding="utf-8")
            os.replace(tmp, path)
        except OSError as e:
            log.warning("nasdaq_symbol_directory: cache write failed for %s: %s", path, e)
            if tmp.exists():
                try:
                    tmp.unlink()
                except OSError:
                    pass

    # -- parse / filter / normalize ------------------------------------------

    def _parse_and_normalize(
        self, nasdaq_content: str, other_content: str
    ) -> list[ResolvedInstrument]:
        seen: set[tuple[str, str]] = set()  # (symbol_normalized, routing) dedup key
        specs: list[ResolvedInstrument] = []

        for (symbol, name, is_etf, is_test, exchange) in self._iter_nasdaqlisted(nasdaq_content):
            self._emit_if_accepted(symbol, name, is_etf, is_test, exchange, seen, specs)

        for (symbol, name, is_etf, is_test, exchange) in self._iter_otherlisted(other_content):
            self._emit_if_accepted(symbol, name, is_etf, is_test, exchange, seen, specs)

        return specs

    def _iter_nasdaqlisted(self, content: str):
        for fields in _iter_rows(content, _NASDAQLISTED_COLS):
            yield (
                fields[_NASDAQ_IDX["symbol"]].strip(),
                fields[_NASDAQ_IDX["name"]].strip(),
                fields[_NASDAQ_IDX["etf"]].strip().upper() == "Y",
                fields[_NASDAQ_IDX["test_issue"]].strip().upper() == "Y",
                "NASDAQ",  # source file implies Nasdaq-listed; Market Category is sub-tier
            )

    def _iter_otherlisted(self, content: str):
        for fields in _iter_rows(content, _OTHERLISTED_COLS):
            yield (
                fields[_OTHER_IDX["symbol"]].strip(),
                fields[_OTHER_IDX["name"]].strip(),
                fields[_OTHER_IDX["etf"]].strip().upper() == "Y",
                fields[_OTHER_IDX["test_issue"]].strip().upper() == "Y",
                fields[_OTHER_IDX["exchange"]].strip().upper(),
            )

    def _emit_if_accepted(
        self,
        symbol: str,
        name: str,
        is_etf: bool,
        is_test: bool,
        exchange: str,
        seen: set[tuple[str, str]],
        out: list[ResolvedInstrument],
    ) -> None:
        if not symbol:
            return
        if is_test and self._exclude_test_issues:
            return
        if is_etf and not self._include_etfs:
            return
        if not is_etf and not self._include_stocks:
            return
        if self._exchanges is not None and exchange not in self._exchanges:
            return
        if self._exclude_derivatives and _is_derivative_name(name):
            return

        normalized = self._normalize_symbol(symbol)
        key = (normalized, self._routing)
        if key in seen:
            return
        seen.add(key)
        spec = f"{normalized}-{self._currency}-STK-{self._routing}"
        # Nasdaq symbol directory authoritatively distinguishes ETFs from
        # common-stock issues — preserve that as the proto-aligned type.
        instrument_type = INST_TYPE_ETF if is_etf else INST_TYPE_STOCK
        out.append(ResolvedInstrument(spec=spec, instrument_type=instrument_type))

    def _normalize_symbol(self, symbol: str) -> str:
        if self._class_share_normalization == "space":
            return symbol.replace(".", " ")
        return symbol


def _is_derivative_name(name: str) -> bool:
    """True if the Security Name marks the listing as a warrant, right, unit, etc.

    These don't qualify under STK/SMART, so dropping them up-front saves IBKR
    round-trips during the qualification pass.
    """
    lowered = name.lower()
    return any(token in lowered for token in _DERIVATIVE_NAME_TOKENS)


def _iter_rows(content: str, expected_cols: int):
    """Yield field lists for data rows only. Skips header, blank, and footer."""
    header_seen = False
    for line in content.splitlines():
        if not line.strip():
            continue
        if line.startswith("File Creation Time"):
            continue
        parts = line.split("|")
        if len(parts) != expected_cols:
            # malformed row — skip quietly (Nasdaq Trader occasionally emits them)
            continue
        if not header_seen:
            header_seen = True
            continue  # header row
        yield parts
