"""Simple asyncio periodic scheduler."""

from __future__ import annotations

import asyncio
from collections.abc import Callable, Coroutine
from typing import Any

from loguru import logger


async def run_periodic(
    interval_s: float,
    coro_factory: Callable[..., Coroutine[Any, Any, Any]],
    *args: Any,
    **kwargs: Any,
) -> None:
    """Run *coro_factory(\*args, \*\*kwargs)* every *interval_s* seconds, forever."""
    while True:
        try:
            await coro_factory(*args, **kwargs)
        except asyncio.CancelledError:
            return
        except Exception:
            logger.exception("periodic job failed")
        await asyncio.sleep(interval_s)
