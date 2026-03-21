"""OANDA v20 transaction stream consumer.

Connects to the OANDA streaming endpoint for transactions and converts
incoming events into venue fact dicts on an asyncio.Queue.
"""

from __future__ import annotations

import asyncio
import json

import httpx
from loguru import logger

from . import normalize as norm


class OandaTransactionStream:
    """Consumes OANDA transaction stream via persistent HTTP GET.

    Auto-reconnects with exponential backoff on disconnect.
    """

    def __init__(
        self,
        stream_base_url: str,
        token: str,
        account_id: str,
        event_queue: asyncio.Queue,
    ) -> None:
        self._stream_base_url = stream_base_url
        self._token = token
        self._account_id = account_id
        self._event_queue = event_queue
        self._last_transaction_id: str | None = None
        self._running = True
        self._connected_event = asyncio.Event()

    @property
    def last_transaction_id(self) -> str | None:
        return self._last_transaction_id

    async def wait_connected(self, timeout: float = 15.0) -> bool:
        """Wait for the stream to establish its first connection.

        Returns True if connected within timeout, False otherwise.
        """
        try:
            await asyncio.wait_for(self._connected_event.wait(), timeout=timeout)
            return True
        except asyncio.TimeoutError:
            return False

    async def run(self) -> None:
        """Main loop: connect, consume, auto-reconnect."""
        backoff = 1.0
        max_backoff = 60.0

        while self._running:
            try:
                await self._consume_stream()
                backoff = 1.0  # reset on clean exit
            except httpx.HTTPStatusError as e:
                logger.error(f"OANDA stream HTTP error: {e.response.status_code}")
            except (httpx.ConnectError, httpx.ReadTimeout, httpx.RemoteProtocolError) as e:
                logger.warning(f"OANDA stream connection error: {e}")
            except Exception:
                logger.exception("OANDA stream unexpected error")

            if not self._running:
                break

            logger.info(f"OANDA stream reconnecting in {backoff:.0f}s")
            await asyncio.sleep(backoff)
            backoff = min(backoff * 2, max_backoff)

    async def stop(self) -> None:
        self._running = False

    async def _consume_stream(self) -> None:
        url = f"/v3/accounts/{self._account_id}/transactions/stream"
        logger.info(f"OANDA stream connecting to {self._stream_base_url}{url}")

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
                logger.info("OANDA transaction stream connected")

                # Signal readiness and emit connected system event
                self._connected_event.set()
                await self._event_queue.put({
                    "event_type": "system",
                    "payload": {
                        "event_type": "connected",
                        "message": "OANDA transaction stream connected",
                        "timestamp": int(__import__("time").time() * 1000),
                    },
                })

                async for line in response.aiter_lines():
                    if not line.strip():
                        continue
                    try:
                        data = json.loads(line)
                    except json.JSONDecodeError:
                        logger.warning(f"OANDA stream: invalid JSON line: {line[:200]}")
                        continue

                    await self._handle_transaction(data)

    async def _handle_transaction(self, data: dict) -> None:
        txn_type = data.get("type", "")

        # Track lastTransactionID from heartbeats and transactions
        if "lastTransactionID" in data:
            self._last_transaction_id = data["lastTransactionID"]
        if "id" in data:
            self._last_transaction_id = data["id"]

        if txn_type == "HEARTBEAT":
            return

        # Order creation transactions
        if txn_type in ("MARKET_ORDER", "LIMIT_ORDER", "STOP_ORDER"):
            event = norm.order_create_event(data)
            await self._event_queue.put(event)
            return

        # Order fill
        if txn_type == "ORDER_FILL":
            event = norm.order_fill_event(data)
            await self._event_queue.put(event)
            return

        # Order cancel
        if txn_type == "ORDER_CANCEL":
            event = norm.order_cancel_event(data)
            await self._event_queue.put(event)
            return

        # Rejection events
        if txn_type.endswith("_REJECT"):
            event = norm.order_reject_event(data)
            await self._event_queue.put(event)
            return

        # Log but skip other transaction types
        logger.debug(f"OANDA stream: unhandled transaction type={txn_type}")
