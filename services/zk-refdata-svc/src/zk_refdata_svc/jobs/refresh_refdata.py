"""Instrument refdata refresh job.

Fetches instruments from configured venue loaders, diffs against
PostgreSQL state, commits changes, and publishes NATS events.
"""

from __future__ import annotations

import time

import nats
from loguru import logger

from zk_refdata_svc.lifecycle.diff import DiffResult, check_duplicates, diff_instruments
from zk_refdata_svc.normalize.instruments import normalize_instrument
from zk_refdata_svc.jobs.publish_changes import publish_change_events
from zk_refdata_svc.repo import RefdataRepo


async def refresh_refdata(
    repo: RefdataRepo,
    nc: nats.NATS,
    venues: list[str],
) -> None:
    """Run one refresh cycle for all configured venues."""
    # Lazy import to avoid circular dependency at module level.
    from zk_refdata_svc.loaders import VENUE_LOADERS

    for venue_name in venues:
        loader_cls = VENUE_LOADERS.get(venue_name)
        if loader_cls is None:
            logger.warning(f"no loader registered for venue {venue_name!r}, skipping")
            continue
        try:
            await _refresh_venue(repo, nc, venue_name, loader_cls)
        except Exception:
            logger.exception(f"refresh failed for venue {venue_name}")


async def _refresh_venue(
    repo: RefdataRepo,
    nc: nats.NATS,
    venue_name: str,
    loader_cls: type,
) -> None:
    run_id = await repo.insert_refresh_run(source_name=venue_name, venue=venue_name)
    logger.info(f"[{venue_name}] refresh run {run_id} started")

    try:
        loader = loader_cls()
        raw_records = await loader.load_instruments()
        records = [normalize_instrument(r) for r in raw_records]

        # Conflict detection: reject batch with duplicate instrument_ids.
        check_duplicates(records)

        fetched_map = {r["instrument_id"]: r for r in records}

        # Load current state for this venue.
        current_rows = await repo.get_instruments_by_venue(venue_name)
        current_map = {r["instrument_id"]: r for r in current_rows}

        diff: DiffResult = diff_instruments(current_map, fetched_map)

        logger.info(
            f"[{venue_name}] diff: "
            f"+{len(diff.added)} ~{len(diff.changed)} -{len(diff.disabled)}"
        )

        watermark_ms = int(time.time() * 1000)

        # Build change event records for post-commit publishing.
        change_events: list[dict] = []
        for rec in diff.added:
            rec["lifecycle_status"] = "active"
            change_events.append({
                "run_id": run_id,
                "instrument_id": rec["instrument_id"],
                "venue": venue_name,
                "change_class": "added",
                "watermark_ms": watermark_ms,
            })
        for rec in diff.changed:
            rec["lifecycle_status"] = "active"
            change_events.append({
                "run_id": run_id,
                "instrument_id": rec["instrument_id"],
                "venue": venue_name,
                "change_class": "changed",
                "watermark_ms": watermark_ms,
            })
        for iid in diff.disabled:
            change_events.append({
                "run_id": run_id,
                "instrument_id": iid,
                "venue": venue_name,
                "change_class": "disabled",
                "watermark_ms": watermark_ms,
            })

        # Commit all changes in a single transaction.
        async with repo.acquire() as conn:
            async with conn.transaction():
                all_upserts = diff.added + diff.changed
                if all_upserts:
                    await repo.upsert_instruments(all_upserts, run_id, conn)
                if diff.disabled:
                    await repo.disable_instruments(diff.disabled, run_id, conn)
                if change_events:
                    await repo.insert_change_events(change_events, conn)

        await repo.complete_refresh_run(
            run_id,
            "completed",
            added=len(diff.added),
            updated=len(diff.changed),
            disabled=len(diff.disabled),
        )

        # Post-commit: publish NATS change events.
        if change_events:
            await publish_change_events(nc, change_events)

        logger.info(f"[{venue_name}] refresh run {run_id} completed")

    except Exception as exc:
        await repo.complete_refresh_run(
            run_id, "failed", error_detail=str(exc)
        )
        raise
