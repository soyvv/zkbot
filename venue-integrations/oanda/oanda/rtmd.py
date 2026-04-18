"""OANDA RTMD adaptor.

Implements the Python RTMD venue adaptor contract for the OANDA v20 API.
Loaded by the Rust ``zk-rtmd-gw-svc`` host via ``zk-pyo3-bridge``.

Supports tick (pricing stream) and kline (REST candle polling) channels.
OrderBook and Funding are not supported (OANDA is FX/CFD, not perps).

Bridge constraints:
- All async methods run on one persistent Python event loop managed by ``zk-pyo3-bridge``.
- Do NOT call ``asyncio.run()`` inside this adaptor.
- Use an internal ``asyncio.Queue`` for events; ``next_event()`` awaits it.
"""

from __future__ import annotations

import asyncio

from loguru import logger

from .client import OandaApiError, OandaRestClient
from .pricing_stream import OandaPricingStream
from . import normalize as norm

_API_URLS = {
    "practice": "https://api-fxpractice.oanda.com",
    "live": "https://api-fxtrade.oanda.com",
}
_STREAM_URLS = {
    "practice": "https://stream-fxpractice.oanda.com",
    "live": "https://stream-fxtrade.oanda.com",
}

# Kline poll intervals (seconds) per granularity
_KLINE_POLL_INTERVAL: dict[str, float] = {
    "M1": 10.0, "M5": 30.0, "M15": 60.0, "M30": 120.0,
    "H1": 300.0, "H4": 600.0, "D": 3600.0,
}


