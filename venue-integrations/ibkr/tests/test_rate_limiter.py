"""Tests for TokenBucketRateLimiter."""

from __future__ import annotations

import asyncio
import time

import pytest

from ibkr.rate_limiter import TokenBucketRateLimiter


class TestTokenBucketRateLimiter:
    @pytest.mark.asyncio
    async def test_acquire_under_limit_is_instant(self) -> None:
        rl = TokenBucketRateLimiter(rate=100.0)
        t0 = time.monotonic()
        await rl.acquire(1)
        elapsed = time.monotonic() - t0
        assert elapsed < 0.05

    @pytest.mark.asyncio
    async def test_acquire_over_limit_blocks(self) -> None:
        rl = TokenBucketRateLimiter(rate=10.0, burst=1)
        await rl.acquire(1)  # consume the 1 token
        t0 = time.monotonic()
        await rl.acquire(1)  # must wait ~0.1s
        elapsed = time.monotonic() - t0
        assert elapsed >= 0.08  # allow some tolerance

    def test_try_acquire_returns_false_when_exhausted(self) -> None:
        rl = TokenBucketRateLimiter(rate=10.0, burst=2)
        assert rl.try_acquire(1) is True
        assert rl.try_acquire(1) is True
        assert rl.try_acquire(1) is False

    def test_try_acquire_refills_over_time(self) -> None:
        rl = TokenBucketRateLimiter(rate=1000.0, burst=1)
        assert rl.try_acquire(1) is True
        assert rl.try_acquire(1) is False
        # Simulate time passing
        rl._last_refill -= 0.01  # 10ms ago -> 10 tokens at 1000/s
        assert rl.try_acquire(1) is True

    def test_available_tokens(self) -> None:
        rl = TokenBucketRateLimiter(rate=10.0, burst=10)
        assert rl.available_tokens == 10.0
        rl.try_acquire(3)
        assert rl.available_tokens == pytest.approx(7.0, abs=0.5)

    def test_rejects_zero_rate(self) -> None:
        with pytest.raises(ValueError, match="rate must be > 0"):
            TokenBucketRateLimiter(rate=0)

    @pytest.mark.asyncio
    async def test_burst_higher_than_rate(self) -> None:
        rl = TokenBucketRateLimiter(rate=5.0, burst=20)
        # Should be able to acquire burst amount without blocking
        for _ in range(20):
            assert rl.try_acquire(1) is True
        assert rl.try_acquire(1) is False
