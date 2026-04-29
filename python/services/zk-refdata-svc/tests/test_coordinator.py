"""Tests for RefreshCoordinator (per-venue lock + manual trigger semantics)."""

from __future__ import annotations

import asyncio
import itertools
from typing import Any

import pytest

from zk_refdata_svc.coordinator import (
    RefreshAlreadyInProgress,
    RefreshCoordinator,
    UnknownVenue,
)


class FakeLoader:
    def __init__(self, hold: asyncio.Event | None = None) -> None:
        self._hold = hold
        self.load_calls = 0

    async def load_instruments(self) -> list[dict]:
        self.load_calls += 1
        if self._hold is not None:
            await self._hold.wait()
        return []


class FakeRepo:
    """Minimal repo stub: tracks insert + complete + venue rows."""

    def __init__(self) -> None:
        self._counter = itertools.count(start=100)
        self.inserted: list[tuple[str, str]] = []
        self.completed: list[tuple[int, str]] = []
        self._runs: dict[int, dict] = {}

    async def insert_refresh_run(self, source_name: str, venue: str) -> int:
        run_id = next(self._counter)
        self.inserted.append((source_name, venue))
        self._runs[run_id] = {
            "run_id": run_id,
            "venue": venue,
            "status": "running",
            "instruments_added": 0,
            "instruments_updated": 0,
            "instruments_disabled": 0,
            "error_detail": None,
            "started_at_ms": 0,
            "ended_at_ms": 0,
        }
        return run_id

    async def complete_refresh_run(
        self,
        run_id: int,
        status: str,
        *,
        added: int = 0,
        updated: int = 0,
        disabled: int = 0,
        deprecated: int = 0,
        error_detail: str | None = None,
    ) -> None:
        self.completed.append((run_id, status))
        if run_id in self._runs:
            self._runs[run_id].update(
                status=status,
                instruments_added=added,
                instruments_updated=updated,
                instruments_disabled=disabled,
                error_detail=error_detail,
            )

    async def get_refresh_run(self, run_id: int) -> dict | None:
        return self._runs.get(run_id)

    async def find_running_run_for_venue(self, venue: str) -> dict | None:
        for row in self._runs.values():
            if row["venue"] == venue and row["status"] == "running":
                return row
        return None

    async def get_instruments_by_venue(self, venue: str) -> list[dict]:
        return []

    def acquire(self):
        return _DummyConn()


class _DummyConn:
    async def __aenter__(self):
        return self

    async def __aexit__(self, *args):
        return False

    def transaction(self):
        return self

    async def upsert_instruments(self, *args: Any) -> None:
        return None


@pytest.fixture
def repo() -> FakeRepo:
    return FakeRepo()


@pytest.fixture
def venue_configs() -> dict[str, dict]:
    return {"fakevenue": {}}


def _patch_loader(monkeypatch, loader):
    """Replace _resolve_loader so the coordinator returns our FakeLoader."""
    from zk_refdata_svc import coordinator as coord_mod

    monkeypatch.setattr(coord_mod, "_resolve_loader", lambda venue, cfg: loader)


@pytest.mark.asyncio
async def test_trigger_manual_returns_run_id(monkeypatch, repo, venue_configs):
    loader = FakeLoader()
    _patch_loader(monkeypatch, loader)
    coord = RefreshCoordinator(repo=repo, nc=None, venue_configs=venue_configs)

    run_id = await coord.trigger_manual("fakevenue")
    # Wait for the spawned task to finish so we don't leak it across tests.
    await asyncio.sleep(0)
    while coord.is_locked("fakevenue"):
        await asyncio.sleep(0.01)

    assert isinstance(run_id, int) and run_id > 0
    assert repo.inserted == [("fakevenue", "fakevenue")]
    assert loader.load_calls == 1


@pytest.mark.asyncio
async def test_trigger_manual_rejects_when_already_running(
    monkeypatch, repo, venue_configs
):
    hold = asyncio.Event()
    loader = FakeLoader(hold=hold)
    _patch_loader(monkeypatch, loader)
    coord = RefreshCoordinator(repo=repo, nc=None, venue_configs=venue_configs)

    first = await coord.trigger_manual("fakevenue")
    # Yield once so the spawned task acquires the lock and starts loading.
    await asyncio.sleep(0)

    with pytest.raises(RefreshAlreadyInProgress) as ei:
        await coord.trigger_manual("fakevenue")
    assert ei.value.venue == "fakevenue"
    assert ei.value.active_run_id == first

    # Release the held loader so the lock clears for cleanup.
    hold.set()
    while coord.is_locked("fakevenue"):
        await asyncio.sleep(0.01)


@pytest.mark.asyncio
async def test_trigger_manual_unknown_venue_raises(monkeypatch, repo, venue_configs):
    from zk_refdata_svc import coordinator as coord_mod

    monkeypatch.setattr(coord_mod, "_resolve_loader", lambda venue, cfg: None)
    coord = RefreshCoordinator(repo=repo, nc=None, venue_configs={"x": {}})

    with pytest.raises(UnknownVenue):
        await coord.trigger_manual("x")


@pytest.mark.asyncio
async def test_periodic_serializes_with_manual(monkeypatch, repo, venue_configs):
    """Periodic loop must wait behind a held manual run rather than overlap."""
    hold = asyncio.Event()
    loader = FakeLoader(hold=hold)
    _patch_loader(monkeypatch, loader)
    coord = RefreshCoordinator(repo=repo, nc=None, venue_configs=venue_configs)

    await coord.trigger_manual("fakevenue")
    await asyncio.sleep(0)

    periodic_task = asyncio.create_task(coord.refresh_all_periodic(["fakevenue"]))
    # Periodic should not have run yet — the manual lock is held.
    await asyncio.sleep(0.05)
    assert loader.load_calls == 1

    # Release manual; periodic should now drain.
    hold.set()
    await asyncio.wait_for(periodic_task, timeout=1.0)
    assert loader.load_calls == 2
