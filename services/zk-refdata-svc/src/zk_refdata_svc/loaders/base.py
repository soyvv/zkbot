"""Async venue loader base class.

Matches Phase 12A contract for forward compatibility with the PyO3 bridge.
"""

from __future__ import annotations

import asyncio
import math
from abc import ABC, abstractmethod

import httpx
from loguru import logger


def int_precision(val: float | None) -> int:
    """Number of decimal places from a tick/lot size value."""
    if not val or val <= 0:
        return 0
    return max(0, -int(math.floor(math.log10(abs(val)))))


def float_precision(val: int) -> float:
    """Tick/lot size from a number of decimal places."""
    if val <= 0:
        return 1.0
    return round(10.0 ** (-val), val)


def instrument_id_ccxt(
    base_asset: str, quote_asset: str, settlement_asset: str | None
) -> str:
    if settlement_asset:
        return f"{base_asset}/{quote_asset}:{settlement_asset}"
    return f"{base_asset}/{quote_asset}"


class VenueLoader(ABC):
    """Base class for venue-specific instrument loaders."""

    def __init__(self, config: dict | None = None) -> None:
        self._config = config or {}

    @abstractmethod
    async def load_instruments(self) -> list[dict]:
        """Fetch and return normalized instrument records as dicts.

        Each dict must have keys matching cfg.instrument_refdata columns:
        instrument_id, instrument_exch, venue, instrument_type, base_asset,
        quote_asset, settlement_asset, contract_size, price_tick_size,
        qty_lot_size, min_notional, max_notional, min_order_qty, max_order_qty,
        max_mkt_order_qty, price_precision, qty_precision, extra_properties,
        disabled.
        """

    async def load_market_sessions(self) -> list[dict]:
        """Load market session data. Default: empty (crypto venues are 24/7)."""
        return []

    @staticmethod
    def instrument_id(
        base_asset: str, type_suffix: str, quote_asset: str, venue: str
    ) -> str:
        return f"{base_asset}{type_suffix}/{quote_asset}@{venue}"

    async def _request(
        self,
        url: str,
        *,
        headers: dict | None = None,
        json_body: dict | None = None,
        method: str = "GET",
        max_retries: int = 5,
    ) -> dict | list | None:
        """HTTP request with retry and backoff."""
        for attempt in range(max_retries):
            try:
                async with httpx.AsyncClient(timeout=30.0) as client:
                    resp = await client.request(
                        method, url, headers=headers or {}, json=json_body
                    )
                    resp.raise_for_status()
                    return resp.json()
            except Exception as e:
                logger.warning(
                    f"{self.__class__.__name__} request failed "
                    f"(attempt {attempt + 1}): {e}"
                )
                if attempt < max_retries - 1:
                    await asyncio.sleep(min(2**attempt, 10))
        logger.error(
            f"{self.__class__.__name__} request failed after {max_retries} attempts: {url}"
        )
        return None

    async def _request_text(
        self,
        url: str,
        *,
        headers: dict | None = None,
        max_retries: int = 5,
    ) -> str | None:
        """HTTP GET returning response text (not JSON)."""
        for attempt in range(max_retries):
            try:
                async with httpx.AsyncClient(timeout=30.0) as client:
                    resp = await client.get(url, headers=headers or {})
                    resp.raise_for_status()
                    return resp.text
            except Exception as e:
                logger.warning(
                    f"{self.__class__.__name__} text request failed "
                    f"(attempt {attempt + 1}): {e}"
                )
                if attempt < max_retries - 1:
                    await asyncio.sleep(min(2**attempt, 10))
        logger.error(
            f"{self.__class__.__name__} text request failed after {max_retries} attempts: {url}"
        )
        return None
