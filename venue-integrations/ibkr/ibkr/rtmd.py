"""IBKR RTMD adaptor — real-time market data via ib_async.

Implements the dict-based async contract consumed by the PyO3 bridge
(see rtmd_adapter.rs / fake_rtmd.py for the interface specification).
"""

from __future__ import annotations

import asyncio
import logging
import math
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any

import ib_async

from ibkr.ibkr_client import IbkrConnection
from ibkr.ibkr_contracts import ContractTranslator
from ibkr.rate_limiter import TokenBucketRateLimiter
from ibkr.types import LIVE, now_ms

from ibkr.proto.zk.rtmd.v1.rtmd_pb2 import (
    FundingRate,
    Kline,
    OrderBook,
    PriceLevel,
    TickData,
    TickUpdateType,
)

log = logging.getLogger(__name__)

_VALID_MODES = frozenset({"paper", "live"})

# Map canonical interval strings to IBKR barSizeSetting values.
_INTERVAL_MAP: dict[str, str] = {
    "1m": "1 min",
    "5m": "5 mins",
    "15m": "15 mins",
    "30m": "30 mins",
    "1h": "1 hour",
    "4h": "4 hours",
    "1d": "1 day",
}

# Map canonical interval strings to Kline proto enum values.
_KLINE_TYPE_MAP: dict[str, int] = {
    "1m": Kline.KlineType.KLINE_1MIN,
    "5m": Kline.KlineType.KLINE_5MIN,
    "15m": Kline.KlineType.KLINE_15MIN,
    "30m": Kline.KlineType.KLINE_30MIN,
    "1h": Kline.KlineType.KLINE_1HOUR,
    "4h": Kline.KlineType.KLINE_4HOUR,
    "1d": Kline.KlineType.KLINE_1DAY,
}

# Map canonical interval to approximate bar duration in seconds, used to
# compute the IBKR durationStr from the requested bar count.
_INTERVAL_SECS: dict[str, int] = {
    "1m": 60,
    "5m": 300,
    "15m": 900,
    "30m": 1800,
    "1h": 3600,
    "4h": 14400,
    "1d": 86400,
}

_EVENT_QUEUE_MAXSIZE = 8192


@dataclass(frozen=True)
class IbkrRtmdConfig:
    """IBKR RTMD adaptor configuration."""

    host: str
    port: int
    client_id: int
    mode: str
    market_data_type: int = 1
    read_only: bool = True
    max_msg_rate: float = 40.0
    reconnect_delay_s: float = 2.0
    reconnect_max_delay_s: float = 60.0
    next_valid_id_timeout_s: float = 10.0

    def __post_init__(self) -> None:
        if self.mode not in _VALID_MODES:
            raise ValueError(f"mode must be one of {_VALID_MODES}, got {self.mode!r}")
        if self.port < 1:
            raise ValueError(f"port must be >= 1, got {self.port}")
        if self.client_id < 0:
            raise ValueError(f"client_id must be >= 0, got {self.client_id}")

    @classmethod
    def from_dict(cls, d: dict) -> IbkrRtmdConfig:
        """Build config from a plain dict, ignoring unknown keys."""
        known = {f.name for f in cls.__dataclass_fields__.values()}
        filtered = {k: v for k, v in d.items() if k in known}
        return cls(**filtered)


# ------------------------------------------------------------------
# Subscription tracking
# ------------------------------------------------------------------

@dataclass
class _Subscription:
    """Internal record for an active market-data subscription."""

    spec: dict
    instrument_code: str
    instrument_exch: str
    channel_type: str
    contract: ib_async.Contract
    # IB request id returned by reqMktData / reqMktDepth
    req_id: int | None = None


# ------------------------------------------------------------------
# Adaptor
# ------------------------------------------------------------------

