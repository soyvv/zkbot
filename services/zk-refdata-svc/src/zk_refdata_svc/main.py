"""zk-refdata-svc entrypoint — starts async gRPC server and registers in NATS KV."""

import asyncio
import sys

import asyncpg
import grpc.aio
import nats
from loguru import logger

from zk_refdata_svc import refdata_pb2_grpc
from zk_refdata_svc.config import Config
from zk_refdata_svc.kv import register
from zk_refdata_svc.repo import RefdataRepo
from zk_refdata_svc.service import RefdataServicer


async def _main() -> None:
    cfg = Config()

    logger.info(f"zk-refdata-svc starting | id={cfg.logical_id} port={cfg.grpc_port}")

    # ── PostgreSQL ─────────────────────────────────────────────────────────────
    pg_pool = await asyncpg.create_pool(cfg.pg_url, min_size=2, max_size=10)
    logger.info("postgres pool connected")

    # ── NATS ───────────────────────────────────────────────────────────────────
    nc = await nats.connect(cfg.nats_url)
    logger.info(f"nats connected to {cfg.nats_url}")

    # ── gRPC server ────────────────────────────────────────────────────────────
    repo = RefdataRepo(pg_pool)
    servicer = RefdataServicer(repo)

    server = grpc.aio.server()
    refdata_pb2_grpc.add_RefdataServiceServicer_to_server(servicer, server)
    server.add_insecure_port(f"0.0.0.0:{cfg.grpc_port}")
    await server.start()
    logger.info(f"gRPC server listening on :{cfg.grpc_port}")

    # ── KV registration ────────────────────────────────────────────────────────
    await register(nc, cfg.logical_id, cfg.grpc_port)

    try:
        await server.wait_for_termination()
    finally:
        await server.stop(grace=5)
        await nc.drain()
        await pg_pool.close()
        logger.info("zk-refdata-svc stopped")


def run() -> None:
    try:
        asyncio.run(_main())
    except KeyboardInterrupt:
        sys.exit(0)

if __name__ == "__main__":
    run()
