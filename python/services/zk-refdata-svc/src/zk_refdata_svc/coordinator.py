"""Refresh coordinator — single owner of per-venue concurrency for refresh runs.

Both the periodic scheduler and the operator-triggered ``TriggerVenueRefresh``
RPC route their refresh calls through this object so a manual click and a
scheduled tick can never overlap on the same venue.

Concurrency model: one ``asyncio.Lock`` per venue, lazily created. The manual
trigger checks ``locked()`` and rejects if a refresh is already running; the
periodic loop instead serializes behind any in-flight manual run.
"""

from __future__ import annotations

import asyncio

import nats
from loguru import logger

from zk_refdata_svc.jobs.refresh_refdata import _refresh_venue, _resolve_loader
from zk_refdata_svc.repo import RefdataRepo


class RefreshAlreadyInProgress(RuntimeError):
    """Raised when a manual trigger collides with an in-flight refresh."""

    def __init__(self, venue: str, active_run_id: int | None) -> None:
        super().__init__(f"refresh already in progress for venue {venue!r}")
        self.venue = venue
        self.active_run_id = active_run_id


class UnknownVenue(ValueError):
    """Raised when no loader can be resolved for the given venue name."""


class RefreshCoordinator:
    """Per-venue refresh dispatcher with mutual exclusion."""

    def __init__(
        self,
        repo: RefdataRepo,
        nc: nats.NATS,
        venue_configs: dict[str, dict],
    ) -> None:
        self._repo = repo
        self._nc = nc
        self._venue_configs = venue_configs
        # Single-threaded asyncio: setdefault has no race here.
        self._locks: dict[str, asyncio.Lock] = {}
        # Strong references to fire-and-forget tasks so the GC doesn't collect
        # them before they finish. asyncio.create_task() does not retain a ref
        # to its task — losing it before completion is a documented footgun.
        self._inflight_tasks: set[asyncio.Task] = set()
        # Per-venue cached loader instance. Both refresh_refdata and
        # refresh_sessions route through this so they share connection state
        # (IBKR client_id, contract_details_cache) and never race a fresh
        # connection against a still-active one (zb-00035).
        self._loaders: dict[str, object] = {}

    def _lock_for(self, venue: str) -> asyncio.Lock:
        return self._locks.setdefault(venue, asyncio.Lock())

    def _get_loader(self, venue: str):
        """Return the cached loader for *venue*, building it lazily on first use.
        Returns None if no loader can be resolved (unknown venue or no manifest).
        """
        if venue not in self._loaders:
            self._loaders[venue] = _resolve_loader(venue, self._venue_configs.get(venue))
        return self._loaders[venue]

    def is_locked(self, venue: str) -> bool:
        lock = self._locks.get(venue)
        return lock is not None and lock.locked()

    async def trigger_manual(self, venue: str) -> int:
        """Begin a manual refresh for *venue*, returning its run_id immediately.

        Raises:
            RefreshAlreadyInProgress: another run is currently active.
            UnknownVenue: the venue has no resolvable loader.
        """
        if self.is_locked(venue):
            existing = await self._repo.find_running_run_for_venue(venue)
            raise RefreshAlreadyInProgress(
                venue, existing["run_id"] if existing else None
            )

        loader = self._get_loader(venue)
        if loader is None:
            raise UnknownVenue(f"no loader for venue {venue!r}")

        lock = self._lock_for(venue)
        # Single-threaded asyncio: between the is_locked() check above and
        # this acquire there is no suspension point, so no other coroutine
        # can take the lock in between.
        await lock.acquire()
        try:
            run_id = await self._repo.insert_refresh_run(
                source_name=venue, venue=venue
            )
        except Exception:
            lock.release()
            raise

        # Spawn the actual refresh as a background task; retain a strong
        # reference so the GC doesn't collect it mid-flight.
        task = asyncio.create_task(self._run_under_lock(venue, loader, run_id, lock))
        self._inflight_tasks.add(task)
        task.add_done_callback(self._inflight_tasks.discard)
        return run_id

    async def _run_under_lock(
        self,
        venue: str,
        loader: object,
        run_id: int,
        lock: asyncio.Lock,
    ) -> None:
        try:
            await _refresh_venue(self._repo, self._nc, venue, loader, run_id=run_id)
        except Exception:
            logger.exception(f"manual refresh failed for venue {venue}")
        finally:
            lock.release()

    async def refresh_all_periodic(self, venues: list[str]) -> None:
        """Periodic-loop entry point: walk all venues, holding the per-venue lock.

        If a manual run already holds a lock, the periodic call waits behind it
        and runs immediately after — this keeps the schedule honest without
        overlapping on the same venue.
        """
        for venue in venues:
            loader = self._get_loader(venue)
            if loader is None:
                logger.warning(f"no loader for venue {venue!r}, skipping")
                continue
            lock = self._lock_for(venue)
            async with lock:
                try:
                    await _refresh_venue(self._repo, self._nc, venue, loader)
                except Exception:
                    logger.exception(f"refresh failed for venue {venue}")

    async def load_market_sessions_for(self, venue: str) -> list[dict] | None:
        """Run ``loader.load_market_sessions()`` for *venue* under the same
        per-venue lock used by ``refresh_all_periodic`` so the call cannot
        race a refresh in flight (zb-00035). Returns ``None`` when the venue
        has no resolvable loader so the caller can fall back gracefully.
        Returns an empty list if the loader does not implement
        ``load_market_sessions``.
        """
        loader = self._get_loader(venue)
        if loader is None:
            return None
        if not hasattr(loader, "load_market_sessions"):
            return []
        lock = self._lock_for(venue)
        async with lock:
            return await loader.load_market_sessions()
