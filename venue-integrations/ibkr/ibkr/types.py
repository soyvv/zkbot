"""Shared constants and helpers for the IBKR venue integration."""

from __future__ import annotations

import time

# ---------------------------------------------------------------------------
# IBKR connectivity error codes (from TWS API docs)
# ---------------------------------------------------------------------------

CONNECTIVITY_LOST = 1100
CONNECTIVITY_RESTORED_DATA_LOST = 1101
CONNECTIVITY_RESTORED_DATA_OK = 1102
SOCKET_PORT_RESET = 1300

CONNECTIVITY_CODES = frozenset({
    CONNECTIVITY_LOST,
    CONNECTIVITY_RESTORED_DATA_LOST,
    CONNECTIVITY_RESTORED_DATA_OK,
    SOCKET_PORT_RESET,
})

# Pacing violation — IBKR sends this when message rate exceeds ~50/s.
PACING_VIOLATION = 100

# ---------------------------------------------------------------------------
# Connection state constants
# ---------------------------------------------------------------------------

DISCONNECTED = "disconnected"
CONNECTING = "connecting"
WAITING_HANDSHAKE = "waiting_handshake"
LIVE = "live"
DEGRADED = "degraded"
RECONNECTING = "reconnecting"

# ---------------------------------------------------------------------------
# IBKR order status -> canonical venue status mapping
# ---------------------------------------------------------------------------

IBKR_STATUS_MAP: dict[str, str] = {
    "PreSubmitted": "booked",
    "Submitted": "booked",
    "Filled": "filled",
    "Cancelled": "cancelled",
    "Inactive": "rejected",
    "ApiCancelled": "cancelled",
    "ApiPending": "booked",
    "PendingSubmit": "booked",
    "PendingCancel": "booked",
}

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def now_ms() -> int:
    """Current time in milliseconds since epoch."""
    return int(time.time() * 1000)
