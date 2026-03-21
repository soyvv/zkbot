"""zk-refdata-svc entrypoint.

Starts gRPC server, applies migrations, registers in NATS KV with CAS
heartbeat, and runs periodic venue refresh jobs.
"""

import asyncio
import pathlib
import sys

import asyncpg
import grpc.aio
import nats
from loguru import logger

from zk_refdata_svc import refdata_pb2_grpc
from zk_refdata_svc.config import Config
from zk_refdata_svc.jobs.refresh_refdata import refresh_refdata
from zk_refdata_svc.jobs.refresh_sessions import refresh_sessions
from zk_refdata_svc.jobs.scheduler import run_periodic
from zk_refdata_svc.registry import ServiceRegistry
from zk_refdata_svc.repo import RefdataRepo
from zk_refdata_svc.service import RefdataServicer

_MIGRATIONS_DIR = pathlib.Path(__file__).resolve().parent.parent.parent / "migrations"


async def _apply_migrations(repo: RefdataRepo) -> None:
    """Apply all SQL migration files in order."""
    if not _MIGRATIONS_DIR.is_dir():
        logger.debug("no migrations directory found, skipping")
        return
    for sql_file in sorted(_MIGRATIONS_DIR.glob("*.sql")):
        logger.info(f"applying migration {sql_file.name}")
        sql = sql_file.read_text()
        await repo.run_migration(sql)


async def _main() -> None:
    cfg = Config()

    logger.info(f"zk-refdata-svc starting | id={cfg.logical_id} port={cfg.grpc_port}")

    # -- PostgreSQL -----------------------------------------------------------
    pg_pool = await asyncpg.create_pool(cfg.pg_url, min_size=2, max_size=10)
    logger.info("postgres pool connected")

    repo = RefdataRepo(pg_pool)

    # -- Migrations -----------------------------------------------------------
    await _apply_migrations(repo)

    # -- NATS -----------------------------------------------------------------
    nc = await nats.connect(cfg.nats_url)
    logger.info(f"nats connected to {cfg.nats_url}")

    # -- gRPC server ----------------------------------------------------------
    servicer = RefdataServicer(repo)
    server = grpc.aio.server()
    refdata_pb2_grpc.add_RefdataServiceServicer_to_server(servicer, server)
    server.add_insecure_port(f"0.0.0.0:{cfg.grpc_port}")
    await server.start()
    logger.info(f"gRPC server listening on :{cfg.grpc_port}")

    # -- KV registration with CAS heartbeat -----------------------------------
    registry = ServiceRegistry(
        service_type="refdata",
        service_id=cfg.logical_id,
        endpoint=f"{cfg.advertise_host}:{cfg.grpc_port}",
        capabilities=[
            "query_instrument_refdata",
            "query_refdata_by_venue_symbol",
            "list_instruments",
            "query_refdata_watermark",
            "query_market_status",
            "query_market_calendar",
        ],
        heartbeat_interval_s=cfg.heartbeat_interval_s,
        lease_ttl_s=cfg.lease_ttl_s,
    )
    await registry.start(nc)

    # -- Initial refresh + periodic jobs --------------------------------------
    background_tasks: list[asyncio.Task] = []

    if cfg.venues:
        logger.info(f"configured venues: {cfg.venues}")
        # Run initial refresh before entering steady-state.
        try:
            await refresh_refdata(repo, nc, cfg.venues, venue_configs=cfg.venue_configs)
        except Exception:
            logger.exception("initial refdata refresh failed (will retry on schedule)")

        background_tasks.append(
            asyncio.create_task(
                run_periodic(
                    cfg.refresh_interval_s,
                    refresh_refdata,
                    repo,
                    nc,
                    cfg.venues,
                    venue_configs=cfg.venue_configs,
                )
            )
        )
        background_tasks.append(
            asyncio.create_task(
                run_periodic(
                    cfg.refresh_interval_s,
                    refresh_sessions,
                    repo,
                    cfg.venues,
                    venue_configs=cfg.venue_configs,
                )
            )
        )
    else:
        logger.info("no venues configured, refresh jobs disabled")

    # -- Wait for termination or fencing --------------------------------------
    fenced_event = registry.wait_fenced()

    try:
        server_task = asyncio.create_task(server.wait_for_termination())
        fence_task = asyncio.create_task(fenced_event.wait())
        done, _ = await asyncio.wait(
            [server_task, fence_task], return_when=asyncio.FIRST_COMPLETED
        )
        if fence_task in done:
            logger.warning("fenced by another instance, shutting down")
    finally:
        for t in background_tasks:
            t.cancel()
        for t in background_tasks:
            try:
                await t
            except asyncio.CancelledError:
                pass
        await registry.deregister()
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
