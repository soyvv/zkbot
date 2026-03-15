"""NATS KV registration for zk-refdata-svc."""

from __future__ import annotations

import json
import time

import nats
import nats.js
from loguru import logger

REGISTRY_BUCKET = "zk-svc-registry-v1"


async def register(nc: nats.NATS, logical_id: str, grpc_port: int) -> None:
    """Register the refdata service in NATS KV."""
    js = nc.jetstream()

    try:
        kv = await js.key_value(REGISTRY_BUCKET)
    except Exception:
        from nats.js.api import KeyValueConfig

        kv = await js.create_key_value(KeyValueConfig(bucket=REGISTRY_BUCKET, ttl=60))
        logger.info(f"created registry KV bucket '{REGISTRY_BUCKET}'")

    payload = json.dumps(
        {
            "service_type": "refdata",
            "logical_id": logical_id,
            "grpc_endpoint": f"grpc://0.0.0.0:{grpc_port}",
            "capabilities": [
                "query_instrument_refdata",
                "query_refdata_by_venue_symbol",
                "list_instruments",
                "query_refdata_watermark",
                "query_market_status",
            ],
            "registered_at_ms": int(time.time() * 1000),
        }
    ).encode()

    key = f"svc.refdata.{logical_id}"
    await kv.put(key, payload)
    logger.info(f"registered in KV: {key}")
