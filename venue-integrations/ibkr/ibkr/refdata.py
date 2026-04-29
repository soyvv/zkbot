"""IBKR refdata loader — fetches instrument metadata from TWS/IB Gateway."""

from __future__ import annotations

import logging
import math
from dataclasses import dataclass, fields
from datetime import datetime, timezone

import ib_async

from ibkr.ibkr_client import IbkrConnection
from ibkr.ibkr_contracts import ContractTranslator
from ibkr.rate_limiter import TokenBucketRateLimiter
from ibkr.resolvers import ResolvedInstrument, build_resolver
from ibkr.types import LIVE

log = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

_VENUE = "IBKR"
# Mapping from IBKR secType to the canonical proto-aligned `instrument_type`
# string written to cfg.instrument_refdata. Mirrors the proto enum names from
# zk.common.v1.InstrumentType (UPPERCASE, no INST_TYPE_ prefix). STK is split
# per-row using the resolver-supplied proto value: `_resolve_instrument_type`
# checks the resolver hint first (ETF vs STOCK), then falls back here.
_SEC_TYPE_TO_CANONICAL = {
    "STK": "STOCK",  # default for STK; ETF override via resolver hint
    "FUT": "FUTURE",
    "OPT": "OPTION",
    "CFD": "CFD",
    "CASH": "SPOT",  # forex cash treated as spot
}
# Resolver-hint proto int → canonical name. Only the values the IBKR loader
# can produce (ETF/STOCK from Nasdaq directory) need entries here; UNSPECIFIED
# means "no hint, derive from secType".
_PROTO_INT_TO_CANONICAL = {
    1: "SPOT", 2: "PERP", 3: "FUTURE", 4: "CFD",
    5: "OPTION", 6: "ETF", 7: "STOCK",
}
# Session-state tagging — stocks and ETFs share the same trading-hours regime,
# so both STK variants collapse to a single market key. Distinct from
# instrument_type (proto-aligned); session market is just a regime tag.
_SESSION_MARKET_MAP = {
    "STK": "EQUITY",
    "FUT": "FUTURE",
    "OPT": "OPTION",
    "CFD": "CFD",
    "CASH": "FOREX",
}
_SEC_TYPE_SUFFIX = {"STK": "", "FUT": "-F", "OPT": "-OPT", "CFD": "-CFD", "CASH": ""}
_VALID_MODES = frozenset({"paper", "live"})


# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class IbkrRefdataConfig:
    """Configuration for the IBKR refdata loader."""

    host: str = "127.0.0.1"
    port: int = 7497
    client_id: int = 10
    mode: str = "paper"
    universe: tuple[str, ...] = ()
    universe_sources: tuple[dict, ...] = ()
    read_only: bool = True
    reconnect_delay_s: float = 2.0
    reconnect_max_delay_s: float = 60.0
    next_valid_id_timeout_s: float = 10.0
    max_qualifications_per_run: int | None = None
    rate_limit_per_sec: float = 40.0
    dry_run_candidates_only: bool = False

    def __post_init__(self) -> None:
        if self.mode not in _VALID_MODES:
            raise ValueError(f"mode must be one of {_VALID_MODES}, got {self.mode!r}")
        if self.rate_limit_per_sec <= 0:
            raise ValueError(
                f"rate_limit_per_sec must be > 0, got {self.rate_limit_per_sec}"
            )

    @classmethod
    def from_dict(cls, d: dict) -> IbkrRefdataConfig:
        """Build config from a plain dict, ignoring unknown keys."""
        known = {f.name for f in fields(cls)}
        filtered = {k: v for k, v in d.items() if k in known}
        if "universe" in filtered and isinstance(filtered["universe"], list):
            filtered["universe"] = tuple(filtered["universe"])
        if "universe_sources" in filtered and isinstance(
            filtered["universe_sources"], list
        ):
            filtered["universe_sources"] = tuple(filtered["universe_sources"])
        return cls(**filtered)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _build_canonical_id(contract: ib_async.Contract) -> str:
    suffix = _SEC_TYPE_SUFFIX.get(contract.secType, "")
    return f"{contract.symbol}{suffix}/{contract.currency}@{_VENUE}"


