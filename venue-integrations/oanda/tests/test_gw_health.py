"""Tests for OANDA gateway stream health monitoring."""

import asyncio
import time

import pytest

from oanda.gw import OandaGatewayAdaptor
from oanda.stream import OandaTransactionStream


@pytest.fixture
def adaptor_config():
    return {
        "environment": "practice",
        "account_id": "101-003-12345678-001",
        "token": "test-token",
        "oms_account_id": 9002,
    }


@pytest.fixture
def adaptor(adaptor_config):
    return OandaGatewayAdaptor(adaptor_config)


class TestCheckStreamHealth:
    @pytest.mark.asyncio
    async def test_healthy_stream_is_left_alone(self, adaptor):
        """Healthy stream (task alive, recent activity) should not be restarted."""
        adaptor._stream = OandaTransactionStream(
            stream_base_url="http://fake",
            token="t",
            account_id="a",
            event_queue=adaptor._event_queue,
        )
        adaptor._stream._last_activity = time.monotonic()  # recent
        adaptor._stream_task = asyncio.create_task(_forever())
        adaptor._reconcile_task = asyncio.create_task(_forever())

        await adaptor._check_stream_health()

        # Stream task should still be the original one (not restarted).
        assert not adaptor._stream_task.done()

        adaptor._stream_task.cancel()
        adaptor._reconcile_task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await adaptor._stream_task
        with pytest.raises(asyncio.CancelledError):
            await adaptor._reconcile_task

    @pytest.mark.asyncio
    async def test_stale_stream_triggers_restart(self, adaptor):
        """Stream with no activity beyond threshold should be restarted."""
        adaptor._stream = OandaTransactionStream(
            stream_base_url="http://fake",
            token="t",
            account_id="a",
            event_queue=adaptor._event_queue,
        )
        # Set activity to well beyond the stale threshold.
        adaptor._stream._last_activity = time.monotonic() - 300
        old_task = asyncio.create_task(_forever())
        adaptor._stream_task = old_task
        adaptor._reconcile_task = asyncio.create_task(_forever())

        await adaptor._check_stream_health()

        # Old task should be cancelled; a new stream object + task should exist.
        assert old_task.cancelled() or old_task.done()
        assert adaptor._stream_task is not old_task

        # Cleanup
        adaptor._stream_task.cancel()
        adaptor._reconcile_task.cancel()
        await _suppress_cancel(adaptor._stream_task)
        await _suppress_cancel(adaptor._reconcile_task)

    @pytest.mark.asyncio
    async def test_dead_stream_task_triggers_restart(self, adaptor):
        """Completely dead stream task should be restarted with fresh stream."""
        adaptor._stream = OandaTransactionStream(
            stream_base_url="http://fake",
            token="t",
            account_id="a",
            event_queue=adaptor._event_queue,
        )
        # Create a task that is already done (raised an exception).
        dead_task = asyncio.create_task(_raise_immediately())
        await asyncio.sleep(0)  # let it finish
        assert dead_task.done()

        old_stream = adaptor._stream
        adaptor._stream_task = dead_task
        adaptor._reconcile_task = asyncio.create_task(_forever())

        await adaptor._check_stream_health()

        assert adaptor._stream is not old_stream  # fresh object
        assert adaptor._stream_task is not dead_task

        # Cleanup
        adaptor._stream_task.cancel()
        adaptor._reconcile_task.cancel()
        await _suppress_cancel(adaptor._stream_task)
        await _suppress_cancel(adaptor._reconcile_task)

    @pytest.mark.asyncio
    async def test_dead_reconcile_task_restarted(self, adaptor):
        """Dead reconcile task should be restarted while stream stays intact."""
        adaptor._stream = OandaTransactionStream(
            stream_base_url="http://fake",
            token="t",
            account_id="a",
            event_queue=adaptor._event_queue,
        )
        adaptor._stream._last_activity = time.monotonic()
        adaptor._stream_task = asyncio.create_task(_forever())

        dead_reconcile = asyncio.create_task(_raise_immediately())
        await asyncio.sleep(0)
        adaptor._reconcile_task = dead_reconcile

        await adaptor._check_stream_health()

        assert adaptor._reconcile_task is not dead_reconcile
        assert not adaptor._reconcile_task.done()

        # Cleanup
        adaptor._stream_task.cancel()
        adaptor._reconcile_task.cancel()
        await _suppress_cancel(adaptor._stream_task)
        await _suppress_cancel(adaptor._reconcile_task)


class TestNextEventIdleWatchdog:
    @pytest.mark.asyncio
    async def test_next_event_returns_queued_event_immediately(self, adaptor):
        """next_event should return immediately when an event is already queued."""
        event = {"event_type": "balance", "payload": []}
        await adaptor._event_queue.put(event)
        result = await asyncio.wait_for(adaptor.next_event(), timeout=2.0)
        assert result == event

    @pytest.mark.asyncio
    async def test_next_event_survives_idle_and_returns_delayed_event(self, adaptor):
        """next_event should survive idle check intervals and return a later event."""
        import oanda.gw as gw_mod
        original = gw_mod._IDLE_CHECK_INTERVAL
        gw_mod._IDLE_CHECK_INTERVAL = 0.2  # speed up for test

        try:
            # Push event after a short delay.
            async def delayed_push():
                await asyncio.sleep(0.5)
                await adaptor._event_queue.put({"event_type": "system", "payload": {}})

            asyncio.create_task(delayed_push())
            result = await asyncio.wait_for(adaptor.next_event(), timeout=3.0)
            assert result["event_type"] == "system"
        finally:
            gw_mod._IDLE_CHECK_INTERVAL = original


# ── Helpers ──────────────────────────────────────────────────────────────────

async def _forever():
    """Coroutine that sleeps forever (simulates a healthy running task)."""
    await asyncio.sleep(999999)


async def _raise_immediately():
    """Coroutine that raises immediately (simulates a crashed task)."""
    raise RuntimeError("simulated task crash")


async def _suppress_cancel(task):
    try:
        await task
    except (asyncio.CancelledError, RuntimeError):
        pass
