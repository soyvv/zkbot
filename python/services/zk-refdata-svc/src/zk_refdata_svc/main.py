"""zk-refdata-svc entrypoint.

Startup sequence (direct mode):
1. Load bootstrap config from env
2. Assemble runtime config (provided configs from JSON file + secret resolution)
3. Initialize infra clients (PG, NATS, gRPC)
4. Run initial refresh for enabled venues
5. Register readiness (NATS KV) — only after step 4 succeeds
6. Enter steady state

Startup sequence (pilot mode):
1. Load bootstrap config from env
2. Connect NATS (needed for bootstrap RPC)
3. Bootstrap register with Pilot → get session grant + venue configs
4. Assemble runtime config (provided configs from Pilot + secret resolution)
5. Initialize remaining infra (PG, gRPC)
6. Run initial refresh for enabled venues
7. Register readiness (NATS KV using Pilot-assigned kv_key)
8. Enter steady state
"""

import asyncio
import pathlib
import sys

import asyncpg
import grpc.aio
import nats
from loguru import logger

from zk_refdata_svc import refdata_pb2_grpc
from zk_refdata_svc.config import load_config
from zk_refdata_svc.config_loader import ConfigAssemblyError
from zk_refdata_svc.config_model import RefdataBootstrapConfig
from zk_refdata_svc.jobs.refresh_sessions import refresh_sessions
from zk_refdata_svc.jobs.scheduler import run_periodic
from zk_refdata_svc.registry import ServiceRegistry
from zk_refdata_svc.repo import RefdataRepo
from zk_refdata_svc.coordinator import RefreshCoordinator
from zk_refdata_svc.service import RefdataServicer

_MIGRATIONS_DIR = pathlib.Path(__file__).resolve().parent.parent.parent / "migrations"


async def _initial_refresh_then_periodic(coordinator, cfg, enabled: list[str]) -> None:
    """Run the initial refresh, then continue with the periodic schedule.

    Decoupled from the registry-readiness gate (zb-00036) so Pilot's discovery
    sees the service as soon as the gRPC server is listening. Catches the
    initial-refresh exception so the periodic loop still starts.
    """
    try:
        await coordinator.refresh_all_periodic(enabled)
    except Exception:
        logger.exception("initial refdata refresh failed (will retry on schedule)")
    await run_periodic(
        cfg.refresh_interval_s,
        coordinator.refresh_all_periodic,
        enabled,
    )


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
    bootstrap = RefdataBootstrapConfig.from_env()
    grant = None
    nc = None
    kv_key_override = None

    if bootstrap.mode == "pilot":
        # -- Pilot mode: connect NATS first for bootstrap RPC --
        from zk_refdata_svc.bootstrap_client import (
            BootstrapError,
            bootstrap_register,
            bootstrap_deregister,
        )

        if not bootstrap.bootstrap_token:
            logger.error("ZK_BOOTSTRAP_TOKEN required for pilot mode")
            sys.exit(1)

        nc = await nats.connect(bootstrap.nats_url)
        logger.info(f"nats connected to {bootstrap.nats_url} (for bootstrap)")

        try:
            grant = await bootstrap_register(
                nc,
                token=bootstrap.bootstrap_token,
                logical_id=bootstrap.logical_id,
                env=bootstrap.env,
            )
        except BootstrapError as exc:
            logger.error(f"bootstrap registration failed: {exc}")
            await nc.drain()
            sys.exit(1)

        kv_key_override = grant.kv_key

        # Assemble runtime config using venue configs from Pilot
        try:
            cfg = load_config(bootstrap_runtime_config=grant.runtime_config)
        except (ConfigAssemblyError, KeyError) as exc:
            logger.error(f"config assembly failed: {exc}")
            await bootstrap_deregister(
                nc, grant.owner_session_id, bootstrap.logical_id, bootstrap.env
            )
            await nc.drain()
            sys.exit(1)
    else:
        # -- Direct mode: config assembly first --
        try:
            cfg = load_config()
        except (ConfigAssemblyError, KeyError) as exc:
            logger.error(f"config assembly failed, refusing to start: {exc}")
            sys.exit(1)

    logger.info(f"zk-refdata-svc starting | id={cfg.logical_id} port={cfg.grpc_port}")
    logger.debug(f"effective config (redacted): {cfg.redacted_dict()}")

    # -- Initialize infra clients -----------------------------------------------
    pg_pool = await asyncpg.create_pool(cfg.pg_url, min_size=2, max_size=10)
    logger.info("postgres pool connected")

    repo = RefdataRepo(pg_pool)
    await _apply_migrations(repo)

    if nc is None:
        nc = await nats.connect(cfg.nats_url)
        logger.info(f"nats connected to {cfg.nats_url}")

    # -- Initial refresh for enabled venues -------------------------------------
    background_tasks: list[asyncio.Task] = []
    enabled = cfg.enabled_venues()

    # Build venue_configs dict from runtime config (resolved secrets)
    venue_configs = (
        {v: cfg.venue_resolved_config(v) for v in enabled} if enabled else {}
    )

    # Single coordinator owns per-venue locks, shared between periodic loop and
    # the operator-triggered TriggerVenueRefresh RPC.
    coordinator = RefreshCoordinator(repo, nc, venue_configs)

    # gRPC server
    servicer = RefdataServicer(repo, coordinator=coordinator)
    server = grpc.aio.server()
    refdata_pb2_grpc.add_RefdataServiceServicer_to_server(servicer, server)
    server.add_insecure_port(f"0.0.0.0:{cfg.grpc_port}")
    await server.start()
    logger.info(f"gRPC server listening on :{cfg.grpc_port}")

    # -- Register readiness AS SOON AS gRPC is listening -----------------------
    # Pilot's discovery cache + the operator-triggered TriggerVenueRefresh RPC
    # depend on the service being visible in NATS KV under svc.refdata.<id>.
    # Holding registration until the initial refresh finishes blocks the Pilot
    # proxy for ~50 minutes after each restart on IBKR-scale universes
    # (~11k contracts at ~2/sec). Register early; consumers can rely on
    # gRPC reachability and use the existing refresh-run state for "is the
    # data fresh" semantics. (zb-00036)
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
        kv_key=kv_key_override,
    )
    await registry.start(nc)
    logger.info("service registered in NATS KV (ready) — initial refresh runs in background")

    if enabled:
        logger.info(f"configured venues: {enabled}")

        # Run the initial refresh as a background task instead of awaiting it
        # inline. The previous DB row state remains queryable while the new
        # run proceeds; consumers that need fresh-data semantics can poll the
        # cfg.refdata_refresh_run row via GetRefreshRun.
        background_tasks.append(
            asyncio.create_task(_initial_refresh_then_periodic(coordinator, cfg, enabled))
        )
        background_tasks.append(
            asyncio.create_task(
                run_periodic(
                    cfg.refresh_interval_s,
                    refresh_sessions,
                    coordinator,
                    repo,
                    enabled,
                )
            )
        )
    else:
        logger.info("no venues configured, refresh jobs disabled")

    # -- Wait for termination or fencing ----------------------------------------
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
        if grant is not None:
            from zk_refdata_svc.bootstrap_client import bootstrap_deregister

            await bootstrap_deregister(
                nc, grant.owner_session_id, cfg.logical_id, bootstrap.env
            )
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
