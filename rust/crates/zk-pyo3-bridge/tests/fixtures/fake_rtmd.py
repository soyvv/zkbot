"""Fake RTMD adaptor for integration testing."""

import asyncio


class FakeRtmdAdaptor:
    def __init__(self, config: dict):
        self.config = config
        self._subscriptions = []
        self._connected = False

    async def connect(self) -> None:
        self._connected = True

    async def disconnect(self) -> None:
        self._connected = False

    async def subscribe(self, req: dict) -> None:
        self._subscriptions.append(req)

    async def unsubscribe(self, req: dict) -> None:
        self._subscriptions = [s for s in self._subscriptions if s != req]

    async def snapshot_active(self) -> list[dict]:
        return self._subscriptions

    def instrument_exch_for(self, instrument_code: str) -> str | None:
        return f"FAKE-{instrument_code}"

    async def query_current_tick(self, instrument_code: str) -> bytes:
        # Return empty proto bytes (valid empty TickData message)
        return b""

    async def query_current_orderbook(
        self, instrument_code: str, depth: int | None = None
    ) -> bytes:
        return b""

    async def query_current_funding(self, instrument_code: str) -> bytes:
        return b""

    async def query_klines(
        self,
        instrument_code: str,
        interval: str,
        limit: int,
        from_ms: int | None = None,
        to_ms: int | None = None,
    ) -> list[bytes]:
        return []

    async def next_event(self) -> dict:
        # Block forever (would be replaced by real event queue)
        await asyncio.sleep(3600)
        return {"event_type": "tick", "payload_bytes": b""}
