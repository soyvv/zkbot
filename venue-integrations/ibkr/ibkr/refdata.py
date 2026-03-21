"""IBKR refdata loader — fetches instrument metadata from TWS/IB Gateway."""

from __future__ import annotations

import logging
import math
from dataclasses import dataclass, fields
from datetime import datetime, timezone

import ib_async

from ibkr.ibkr_client import IbkrConnection
from ibkr.ibkr_contracts import ContractTranslator
from ibkr.types import LIVE

log = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

_VENUE = "IBKR"
_SEC_TYPE_MAP = {"STK": "spot", "FUT": "future", "OPT": "option", "CFD": "cfd", "CASH": "forex"}
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
    read_only: bool = True
    reconnect_delay_s: float = 2.0
    reconnect_max_delay_s: float = 60.0
    next_valid_id_timeout_s: float = 10.0

    def __post_init__(self) -> None:
        if self.mode not in _VALID_MODES:
            raise ValueError(f"mode must be one of {_VALID_MODES}, got {self.mode!r}")

    @classmethod
    def from_dict(cls, d: dict) -> IbkrRefdataConfig:
        """Build config from a plain dict, ignoring unknown keys."""
        known = {f.name for f in fields(cls)}
        filtered = {k: v for k, v in d.items() if k in known}
        # Convert universe list to tuple if needed
        if "universe" in filtered and isinstance(filtered["universe"], list):
            filtered["universe"] = tuple(filtered["universe"])
        return cls(**filtered)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _build_canonical_id(contract: ib_async.Contract) -> str:
    suffix = _SEC_TYPE_SUFFIX.get(contract.secType, "")
    return f"{contract.symbol}{suffix}/{contract.currency}@{_VENUE}"


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

    async def load_instruments(self) -> list[dict]:
        """Connect to TWS, qualify each contract in universe, return refdata dicts."""
        if not self._config.universe:
            return []

        conn = await self._ensure_connected()
        ib = conn.ib
        results: list[dict] = []

        for instrument_str in self._config.universe:
            try:
                contract = await self._translator.qualify(ib, instrument_str)
                detail_list = await ib.reqContractDetailsAsync(contract)
                if not detail_list:
                    log.warning("no contract details for %s, skipping", instrument_str)
                    continue

                details = detail_list[0]
                self._contract_details_cache[instrument_str] = details
                c = details.contract

                results.append({
                    "instrument_id": _build_canonical_id(c),
                    "instrument_id_exchange": c.symbol,
                    "exchange_name": _VENUE,
                    "instrument_exch": c.symbol,
                    "venue": _VENUE,
                    "instrument_type": _SEC_TYPE_MAP.get(c.secType, "unknown"),
                    "base_asset": c.symbol,
                    "quote_asset": c.currency,
                    "settlement_asset": None,
                    "contract_size": float(details.multiplier or 1),
                    "price_precision": _infer_precision(details.minTick),
                    "qty_precision": 0,
                    "price_tick_size": details.minTick,
                    "qty_lot_size": 1.0,
                    "min_notional": None,
                    "min_order_qty": 1.0,
                    "max_order_qty": None,
                    "disabled": False,
                    "currency": c.currency,
                    "asset_class": _SEC_TYPE_MAP.get(c.secType, "unknown"),
                })
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
        if not self._config.universe:
            return []

        # Populate cache if empty
        if not self._contract_details_cache:
            await self.load_instruments()

        seen_markets: set[str] = set()
        results: list[dict] = []
        now = datetime.now(tz=timezone.utc)

        for instrument_str, details in self._contract_details_cache.items():
            market = _SEC_TYPE_MAP.get(details.contract.secType, "equity")
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
