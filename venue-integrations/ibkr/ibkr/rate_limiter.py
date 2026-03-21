"""Token-bucket rate limiter for IBKR API pacing compliance."""

from __future__ import annotations

import asyncio
import time


class TokenBucketRateLimiter:
    """Async token-bucket rate limiter.

    Default rate of 40 msg/s provides ~20% headroom under IBKR's documented
    50 msg/s hard ceiling before pacing violation error 100.
    """

    def __init__(self, rate: float = 40.0, burst: int | None = None) -> None:
        if rate <= 0:
            raise ValueError(f"rate must be > 0, got {rate}")
        self._rate = rate
        self._burst = burst if burst is not None else max(1, int(rate))
        self._tokens = float(self._burst)
        self._last_refill = time.monotonic()

    def _refill(self) -> None:
        now = time.monotonic()
        elapsed = now - self._last_refill
        self._tokens = min(self._burst, self._tokens + elapsed * self._rate)
        self._last_refill = now

    async def acquire(self, count: int = 1) -> None:
        """Block until *count* tokens are available, then consume them."""
        while True:
            self._refill()
            if self._tokens >= count:
                self._tokens -= count
                return
            # Compute wait time for enough tokens to accumulate
            deficit = count - self._tokens
            wait = deficit / self._rate
            await asyncio.sleep(wait)

    def try_acquire(self, count: int = 1) -> bool:
        """Non-blocking attempt. Returns True if tokens were consumed."""
        self._refill()
        if self._tokens >= count:
            self._tokens -= count
            return True
        return False

    @property
    def available_tokens(self) -> float:
        self._refill()
        return self._tokens
