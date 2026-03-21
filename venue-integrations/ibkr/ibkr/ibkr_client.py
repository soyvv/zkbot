"""IB session lifecycle manager: connect, handshake, reconnect."""

from __future__ import annotations

import asyncio
import logging
import random
from typing import Any, Callable

import ib_async

from ibkr.config import IbkrGwConfig
from ibkr.types import (
    CONNECTING,
    CONNECTIVITY_LOST,
    CONNECTIVITY_RESTORED_DATA_LOST,
    CONNECTIVITY_RESTORED_DATA_OK,
    DEGRADED,
    DISCONNECTED,
    LIVE,
    RECONNECTING,
    SOCKET_PORT_RESET,
    WAITING_HANDSHAKE,
    now_ms,
)

log = logging.getLogger(__name__)

# Callback type for pushing system events into the event router
SystemEventCallback = Callable[[dict], None]


class IbkrConnection:
    """Manages the ib_async.IB session lifecycle.

    Responsibilities:
    - Connect to TWS/IB Gateway and wait for nextValidId handshake
    - Allocate session-scoped orderIds
    - Handle connectivity error codes (1100/1101/1102/1300)
    - Auto-reconnect with bounded exponential backoff + jitter
    """

    def __init__(
        self,
        config: IbkrGwConfig,
        system_event_cb: SystemEventCallback | None = None,
        on_reconnect_cb: Callable[[], None] | None = None,
        ib: ib_async.IB | None = None,
    ) -> None:
        self._config = config
        self._ib = ib or ib_async.IB()
        self._system_event_cb = system_event_cb
        self._on_reconnect_cb = on_reconnect_cb
        self._state = DISCONNECTED
        self._next_order_id: int | None = None
        self._handshake_event = asyncio.Event()
        self._reconnect_task: asyncio.Task | None = None
        self._should_reconnect = True

    @property
    def ib(self) -> ib_async.IB:
        return self._ib

    @property
    def state(self) -> str:
        return self._state

    @property
    def next_order_id(self) -> int | None:
        return self._next_order_id

    async def connect(self) -> None:
        """Connect to TWS/IB Gateway and block until nextValidId is received.

        Raises asyncio.TimeoutError if handshake doesn't complete in time.
        Raises ConnectionError on connection failure.
        """
        self._handshake_event.clear()
        self._state = CONNECTING

        # Register event handlers before connecting
        self._ib.connectedEvent += self._on_connected
        self._ib.disconnectedEvent += self._on_disconnected
        self._ib.errorEvent += self._on_error

        try:
            await self._ib.connectAsync(
                self._config.host,
                self._config.port,
                clientId=self._config.client_id,
                readonly=self._config.read_only,
            )
        except Exception as e:
            self._state = DISCONNECTED
            raise ConnectionError(f"failed to connect to TWS/IB Gateway: {e}") from e

        self._state = WAITING_HANDSHAKE

        # Wait for nextValidId via the connectedEvent handler
        try:
            await asyncio.wait_for(
                self._handshake_event.wait(),
                timeout=self._config.next_valid_id_timeout_s,
            )
        except asyncio.TimeoutError:
            self._state = DEGRADED
            raise asyncio.TimeoutError(
                f"nextValidId not received within {self._config.next_valid_id_timeout_s}s"
            )

        self._state = LIVE
        log.info(
            "IBKR connected: host=%s port=%d clientId=%d nextOrderId=%d",
            self._config.host,
            self._config.port,
            self._config.client_id,
            self._next_order_id,
        )

    def allocate_order_id(self) -> int:
        """Return the next orderId and increment.

        Raises RuntimeError if the handshake is not yet complete.
        """
        if self._next_order_id is None:
            raise RuntimeError("cannot allocate orderId: nextValidId handshake not complete")
        oid = self._next_order_id
        self._next_order_id += 1
        return oid

    async def disconnect(self) -> None:
        """Gracefully disconnect."""
        self._should_reconnect = False
        if self._reconnect_task and not self._reconnect_task.done():
            self._reconnect_task.cancel()
            try:
                await self._reconnect_task
            except asyncio.CancelledError:
                pass
        self._ib.disconnect()
        self._state = DISCONNECTED

    # ------------------------------------------------------------------
    # Event handlers
    # ------------------------------------------------------------------

    def _on_connected(self) -> None:
        """Called when ib_async fires connectedEvent after successful connect."""
        # ib_async makes nextValidId available after connection
        # Access it through the client's internal state
        try:
            self._next_order_id = self._ib.client.getReqId()
        except Exception:
            # Fallback: will be set via nextValidId callback
            log.debug("getReqId() not available yet, waiting for nextValidId")
            return
        self._handshake_event.set()

    def _on_disconnected(self) -> None:
        """Called when ib_async fires disconnectedEvent."""
        prev_state = self._state
        self._state = DEGRADED
        self._handshake_event.clear()
        log.warning("IBKR disconnected (was %s)", prev_state)

        self._push_system_event("disconnected", "connection lost")

        if self._should_reconnect:
            self._schedule_reconnect()

    def _on_error(
        self, reqId: int, errorCode: int, errorString: str, contract: Any
    ) -> None:
        """Handle connectivity-related error codes."""
        if errorCode == CONNECTIVITY_LOST:
            self._state = DEGRADED
            self._handshake_event.clear()
            log.warning("[1100] Connectivity lost")
            self._push_system_event("disconnected", f"[1100] {errorString}")
        elif errorCode == CONNECTIVITY_RESTORED_DATA_LOST:
            log.info("[1101] Connectivity restored, data lost — resubscribe needed")
            self._push_system_event("connected", f"[1101] {errorString}")
            # Data lost means we need full reconciliation
        elif errorCode == CONNECTIVITY_RESTORED_DATA_OK:
            log.info("[1102] Connectivity restored, data maintained")
            self._push_system_event("connected", f"[1102] {errorString}")
        elif errorCode == SOCKET_PORT_RESET:
            log.warning("[1300] Socket port reset — reconnect on new port")
            self._push_system_event("reconnecting", f"[1300] {errorString}")
            if self._should_reconnect:
                self._schedule_reconnect()

    # ------------------------------------------------------------------
    # Reconnect logic
    # ------------------------------------------------------------------

    def _schedule_reconnect(self) -> None:
        """Schedule a reconnect attempt if one isn't already running."""
        if self._reconnect_task and not self._reconnect_task.done():
            return
        self._reconnect_task = asyncio.ensure_future(self._reconnect_loop())

    async def _reconnect_loop(self) -> None:
        """Reconnect with bounded exponential backoff + jitter."""
        self._state = RECONNECTING
        delay = self._config.reconnect_delay_s
        max_delay = self._config.reconnect_max_delay_s

        attempt = 0
        while self._should_reconnect:
            attempt += 1
            jitter = random.uniform(0, delay * 0.3)
            wait = min(delay + jitter, max_delay)
            log.info("reconnect attempt %d in %.1fs", attempt, wait)
            await asyncio.sleep(wait)

            if not self._should_reconnect:
                break

            try:
                self._handshake_event.clear()
                self._state = CONNECTING
                await self._ib.connectAsync(
                    self._config.host,
                    self._config.port,
                    clientId=self._config.client_id,
                    readonly=self._config.read_only,
                )
                self._state = WAITING_HANDSHAKE
                await asyncio.wait_for(
                    self._handshake_event.wait(),
                    timeout=self._config.next_valid_id_timeout_s,
                )
                self._state = LIVE
                log.info("reconnected successfully on attempt %d", attempt)
                self._push_system_event("connected", f"reconnected on attempt {attempt}")
                # Notify adaptor to resubscribe (reqAccountUpdates, reqPositions, etc.)
                if self._on_reconnect_cb:
                    try:
                        self._on_reconnect_cb()
                    except Exception as e:
                        log.error("on_reconnect_cb failed: %s", e)
                return
            except Exception as e:
                log.warning("reconnect attempt %d failed: %s", attempt, e)
                self._state = RECONNECTING
                delay = min(delay * 2, max_delay)

        self._state = DISCONNECTED

    # ------------------------------------------------------------------
    # Helpers
    # ------------------------------------------------------------------

    def _push_system_event(self, event_type: str, message: str) -> None:
        if self._system_event_cb:
            self._system_event_cb({
                "event_type": "system",
                "payload": {
                    "event_type": event_type,
                    "message": message,
                    "timestamp": now_ms(),
                },
            })
