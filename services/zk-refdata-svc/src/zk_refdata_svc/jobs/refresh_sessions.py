"""Market session refresh job (stub for crypto venues).

Crypto venues are 24/7 so this stub simply ensures each known venue
has session_state='open' in cfg.market_session_state.  Real TradFi
session/calendar handling is deferred to when Oanda/IBKR adaptors
are added.
"""

from __future__ import annotations

from loguru import logger

from zk_refdata_svc.repo import RefdataRepo


async def refresh_sessions(repo: RefdataRepo, venues: list[str]) -> None:
    """Upsert session_state='open' for all configured crypto venues."""
    for venue in venues:
        try:
            await repo.upsert_market_session_state(venue, "default", "open")
        except Exception:
            logger.exception(f"session refresh failed for {venue}")
    if venues:
        logger.debug(f"session refresh complete for {len(venues)} venues")