class OandaRtmdAdaptor:
    """OANDA RTMD venue adaptor loaded by the Rust RTMD gateway host.

    Config keys:
        environment: "practice" or "live"
        account_id: OANDA account ID string
        token: API bearer token (resolved from secret_ref by the host)
        api_base_url: (optional) override REST base URL
        stream_base_url: (optional) override stream base URL
    """

    def __init__(self, config: dict) -> None:
        env = config.get("environment", "practice")
        self._oanda_account_id = config["account_id"]
        self._token = config.get("token") or config["secret_ref"]
        self._api_base_url = config.get("api_base_url") or _API_URLS[env]
        self._stream_base_url = config.get("stream_base_url") or _STREAM_URLS[env]
        self._event_queue: asyncio.Queue = asyncio.Queue(maxsize=8192)
        self._client: OandaRestClient | None = None
        self._pricing_stream: OandaPricingStream | None = None
        self._stream_task: asyncio.Task | None = None
        self._connected = False

        # Subscription tracking
        # Key: (instrument_exch, channel_type_str) — for kline, channel_type_str
        # includes the interval (e.g. "kline:1m") so distinct intervals are not
        # collapsed into a single subscription.
        self._active_subs: dict[tuple[str, str], dict] = {}
        # instrument_code → instrument_exch mapping
        self._instrument_map: dict[str, str] = {}
        # Kline poll tasks: key = (instrument_exch, granularity)
        self._kline_tasks: dict[tuple[str, str], asyncio.Task] = {}
        # Last emitted kline timestamp per (instrument, granularity)
        self._kline_watermarks: dict[tuple[str, str], int] = {}

    async def connect(self) -> None:
        """Establish REST client connectivity."""
        self._client = OandaRestClient(
            api_base_url=self._api_base_url,
            token=self._token,
            account_id=self._oanda_account_id,
        )
        summary = await self._client.get_account_summary()
        acct = summary.get("account", {})
        logger.info(
            f"OANDA RTMD connected: account={self._oanda_account_id} "
            f"currency={acct.get('currency', '?')}"
        )
        self._pricing_stream = OandaPricingStream(
            stream_base_url=self._stream_base_url,
            token=self._token,
            account_id=self._oanda_account_id,
            event_queue=self._event_queue,
        )
        self._stream_task = asyncio.create_task(
            self._pricing_stream.run(), name="oanda-pricing-stream"
        )
        self._connected = True

    async def subscribe(self, spec: dict) -> None:
        """Subscribe to a market data channel.

        spec shape (from pythonize):
        {"stream_key": {"instrument_code": "EUR_USD", "channel": {"type": "tick"}},
         "instrument_exch": "EUR_USD", "venue": "oanda"}
        """
        stream_key = spec["stream_key"]
        instrument_code = stream_key["instrument_code"]
        instrument_exch = spec["instrument_exch"]
        channel = stream_key["channel"]
        channel_type = channel["type"]

        self._instrument_map[instrument_code] = instrument_exch
        sub_key = self._sub_key(instrument_exch, channel)

        if sub_key in self._active_subs:
            return

        self._active_subs[sub_key] = spec

        if channel_type == "tick":
            assert self._pricing_stream is not None
            current = self._pricing_stream.instruments
            current.add(instrument_exch)
            self._pricing_stream.update_instruments(current)
            logger.info(f"RTMD subscribed tick: {instrument_exch}")

        elif channel_type == "kline":
            interval = channel.get("interval", "1m")
            granularity = norm.oanda_granularity(interval)
            task_key = (instrument_exch, granularity)
            if task_key not in self._kline_tasks:
                task = asyncio.create_task(
                    self._kline_poll_loop(instrument_exch, granularity),
                    name=f"kline-{instrument_exch}-{granularity}",
                )
                self._kline_tasks[task_key] = task
                logger.info(f"RTMD subscribed kline: {instrument_exch} {granularity}")

    async def unsubscribe(self, spec: dict) -> None:
        """Unsubscribe from a market data channel."""
        stream_key = spec["stream_key"]
        instrument_exch = spec["instrument_exch"]
        channel = stream_key["channel"]
        channel_type = channel["type"]

        sub_key = self._sub_key(instrument_exch, channel)
        self._active_subs.pop(sub_key, None)

        if channel_type == "tick":
            assert self._pricing_stream is not None
            current = self._pricing_stream.instruments
            current.discard(instrument_exch)
            self._pricing_stream.update_instruments(current)
            logger.info(f"RTMD unsubscribed tick: {instrument_exch}")

        elif channel_type == "kline":
            interval = channel.get("interval", "1m")
            granularity = norm.oanda_granularity(interval)
            task_key = (instrument_exch, granularity)
            task = self._kline_tasks.pop(task_key, None)
            if task:
                task.cancel()
                try:
                    await task
                except asyncio.CancelledError:
                    pass
                logger.info(f"RTMD unsubscribed kline: {instrument_exch} {granularity}")

    async def snapshot_active(self) -> list[dict]:
        """Return current active subscriptions as list of RtmdSubscriptionSpec dicts."""
        return list(self._active_subs.values())

    async def next_event(self) -> dict:
        """Await and return the next RTMD event from the internal queue."""
        return await self._event_queue.get()

    def instrument_exch_for(self, instrument_code: str) -> str | None:
        """Map internal instrument code to venue-native instrument identifier."""
        return self._instrument_map.get(instrument_code)

    # ── query methods ────────────────────────────────────────────────────

    async def query_current_tick(self, instrument_code: str) -> bytes:
        """Query current tick (best bid/ask) via REST pricing endpoint."""
        assert self._client is not None
        instrument_exch = self._instrument_map.get(instrument_code, instrument_code)
        resp = await self._client.get_pricing([instrument_exch])
        prices = resp.get("prices", [])
        if not prices:
            raise ValueError(f"No pricing data for {instrument_exch}")
        return norm.build_tick_data(prices[0])

    async def query_current_orderbook(
        self, instrument_code: str, depth: int | None = None
    ) -> bytes:
        """OANDA does not provide traditional L2 orderbook data."""
        raise NotImplementedError("OANDA does not support orderbook queries")

    async def query_current_funding(self, instrument_code: str) -> bytes:
        """OANDA FX/CFD has no funding rates."""
        raise NotImplementedError("OANDA does not support funding rate queries")

    async def query_klines(
        self,
        instrument_code: str,
        interval: str,
        limit: int,
        from_ms: int | None = None,
        to_ms: int | None = None,
    ) -> list[bytes]:
        """Query historical klines via REST candles endpoint."""
        assert self._client is not None
        instrument_exch = self._instrument_map.get(instrument_code, instrument_code)
        granularity = norm.oanda_granularity(interval)

        kwargs: dict = {"granularity": granularity, "count": limit}
        if from_ms is not None:
            from datetime import datetime, timezone

            kwargs["from_time"] = datetime.fromtimestamp(
                from_ms / 1000, tz=timezone.utc
            ).strftime("%Y-%m-%dT%H:%M:%S.000000000Z")
            kwargs.pop("count", None)
        if to_ms is not None:
            from datetime import datetime, timezone

            kwargs["to_time"] = datetime.fromtimestamp(
                to_ms / 1000, tz=timezone.utc
            ).strftime("%Y-%m-%dT%H:%M:%S.000000000Z")

        resp = await self._client.get_candles(instrument_exch, **kwargs)
        candles = resp.get("candles", [])
        return [
            norm.build_kline(c, instrument_exch, granularity)
            for c in candles
            if c.get("complete", False)
        ]

    # ── shutdown ──────────────────────────────────────────────────────────

    async def shutdown(self) -> None:
        if self._pricing_stream:
            await self._pricing_stream.stop()
        if self._stream_task:
            self._stream_task.cancel()
            try:
                await self._stream_task
            except asyncio.CancelledError:
                pass
        for task in self._kline_tasks.values():
            task.cancel()
            try:
                await task
            except asyncio.CancelledError:
                pass
        self._kline_tasks.clear()
        if self._client:
            await self._client.close()
        self._connected = False
        logger.info("OANDA RTMD adaptor shut down")

    # ── internal ──────────────────────────────────────────────────────────

    @staticmethod
    def _sub_key(instrument_exch: str, channel: dict) -> tuple[str, str]:
        """Build a unique subscription key, including interval for kline channels."""
        channel_type = channel["type"]
        if channel_type == "kline":
            interval = channel.get("interval", "1m")
            return (instrument_exch, f"kline:{interval}")
        return (instrument_exch, channel_type)

    async def _kline_poll_loop(self, instrument_exch: str, granularity: str) -> None:
        """Periodically poll REST candles and emit new completed klines."""
        poll_interval = _KLINE_POLL_INTERVAL.get(granularity, 30.0)
        wm_key = (instrument_exch, granularity)
        poll_count = 0

        while True:
            await asyncio.sleep(poll_interval)
            try:
                assert self._client is not None
                resp = await self._client.get_candles(
                    instrument_exch, granularity=granularity, count=2
                )
                candles = resp.get("candles", [])
                poll_count += 1
                complete_count = sum(1 for c in candles if c.get("complete", False))
                for candle in candles:
                    if not candle.get("complete", False):
                        continue
                    ts = norm.parse_oanda_time(candle["time"])
                    watermark = self._kline_watermarks.get(wm_key, 0)
                    if ts > watermark:
                        self._kline_watermarks[wm_key] = ts
                        kline_bytes = norm.build_kline(
                            candle, instrument_exch, granularity
                        )
                        logger.info(
                            "kline emitted | {} {} ts={} qsize={}",
                            instrument_exch, granularity, ts,
                            self._event_queue.qsize(),
                        )
                        await self._event_queue.put({
                            "event_type": "kline",
                            "payload_bytes": kline_bytes,
                        })
                    elif poll_count <= 3 or poll_count % 30 == 0:
                        logger.debug(
                            "kline skipped (watermark) | {} {} ts={} wm={}",
                            instrument_exch, granularity, ts, watermark,
                        )
            except asyncio.CancelledError:
                raise
            except OandaApiError as e:
                if e.is_transient:
                    logger.warning(
                        "kline poll transient error for {} {} — {}",
                        instrument_exch, granularity, e,
                    )
                else:
                    logger.error(
                        "kline poll error for {} {} — {}",
                        instrument_exch, granularity, e,
                    )
            except Exception:
                logger.exception(
                    f"kline poll error for {instrument_exch} {granularity}"
                )
