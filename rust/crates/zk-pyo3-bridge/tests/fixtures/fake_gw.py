"""Fake gateway adaptor for integration testing."""

import asyncio


class FakeGatewayAdaptor:
    def __init__(self, config: dict):
        self.config = config
        self._event_queue: asyncio.Queue = asyncio.Queue()
        self._connected = False

    async def connect(self) -> None:
        self._connected = True

    async def disconnect(self) -> None:
        self._connected = False

    async def place_order(self, req: dict) -> dict:
        return {
            "success": True,
            "exch_order_ref": f"FAKE-{req.get('correlation_id', 0)}",
            "error_message": None,
        }

    async def cancel_order(self, req: dict) -> dict:
        return {
            "success": True,
            "exch_order_ref": req.get("exch_order_ref"),
            "error_message": None,
        }

    async def query_balance(self, req: dict) -> list[dict]:
        return [
            {"asset": "USDT", "total_qty": 10000.0, "avail_qty": 9000.0, "frozen_qty": 1000.0}
        ]

    async def query_order(self, req: dict) -> list[dict]:
        return []

    async def query_open_orders(self, req: dict) -> list[dict]:
        return []

    async def query_trades(self, req: dict) -> list[dict]:
        return []

    async def query_funding_fees(self, req: dict) -> list[dict]:
        return []

    async def query_positions(self, req: dict) -> list[dict]:
        return []

    async def next_event(self) -> dict:
        return await self._event_queue.get()

    async def push_event(self, event: dict) -> None:
        """Test helper: inject an event into the queue."""
        await self._event_queue.put(event)


class IdleTimeoutGatewayAdaptor:
    """Adaptor whose next_event uses wait_for — matches real adaptor contract.

    Ensures timed-out coroutines complete (raise TimeoutError) so they don't
    steal events from later retry attempts.
    """

    def __init__(self, config: dict):
        self.config = config
        self._event_queue: asyncio.Queue = asyncio.Queue()
        self._connected = False

    async def connect(self) -> None:
        self._connected = True

    async def next_event(self) -> dict:
        # Short internal timeout — the bridge's 2s timeout will fire after this.
        # When wait_for times out, the coroutine raises TimeoutError and completes,
        # preventing zombie queue consumers.
        return await asyncio.wait_for(self._event_queue.get(), timeout=1.0)

    async def push_event(self, event: dict) -> None:
        await self._event_queue.put(event)


class ErrorGatewayAdaptor:
    """Gateway adaptor that raises errors for testing error mapping."""

    def __init__(self, config: dict):
        pass

    async def connect(self) -> None:
        pass

    async def place_order(self, req: dict) -> dict:
        raise RuntimeError("simulated venue error")
