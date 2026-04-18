"""zk-pilot entrypoint — starts FastAPI HTTP server and NATS bootstrap subscriptions."""

import asyncio
import sys

import asyncpg
import nats
import uvicorn
from fastapi import FastAPI
from loguru import logger

from .admin import router as admin_router
from .bootstrap import KvReconciler, REGISTRY_BUCKET, make_handlers
from .config import Config
from .strategy_executions import router as strategy_router


async def _main() -> None:
    cfg = Config()

    logger.info(f"zk-pilot starting | env={cfg.env} id={cfg.pilot_id} port={cfg.http_port}")

    # ── Postgres ─────────────────────────────────────────────────────────────
    pg_pool = await asyncpg.create_pool(cfg.pg_url, min_size=2, max_size=10)
    logger.info("postgres pool connected")

    # ── NATS ─────────────────────────────────────────────────────────────────
    nc = await nats.connect(cfg.nats_url)
    logger.info(f"nats connected to {cfg.nats_url}")

    # ── KV Reconciler ─────────────────────────────────────────────────────────
    # Ensure the registry bucket exists before the reconciler tries to open it.
    # On a fresh environment the bucket may not yet exist, which would cause
    # js.key_value() to fail and leave wait_ready() blocked forever.
    js = nc.jetstream()
    try:
        await js.key_value(REGISTRY_BUCKET)
    except Exception:
        from nats.js.api import KeyValueConfig
        await js.create_key_value(KeyValueConfig(bucket=REGISTRY_BUCKET, ttl=60))
        logger.info(f"created registry KV bucket '{REGISTRY_BUCKET}'")

    # Start before subscribing to bootstrap subjects so the reconciler's initial
    # KV snapshot is loaded before any register requests are served.
    reconciler = KvReconciler(pg_pool, cfg)
    asyncio.create_task(reconciler.run(nc))
    await reconciler.wait_ready()
    logger.info("kv reconciler ready")

    handle_register, handle_deregister = make_handlers(pg_pool, cfg, reconciler)
    await nc.subscribe("zk.bootstrap.register", cb=handle_register)
    await nc.subscribe("zk.bootstrap.deregister", cb=handle_deregister)
    logger.info("nats bootstrap subscriptions active")

    # ── FastAPI ───────────────────────────────────────────────────────────────
    app = FastAPI(title="zk-pilot", version="0.1.0")
    app.state.pg_pool = pg_pool
    app.state.nats = nc
    app.include_router(admin_router)
    app.include_router(strategy_router)

    @app.get("/health")
    async def health():
        return {"status": "ok", "pilot_id": cfg.pilot_id, "env": cfg.env}

    uv_cfg = uvicorn.Config(app, host="0.0.0.0", port=cfg.http_port, log_level="warning")
    server = uvicorn.Server(uv_cfg)

    try:
        await server.serve()
    finally:
        await nc.drain()
        await pg_pool.close()
        logger.info("zk-pilot stopped")


def run() -> None:
    try:
        asyncio.run(_main())
    except KeyboardInterrupt:
        sys.exit(0)
