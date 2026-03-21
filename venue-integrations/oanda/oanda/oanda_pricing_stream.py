"""OANDA v20 pricing stream consumer for RTMD tick data.

Connects to the OANDA streaming pricing endpoint and converts incoming
price updates into TickData protobuf events on an asyncio.Queue.
"""

from __future__ import annotations

import asyncio
import json
import time

import httpx
from loguru import logger

from . import oanda_normalize as norm


class OandaPricingStream:
    """Consumes OANDA pricing stream via persistent HTTP GET.

    Unlike the transaction stream, the pricing stream requires all subscribed
    instruments in the URL query parameter. When instruments change, the stream
    must be reconnected.
    """

    def __init__(
        self,
        stream_base_url: str,
        token: str,
        account_id: str,
        event_queue: asyncio.Queue,
        venue: str = "oanda",
    ) -> None:
        self._stream_base_url = stream_base_url
        self._token = token
        self._account_id = account_id
        self._event_queue = event_queue
        self._venue = venue
        self._instruments: set[str] = set()
        self._running = False
        self._connected_event = asyncio.Event()
        self._reconnect_event = asyncio.Event()

    @property
    def instruments(self) -> set[str]:
        return set(self._instruments)

    def update_instruments(self, instruments: set[str]) -> None:
        """Update subscribed instruments. Triggers reconnect if running."""
        self._instruments = set(instruments)
        if self._running:
            self._reconnect_event.set()

    async def wait_connected(self, timeout: float = 15.0) -> bool:
        """Wait for stream to establish its first connection."""
        try:
            await asyncio.wait_for(self._connected_event.wait(), timeout=timeout)
            return True
        except asyncio.TimeoutError:
            return False

    async def run(self) -> None:
        """Main loop: connect, consume, auto-reconnect."""
        self._running = True
        backoff = 1.0
        max_backoff = 60.0

        while self._running:
            if not self._instruments:
                # No instruments subscribed — wait for subscription change
                self._reconnect_event.clear()
                await self._reconnect_event.wait()
                continue

            try:
                await self._consume_stream()
                backoff = 1.0
            except httpx.HTTPStatusError as e:
                logger.error(f"OANDA pricing stream HTTP error: {e.response.status_code}")
            except (httpx.ConnectError, httpx.ReadTimeout, httpx.RemoteProtocolError) as e:
                logger.warning(f"OANDA pricing stream connection error: {e}")
            except _ReconnectRequested:
                backoff = 0.1  # fast reconnect on subscription change
                continue
            except Exception:
                logger.exception("OANDA pricing stream unexpected error")

            if not self._running:
                break

            logger.info(f"OANDA pricing stream reconnecting in {backoff:.1f}s")
            await asyncio.sleep(backoff)
            backoff = min(backoff * 2, max_backoff)

    async def stop(self) -> None:
        self._running = False
        self._reconnect_event.set()

    async def _consume_stream(self) -> None:
        instruments_csv = ",".join(sorted(self._instruments))
        url = f"/v3/accounts/{self._account_id}/pricing/stream?instruments={instruments_csv}"
        logger.info(f"OANDA pricing stream connecting: {len(self._instruments)} instruments")

        self._reconnect_event.clear()

        async with httpx.AsyncClient(
            base_url=self._stream_base_url,
            headers={
                "Authorization": f"Bearer {self._token}",
                "Accept-Datetime-Format": "RFC3339",
            },
            timeout=httpx.Timeout(connect=10.0, read=90.0, write=10.0, pool=10.0),
        ) as client:
            async with client.stream("GET", url) as response:
                response.raise_for_status()
                logger.info("OANDA pricing stream connected")
                self._connected_event.set()

                async for line in response.aiter_lines():
                    # Check for reconnect request (instruments changed)
                    if self._reconnect_event.is_set():
                        raise _ReconnectRequested()

                    if not line.strip():
                        continue
                    try:
                        data = json.loads(line)
                    except json.JSONDecodeError:
                        logger.warning(f"OANDA pricing: invalid JSON: {line[:200]}")
                        continue

                    msg_type = data.get("type", "")
                    if msg_type == "PRICE":
                        tick_bytes = norm.build_tick_data(data, self._venue)
                        await self._event_queue.put({
                            "event_type": "tick",
                            "payload_bytes": tick_bytes,
                        })
                    # HEARTBEAT messages are silently ignored


class _ReconnectRequested(Exception):
    """Internal signal to trigger stream reconnection."""
