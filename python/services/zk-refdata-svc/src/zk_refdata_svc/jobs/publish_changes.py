"""Publish refdata change events to NATS after a refresh commit."""

from __future__ import annotations

import json
import time

import nats
from loguru import logger

REFDATA_UPDATED_SUBJECT = "zk.control.refdata.updated"


async def publish_change_events(nc: nats.NATS, events: list[dict]) -> None:
    """Publish one NATS message per change event."""
    for evt in events:
        change_class = evt["change_class"]
        # Map lifecycle change classes to invalidation signals.
        if change_class in ("added", "changed"):
            signal = "invalidate"
        else:
            signal = "deprecate"

        payload = json.dumps({
            "event_type": "refdata_updated",
            "instrument_id": evt["instrument_id"],
            "venue": evt["venue"],
            "change_class": signal,
            "watermark": evt["watermark_ms"],
            "updated_at_ms": int(time.time() * 1000),
        }).encode()

        try:
            await nc.publish(REFDATA_UPDATED_SUBJECT, payload)
        except Exception:
            logger.exception(f"failed to publish change event for {evt['instrument_id']}")

    if events:
        logger.info(f"published {len(events)} refdata change events")
