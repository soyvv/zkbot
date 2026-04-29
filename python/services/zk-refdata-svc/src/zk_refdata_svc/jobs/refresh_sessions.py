"""Market session refresh job.

For crypto venues (supports_tradfi_sessions=false), sets session_state='open'.
For TradFi venues, calls the adaptor's load_market_sessions() through the
RefreshCoordinator so it shares the cached loader (and its IBKR connection)
with refresh_refdata, and is serialized under the same per-venue lock
(zb-00035 — previously a fresh loader instance per tick raced the main
refresh_refdata loader for the same client_id and intermittently failed).
"""

from __future__ import annotations

from typing import Any

from loguru import logger

from zk_refdata_svc.repo import RefdataRepo


async def refresh_sessions(
    coordinator: Any,
    repo: RefdataRepo,
    venues: list[str],
) -> None:
    """Refresh market session state for all configured venues."""
    for venue in venues:
        try:
            await _refresh_venue_sessions(coordinator, repo, venue)
        except Exception:
            logger.exception(f"session refresh failed for {venue}")
    if venues:
        logger.debug(f"session refresh complete for {len(venues)} venues")


async def _refresh_venue_sessions(coordinator: Any, repo: RefdataRepo, venue: str) -> None:
    # Try to load manifest and check if venue has TradFi sessions.
    supports_sessions = False
    try:
        from zk_refdata_svc.venue_registry import load_manifest, manifest_supports_sessions

        manifest = load_manifest(venue)
        supports_sessions = manifest_supports_sessions(manifest)
    except FileNotFoundError:
        pass  # No manifest — treat as crypto (default-open)

    if supports_sessions:
        # TradFi venue: call adaptor's load_market_sessions() via coordinator
        # so the call shares the loader cache + per-venue lock with the
        # refresh_refdata path.
        try:
            sessions = await coordinator.load_market_sessions_for(venue)
            if sessions is None:
                logger.debug(f"[{venue}] no loader available; skipping sessions")
                return
            for s in sessions:
                await repo.upsert_market_session_state(
                    s["venue"], s["market"], s["session_state"]
                )
            if sessions:
                logger.debug(f"[{venue}] wrote {len(sessions)} session entries from adaptor")
            else:
                logger.debug(f"[{venue}] TradFi venue returned no session data")
        except Exception:
            logger.exception(f"[{venue}] failed to load sessions from adaptor")
    else:
        # Crypto venue: explicitly default-open
        await repo.upsert_market_session_state(venue.upper(), "default", "open")