def _resolve_instrument_type(sec_type: str, hint_proto_int: int = 0) -> str:
    """Return the canonical proto-aligned UPPERCASE instrument_type for a row.

    Resolver hint (proto enum int from `ResolvedInstrument.instrument_type`)
    takes precedence — only the Nasdaq directory can authoritatively
    distinguish ETF (6) from STOCK (7) for an IBKR STK contract. Explicit
    lists pass UNSPECIFIED (0); we then derive from secType (STK→STOCK).
    """
    if hint_proto_int and hint_proto_int in _PROTO_INT_TO_CANONICAL:
        return _PROTO_INT_TO_CANONICAL[hint_proto_int]
    return _SEC_TYPE_TO_CANONICAL.get(sec_type, "UNSPECIFIED")


def _infer_precision(min_tick: float) -> int:
    if min_tick <= 0:
        return 2
    return max(0, int(-math.floor(math.log10(min_tick))))


def _parse_liquid_hours(liquid_hours: str) -> list[dict[str, str]]:
    """Parse IBKR liquidHours string into session time ranges.

    Handles both IBKR formats:
      Short: "YYYYMMDD:HHMM-HHMM;..."
      Long:  "YYYYMMDD:HHMM-YYYYMMDD:HHMM;..."
    Extracts the distinct time ranges (ignoring dates) and returns unique sessions.
    """
    if not liquid_hours:
        return []

    seen: set[tuple[str, str]] = set()
    sessions: list[dict[str, str]] = []

    for segment in liquid_hours.split(";"):
        segment = segment.strip()
        if not segment or "CLOSED" in segment.upper():
            continue
        try:
            # Split on "-" to separate start and end parts
            # Short: "YYYYMMDD:HHMM-HHMM" -> ["YYYYMMDD:HHMM", "HHMM"]
            # Long:  "YYYYMMDD:HHMM-YYYYMMDD:HHMM" -> ["YYYYMMDD:HHMM", "YYYYMMDD:HHMM"]
            dash_pos = segment.index("-")
            start_part = segment[:dash_pos]
            end_part = segment[dash_pos + 1 :]

            # Extract HHMM from start (after the colon)
            if ":" not in start_part:
                continue
            start_raw = start_part.split(":", 1)[1]

            # Extract HHMM from end (after colon if long format, or raw if short)
            if ":" in end_part:
                # Long format: "YYYYMMDD:HHMM"
                end_raw = end_part.split(":", 1)[1]
            else:
                # Short format: "HHMM"
                end_raw = end_part

            start = f"{start_raw[:2]}:{start_raw[2:4]}"
            end = f"{end_raw[:2]}:{end_raw[2:4]}"
            key = (start, end)
            if key not in seen:
                seen.add(key)
                sessions.append({"name": "regular", "start": start, "end": end})
        except (ValueError, IndexError):
            continue

    return sessions


def _compute_session_state(liquid_hours: str, now: datetime) -> str:
    """Determine if the market is currently open or closed.

    Checks today's liquid hours segment against the current UTC time.
    Returns "open" if *now* falls within any session window, else "closed".
    """
    if not liquid_hours:
        return "closed"

    today_str = now.strftime("%Y%m%d")
    for segment in liquid_hours.split(";"):
        segment = segment.strip()
        if not segment or "CLOSED" in segment.upper():
            continue
        try:
            dash_pos = segment.index("-")
            start_part = segment[:dash_pos]
            end_part = segment[dash_pos + 1:]

            # Extract date and HHMM from start
            if ":" not in start_part:
                continue
            start_date, start_hhmm = start_part.split(":", 1)
            if start_date != today_str:
                continue

            # Extract HHMM from end (handles both long and short formats)
            if ":" in end_part:
                _, end_hhmm = end_part.split(":", 1)
            else:
                end_hhmm = end_part

            start_mins = int(start_hhmm[:2]) * 60 + int(start_hhmm[2:4])
            end_mins = int(end_hhmm[:2]) * 60 + int(end_hhmm[2:4])
            now_mins = now.hour * 60 + now.minute

            if start_mins <= now_mins < end_mins:
                return "open"
        except (ValueError, IndexError):
            continue

    return "closed"


