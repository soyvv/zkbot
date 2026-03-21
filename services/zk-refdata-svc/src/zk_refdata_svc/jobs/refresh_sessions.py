"""Market session refresh job.

For crypto venues (supports_tradfi_sessions=false), sets session_state='open'.
For TradFi venues, calls the adaptor's load_market_sessions() if available.
"""

from __future__ import annotations

from loguru import logger

from zk_refdata_svc.repo import RefdataRepo


async def refresh_sessions(
    repo: RefdataRepo,
    venues: list[str],
    venue_configs: dict[str, dict] | None = None,
) -> None:
    """Refresh market session state for all configured venues."""
    for venue in venues:
        try:
            await _refresh_venue_sessions(repo, venue, (venue_configs or {}).get(venue))
        except Exception:
            logger.exception(f"session refresh failed for {venue}")
    if venues:
        logger.debug(f"session refresh complete for {len(venues)} venues")


async def _refresh_venue_sessions(
    repo: RefdataRepo, venue: str, config: dict | None
) -> None:
    # Try to load manifest and check if venue has TradFi sessions.
    supports_sessions = False
    try:
        from zk_refdata_svc.venue_registry import load_manifest, manifest_supports_sessions

        manifest = load_manifest(venue)
        supports_sessions = manifest_supports_sessions(manifest)
    except FileNotFoundError:
        pass  # No manifest — treat as crypto (default-open)

    if supports_sessions:
        # TradFi venue: call adaptor's load_market_sessions()
        try:
            from zk_refdata_svc.venue_registry import resolve_refdata_loader

            loader = resolve_refdata_loader(venue, config)
            sessions = await loader.load_market_sessions()
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