class IbkrRtmdAdaptor:
    """IBKR real-time market data adaptor.

    Implements the RTMD venue adaptor contract:
    - ``__init__(config: dict)``
    - ``async connect() -> None``
    - ``async subscribe(spec: dict) -> None``
    - ``async unsubscribe(spec: dict) -> None``
    - ``async snapshot_active() -> list[dict]``
    - ``def instrument_exch_for(instrument_code: str) -> str | None``
    - ``async query_current_tick(instrument_code: str) -> bytes``
    - ``async query_current_orderbook(instrument_code: str, depth: int | None) -> bytes``
    - ``async query_current_funding(instrument_code: str) -> bytes``
    - ``async query_klines(instrument_code, interval, limit, from_ms, to_ms) -> list[bytes]``
    - ``async next_event() -> dict``
    """

    def __init__(self, config: dict) -> None:
        self._config = IbkrRtmdConfig.from_dict(config)
        self._rate_limiter = TokenBucketRateLimiter(rate=self._config.max_msg_rate)
        self._contract_translator = ContractTranslator()
        self._event_queue: asyncio.Queue[dict] = asyncio.Queue(maxsize=_EVENT_QUEUE_MAXSIZE)

        # instrument_code -> instrument_exch mapping
        self._instrument_map: dict[str, str] = {}
        # instrument_code -> _Subscription
        self._subscriptions: dict[str, _Subscription] = {}
        # instrument_code -> contract (populated during subscribe)
        self._contract_cache: dict[str, ib_async.Contract] = {}
        # Tick snapshot cache: instrument_code -> serialized TickData bytes
        self._tick_cache: dict[str, bytes] = {}
        # Orderbook snapshot cache: instrument_code -> serialized OrderBook bytes
        self._ob_cache: dict[str, bytes] = {}
        # Reverse lookup: ib conId -> instrument_code (for ticker callback)
        self._conid_to_instrument: dict[int, str] = {}

        # IbkrConnection needs an IbkrGwConfig-shaped object; reuse the RTMD config
        # by building a minimal GwConfig-compatible dict.
        from ibkr.config import IbkrGwConfig

        gw_cfg = IbkrGwConfig.from_dict({
            "host": self._config.host,
            "port": self._config.port,
            "client_id": self._config.client_id,
            "mode": self._config.mode,
            "read_only": self._config.read_only,
            "max_msg_rate": self._config.max_msg_rate,
            "reconnect_delay_s": self._config.reconnect_delay_s,
            "reconnect_max_delay_s": self._config.reconnect_max_delay_s,
            "next_valid_id_timeout_s": self._config.next_valid_id_timeout_s,
        })
        self._conn = IbkrConnection(
            gw_cfg,
            on_reconnect_cb=self._on_reconnect,
        )

    # ------------------------------------------------------------------
    # Lifecycle
    # ------------------------------------------------------------------

    async def connect(self) -> None:
        """Connect to TWS/IB Gateway, configure market data type, register callbacks."""
        await self._conn.connect()
        self._conn.ib.reqMarketDataType(self._config.market_data_type)
        self._register_callbacks()
        log.info(
            "IBKR RTMD adaptor connected (host=%s port=%d clientId=%d mdt=%d)",
            self._config.host,
            self._config.port,
            self._config.client_id,
            self._config.market_data_type,
        )

    async def disconnect(self) -> None:
        """Disconnect from TWS/IB Gateway."""
        await self._conn.disconnect()
        log.info("IBKR RTMD adaptor disconnected")

    # ------------------------------------------------------------------
    # Subscribe / Unsubscribe
    # ------------------------------------------------------------------

    async def subscribe(self, spec: dict) -> None:
        """Subscribe to a market data channel for the given instrument."""
        stream_key = spec["stream_key"]
        instrument_code: str = stream_key["instrument_code"]
        channel_type: str = stream_key["channel"]["type"]
        instrument_exch: str = spec["instrument_exch"]

        # Store instrument mapping
        self._instrument_map[instrument_code] = instrument_exch

        # Build / retrieve IB contract
        contract = self._contract_for_instrument(instrument_code, instrument_exch)

        # Qualify the contract so we get conId for reverse lookup
        try:
            qualified = await self._conn.ib.qualifyContractsAsync(contract)
            if qualified:
                contract = qualified[0]
                self._contract_cache[instrument_code] = contract
                if contract.conId:
                    self._conid_to_instrument[contract.conId] = instrument_code
        except Exception as exc:
            log.warning("contract qualification failed for %s: %s", instrument_code, exc)

        sub_key = f"{instrument_code}:{channel_type}"
        req_id: int | None = None

        if channel_type == "tick":
            await self._rate_limiter.acquire()
            ticker = self._conn.ib.reqMktData(contract)
            req_id = ticker.reqId if hasattr(ticker, "reqId") else None
            log.info("subscribed tick: %s (reqId=%s)", instrument_code, req_id)

        elif channel_type == "order_book":
            # IBKR L2 depth data path is not yet implemented (no callback populates
            # _ob_cache). Log and register the subscription for tracking, but don't
            # call reqMktDepth until the data pipeline is wired end-to-end.
            log.warning(
                "order_book subscribe accepted but L2 data path not yet wired: %s",
                instrument_code,
            )

        elif channel_type == "kline":
            # Klines are query-only in IBKR (reqHistoricalData); no streaming sub needed.
            log.info("kline channel registered (query-only): %s", instrument_code)

        elif channel_type == "funding":
            log.warning("IBKR has no funding rates; ignoring subscribe for %s", instrument_code)
            return

        else:
            log.warning("unknown channel type %r for %s", channel_type, instrument_code)
            return

        self._subscriptions[sub_key] = _Subscription(
            spec=spec,
            instrument_code=instrument_code,
            instrument_exch=instrument_exch,
            channel_type=channel_type,
            contract=contract,
            req_id=req_id,
        )

    async def unsubscribe(self, spec: dict) -> None:
        """Cancel a market data subscription."""
        stream_key = spec["stream_key"]
        instrument_code: str = stream_key["instrument_code"]
        channel_type: str = stream_key["channel"]["type"]
        sub_key = f"{instrument_code}:{channel_type}"

        sub = self._subscriptions.pop(sub_key, None)
        if sub is None:
            log.warning("unsubscribe: no active sub for %s", sub_key)
            return

        if channel_type == "tick":
            await self._rate_limiter.acquire()
            self._conn.ib.cancelMktData(sub.contract)
            self._tick_cache.pop(instrument_code, None)
            log.info("unsubscribed tick: %s", instrument_code)

        elif channel_type == "order_book":
            self._ob_cache.pop(instrument_code, None)
            log.info("unsubscribed orderbook (stub): %s", instrument_code)

        # Clean up instrument_map if no remaining subs reference this instrument
        remaining = any(s.instrument_code == instrument_code for s in self._subscriptions.values())
        if not remaining:
            self._instrument_map.pop(instrument_code, None)
            self._contract_cache.pop(instrument_code, None)
            con_id = sub.contract.conId
            if con_id:
                self._conid_to_instrument.pop(con_id, None)

    # ------------------------------------------------------------------
    # Queries
    # ------------------------------------------------------------------

    async def snapshot_active(self) -> list[dict]:
        """Return specs for all active subscriptions."""
        return [sub.spec for sub in self._subscriptions.values()]

    def instrument_exch_for(self, instrument_code: str) -> str | None:
        """Sync lookup: canonical instrument_code -> venue-native symbol."""
        return self._instrument_map.get(instrument_code)

    async def query_current_tick(self, instrument_code: str) -> bytes:
        """Return cached TickData proto bytes, or empty bytes if not cached."""
        return self._tick_cache.get(instrument_code, b"")

    async def query_current_orderbook(
        self, instrument_code: str, depth: int | None = None
    ) -> bytes:
        """Return cached OrderBook proto bytes, or empty bytes if no L2 sub."""
        return self._ob_cache.get(instrument_code, b"")

    async def query_current_funding(self, instrument_code: str) -> bytes:
        """IBKR has no funding rates — always returns empty bytes."""
        return b""

    async def query_klines(
        self,
        instrument_code: str,
        interval: str,
        limit: int,
        from_ms: int | None = None,
        to_ms: int | None = None,
    ) -> list[bytes]:
        """Fetch historical bars from IBKR and return as list of serialized Kline protos."""
        bar_size = _INTERVAL_MAP.get(interval)
        if bar_size is None:
            log.error("unsupported kline interval %r", interval)
            return []

        contract = self._contract_cache.get(instrument_code)
        if contract is None:
            log.error("no contract cached for %s — subscribe first", instrument_code)
            return []

        duration_str = _compute_duration_str(interval, limit)
        end_dt = ""
        if to_ms is not None:
            end_dt = datetime.fromtimestamp(to_ms / 1000.0, tz=timezone.utc).strftime(
                "%Y%m%d-%H:%M:%S"
            )

        await self._rate_limiter.acquire()
        try:
            bars = await self._conn.ib.reqHistoricalDataAsync(
                contract,
                endDateTime=end_dt,
                durationStr=duration_str,
                barSizeSetting=bar_size,
                whatToShow="TRADES",
                useRTH=False,
            )
        except Exception as exc:
            log.error("reqHistoricalData failed for %s: %s", instrument_code, exc)
            return []

        if bars is None:
            return []

        kline_type = _KLINE_TYPE_MAP.get(interval, Kline.KlineType.KLINE_1MIN)
        result: list[bytes] = []
        for bar in bars:
            ts_ms = int(bar.date.timestamp() * 1000) if hasattr(bar.date, "timestamp") else 0
            bar_secs = _INTERVAL_SECS.get(interval, 60)
            kline = Kline(
                timestamp=ts_ms,
                kline_end_timestamp=ts_ms + bar_secs * 1000,
                open=bar.open,
                high=bar.high,
                close=bar.close,
                low=bar.low,
                volume=bar.volume,
                amount=0.0,
                kline_type=kline_type,
                symbol=instrument_code,
                source="IBKR",
            )
            result.append(kline.SerializeToString())

        return result

    async def next_event(self) -> dict:
        """Block until the next market-data event is available."""
        return await self._event_queue.get()

    # ------------------------------------------------------------------
    # Callbacks
    # ------------------------------------------------------------------

    def _register_callbacks(self) -> None:
        """Register ib_async event callbacks. Safe to call multiple times."""
        ib = self._conn.ib
        try:
            ib.pendingTickersEvent -= self._on_pending_tickers
        except ValueError:
            pass
        ib.pendingTickersEvent += self._on_pending_tickers

    def _on_pending_tickers(self, tickers: set[Any]) -> None:
        """Handle incoming ticker updates from ib_async."""
        ts_ms = now_ms()
        for ticker in tickers:
            contract = ticker.contract
            instrument_code: str | None = None

            # Reverse-lookup by conId
            if contract and contract.conId:
                instrument_code = self._conid_to_instrument.get(contract.conId)

            # Fallback: try matching by symbol in instrument_map values
            if instrument_code is None and contract:
                symbol = contract.symbol
                for ic, ie in self._instrument_map.items():
                    if ie == symbol:
                        instrument_code = ic
                        break

            if instrument_code is None:
                continue

            exchange = contract.exchange if contract else "IBKR"

            # Build TickData proto
            buy_levels: list[PriceLevel] = []
            sell_levels: list[PriceLevel] = []

            bid = getattr(ticker, "bid", None)
            bid_size = getattr(ticker, "bidSize", None)
            if bid is not None and bid == bid and bid > 0:  # NaN check
                buy_levels.append(PriceLevel(
                    price=bid,
                    qty=float(bid_size) if bid_size is not None and bid_size == bid_size else 0.0,
                ))

            ask = getattr(ticker, "ask", None)
            ask_size = getattr(ticker, "askSize", None)
            if ask is not None and ask == ask and ask > 0:
                sell_levels.append(PriceLevel(
                    price=ask,
                    qty=float(ask_size) if ask_size is not None and ask_size == ask_size else 0.0,
                ))

            last = getattr(ticker, "last", None)
            last_size = getattr(ticker, "lastSize", None)
            volume = getattr(ticker, "volume", None)

            tick = TickData(
                tick_type=TickUpdateType.M,
                instrument_code=instrument_code,
                exchange=exchange,
                original_timestamp=ts_ms,
                received_timestamp=ts_ms,
                volume=float(volume) if volume is not None and volume == volume else 0.0,
                latest_trade_price=float(last) if last is not None and last == last else 0.0,
                latest_trade_qty=float(last_size)
                if last_size is not None and last_size == last_size
                else 0.0,
                buy_price_levels=buy_levels,
                sell_price_levels=sell_levels,
            )

            serialized = tick.SerializeToString()
            self._tick_cache[instrument_code] = serialized

            event = {"event_type": "tick", "payload_bytes": serialized}
            try:
                self._event_queue.put_nowait(event)
            except asyncio.QueueFull:
                log.warning("event queue full, dropping tick for %s", instrument_code)

    # ------------------------------------------------------------------
    # Reconnect
    # ------------------------------------------------------------------

    def _on_reconnect(self) -> None:
        """Re-establish market data subscriptions after reconnect."""
        self._conn.ib.reqMarketDataType(self._config.market_data_type)
        self._register_callbacks()

        for sub in self._subscriptions.values():
            try:
                if sub.channel_type == "tick":
                    self._conn.ib.reqMktData(sub.contract)
                # order_book: L2 data path not yet wired, skip reqMktDepth
            except Exception as exc:
                log.error(
                    "failed to resubscribe %s:%s on reconnect: %s",
                    sub.instrument_code,
                    sub.channel_type,
                    exc,
                )

        log.info("post-reconnect: resubscribed %d market data streams", len(self._subscriptions))

    # ------------------------------------------------------------------
    # Helpers
    # ------------------------------------------------------------------

    def _contract_for_instrument(
        self, instrument_code: str, instrument_exch: str
    ) -> ib_async.Contract:
        """Get or build an IB contract for the given instrument.

        Tries the contract cache first, then the ContractTranslator cache,
        and finally builds a basic Stock contract from instrument_exch.
        """
        if instrument_code in self._contract_cache:
            return self._contract_cache[instrument_code]

        # Try ContractTranslator if instrument_exch looks like a transport key
        if instrument_exch.count("-") >= 3:
            try:
                contract = self._contract_translator.to_ib_contract(instrument_exch)
                self._contract_cache[instrument_code] = contract
                return contract
            except ValueError:
                pass

        # Default: build a basic Stock contract
        contract = ib_async.Stock(symbol=instrument_exch, exchange="SMART", currency="USD")
        self._contract_cache[instrument_code] = contract
        return contract


def _compute_duration_str(interval: str, limit: int) -> str:
    """Compute IBKR durationStr from interval and bar count.

    IBKR accepts: "N S" (seconds), "N D" (days), "N W" (weeks),
    "N M" (months), "N Y" (years).
    """
    bar_secs = _INTERVAL_SECS.get(interval, 60)
    total_secs = bar_secs * limit

    if total_secs <= 86400:
        # Express in seconds for sub-day durations
        return f"{total_secs} S"
    days = math.ceil(total_secs / 86400)
    if days <= 365:
        return f"{days} D"
    # For very long durations, use years
    years = math.ceil(days / 365)
    return f"{years} Y"