# ---------------------------------------------------------------------------
# Loader
# ---------------------------------------------------------------------------


class IbkrRefdataLoader:
    """Loads instrument reference data and market sessions from IBKR TWS/Gateway."""

    def __init__(self, config: dict | None = None) -> None:
        self._config = IbkrRefdataConfig.from_dict(config or {})
        self._translator = ContractTranslator()
        self._contract_details_cache: dict[str, ib_async.ContractDetails] = {}
        self._conn: IbkrConnection | None = None
        self._rate_limiter = TokenBucketRateLimiter(
            rate=self._config.rate_limit_per_sec
        )

    async def _ensure_connected(self) -> IbkrConnection:
        """Return an active IbkrConnection, creating one if needed."""
        if self._conn and self._conn.state == LIVE:
            return self._conn

        # IbkrConnection expects an IbkrGwConfig; build a minimal one.
        from ibkr.config import IbkrGwConfig

        gw_cfg = IbkrGwConfig(
            host=self._config.host,
            port=self._config.port,
            client_id=self._config.client_id,
            mode=self._config.mode,
            read_only=self._config.read_only,
            reconnect_delay_s=self._config.reconnect_delay_s,
            reconnect_max_delay_s=self._config.reconnect_max_delay_s,
            next_valid_id_timeout_s=self._config.next_valid_id_timeout_s,
        )
        conn = IbkrConnection(gw_cfg)
        await conn.connect()
        self._conn = conn
        return conn

    async def _disconnect(self) -> None:
        if self._conn:
            await self._conn.disconnect()
            self._conn = None

    async def _resolve_universe(self) -> list[ResolvedInstrument]:
        """Return resolved candidates from the configured universe sources.

        ``universe_sources`` is a list of ``{type, config}`` entries processed in
        order. Emissions are merged; on duplicate symbol (first ``-``-separated
        field) the later source wins, so operators can layer a hand-override
        source after a bulk-discovery source.
        """
        if self._config.universe_sources:
            merged: dict[str, ResolvedInstrument] = {}
            for i, source in enumerate(self._config.universe_sources):
                resolver = build_resolver(source)
                resolved = await resolver.resolve()
                log.info(
                    "refdata: source[%d] type=%s emitted %d candidates",
                    i,
                    source.get("type"),
                    len(resolved),
                )
                for ri in resolved:
                    symbol = ri.spec.split("-", 1)[0]
                    merged[symbol] = ri
            return list(merged.values())
        if self._config.universe:
            # Legacy explicit list — no proto type hint; loader infers from secType.
            return [ResolvedInstrument(spec=s) for s in self._config.universe]
        return []

    async def load_instruments(self) -> list[dict]:
        """Connect to TWS, qualify each contract in universe, return refdata dicts."""
        candidates = await self._resolve_universe()
        if not candidates:
            return []

        # Dry-run logs the full resolver output, not a truncated view.
        if self._config.dry_run_candidates_only:
            log.info(
                "refdata: dry_run_candidates_only=true — skipping qualification for "
                "%d candidates (first 5: %s)",
                len(candidates),
                [ri.spec for ri in candidates[:5]],
            )
            return []

        cap = self._config.max_qualifications_per_run
        if cap is not None and len(candidates) > cap:
            log.warning(
                "refdata: truncating %d candidates to max_qualifications_per_run=%d",
                len(candidates),
                cap,
            )
            candidates = candidates[:cap]

        conn = await self._ensure_connected()
        ib = conn.ib
        results: list[dict] = []

        for ri in candidates:
            instrument_str = ri.spec
            # Circuit-breaker: bail out of the candidate loop the moment IBKR
            # drops the socket. Without this, every remaining iteration calls
            # qualify() on a dead connection and raises ConnectionError("Not
            # connected") instantly — flooding the log and leaving the run row
            # stuck in 'running' (the diff/upsert that marks the run never
            # executes because it's after the loop).
            if not ib.isConnected():
                log.error(
                    "refdata: IBKR socket disconnected mid-run after %d/%d "
                    "qualifications; aborting to let the next refresh tick "
                    "reconnect cleanly",
                    len(results),
                    len(candidates),
                )
                raise ConnectionError("IBKR socket disconnected mid-run")
            try:
                # qualifyContractsAsync and reqContractDetailsAsync are two
                # separate IBKR requests. Debit one token per request so the
                # effective outbound rate stays below the configured ceiling.
                await self._rate_limiter.acquire()
                contract = await self._translator.qualify(ib, instrument_str)
                await self._rate_limiter.acquire()
                detail_list = await ib.reqContractDetailsAsync(contract)
                if not detail_list:
                    log.warning("no contract details for %s, skipping", instrument_str)
                    continue

                details = detail_list[0]
                self._contract_details_cache[instrument_str] = details
                c = details.contract

                instrument_type = _resolve_instrument_type(c.secType, ri.instrument_type)
                results.append({
                    "instrument_id": _build_canonical_id(c),
                    "exchange_name": _VENUE,
                    # IBKR-native four-part contract spec ({symbol}-{currency}-{secType}-{exchange}).
                    # OMS forwards this verbatim as gw_req.instrument; ContractTranslator parses
                    # it back to an ib_async.Contract on the gateway side. Use ri.spec rather
                    # than reconstructing from c so the operator's routing intent (e.g. SMART)
                    # is preserved instead of being replaced by a primary exchange.
                    "instrument_exch": ri.spec,
                    "venue": _VENUE,
                    "instrument_type": instrument_type,
                    "base_asset": c.symbol,
                    "quote_asset": c.currency,
                    "settlement_asset": None,
                    "contract_size": float(c.multiplier or 1),
                    "price_precision": _infer_precision(details.minTick),
                    "qty_precision": 0,
                    "price_tick_size": details.minTick,
                    "qty_lot_size": 1.0,
                    "min_notional": None,
                    "min_order_qty": 1.0,
                    "max_order_qty": None,
                    "disabled": False,
                    "currency": c.currency,
                })
            except ConnectionError:
                # Re-raise so the caller (refresh job) sees the run as failed
                # rather than swallowing the disconnect at the per-candidate
                # try/except level.
                raise
            except Exception:
                log.exception("failed to load refdata for %s", instrument_str)

        await self._disconnect()
        return results

    async def load_market_sessions(self) -> list[dict]:
        """Return market session state for the refresh_sessions job.

        Returns list of ``{"venue", "market", "session_state"}`` dicts.
        ``session_state`` is ``"open"`` or ``"closed"`` based on current UTC time
        and the IBKR liquidHours from ContractDetails.

        If the cache is empty (load_instruments was not called or universe is empty),
        connects and qualifies contracts first.
        """
        if not self._config.universe and not self._config.universe_sources:
            return []

        # Populate cache if empty
        if not self._contract_details_cache:
            await self.load_instruments()

        seen_markets: set[str] = set()
        results: list[dict] = []
        now = datetime.now(tz=timezone.utc)

        for instrument_str, details in self._contract_details_cache.items():
            market = _SESSION_MARKET_MAP.get(details.contract.secType, "EQUITY")
            if market in seen_markets:
                continue
            seen_markets.add(market)

            state = _compute_session_state(details.liquidHours or "", now)
            results.append({
                "venue": _VENUE,
                "market": market,
                "session_state": state,
            })

        await self._disconnect()
        return results
