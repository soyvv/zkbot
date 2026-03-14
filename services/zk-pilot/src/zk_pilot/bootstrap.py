"""NATS bootstrap request handlers and Pilot KV reconciler."""

import asyncio
from uuid import uuid4

import asyncpg
import nats.aio.client
from loguru import logger
from nats.aio.msg import Msg
from nats.js.kv import KeyValueOperation

from .config import Config
from . import db
from .proto import (
    BootstrapDeregisterRequest,
    BootstrapDeregisterResponse,
    BootstrapRegisterRequest,
    BootstrapRegisterResponse,
)

LEASE_TTL_MS = 30_000
REGISTRY_BUCKET = "zk-svc-registry-v1"


# ── KV Reconciler ──────────────────────────────────────────────────────────────


class KvReconciler:
    """Watches the service registry KV bucket and maintains an in-memory live set.

    KV is the sole liveness authority. This reconciler:
    - Maintains `_live`: set of kv_keys currently present in the KV bucket.
    - Sets `_ready` after the initial snapshot is loaded so bootstrap handlers
      can wait before serving requests.
    - Fences stale DB sessions when their KV key disappears (Delete/Purge events,
      or keys that vanished while Pilot was disconnected).
    - Rebuilds `_live` from scratch on every reconnect to avoid stale state.
    """

    def __init__(self, pg_pool: asyncpg.Pool, cfg: Config) -> None:
        self.pg_pool = pg_pool
        self.cfg = cfg
        self._live: set[str] = set()
        self._ready = asyncio.Event()

    def is_kv_live(self, kv_key: str) -> bool:
        return kv_key in self._live

    async def wait_ready(self) -> None:
        await self._ready.wait()

    async def run(self, nc: nats.aio.client.Client) -> None:
        """Watch svc.> and maintain in-memory live set. Rebuilds state on reconnect."""
        while True:
            try:
                js = nc.jetstream()
                kv = await js.key_value(REGISTRY_BUCKET)
                watcher = await kv.watchall()
                next_live: set[str] = set()
                snapshot_done = False

                async for entry in watcher:
                    if entry is None:
                        # Snapshot-complete marker: atomically swap in the new live set.
                        old_live = self._live
                        self._live = next_live
                        if not self._ready.is_set():
                            self._ready.set()
                            logger.info(
                                f"reconciler: initial snapshot loaded "
                                f"({len(self._live)} live keys)"
                            )
                        # Fence any keys that disappeared while Pilot was disconnected.
                        for lost_key in old_live - next_live:
                            await self._on_kv_lost(lost_key)
                        snapshot_done = True
                        continue

                    key = entry.key
                    if entry.operation == KeyValueOperation.PUT:
                        if snapshot_done:
                            self._live.add(key)
                        else:
                            next_live.add(key)
                    else:  # DELETE or PURGE
                        if snapshot_done:
                            self._live.discard(key)
                            await self._on_kv_lost(key)
                        else:
                            next_live.discard(key)

            except Exception as exc:
                logger.warning(f"reconciler: watch error: {exc}, retrying in 5s")
                await asyncio.sleep(5)

    async def _on_kv_lost(self, kv_key: str) -> None:
        """Fence the DB session for a kv_key that is no longer live in KV."""
        try:
            async with self.pg_pool.acquire() as conn:
                row = await conn.fetchrow(
                    "SELECT owner_session_id, logical_id, instance_type "
                    "FROM mon.active_session WHERE kv_key = $1 AND status = 'active'",
                    kv_key,
                )
                if not row:
                    return
                await db.fence_session(conn, row["owner_session_id"])
                if row["instance_type"].upper() == "ENGINE":
                    env_row = await conn.fetchrow(
                        "SELECT env FROM cfg.logical_instance WHERE logical_id = $1",
                        row["logical_id"],
                    )
                    if env_row:
                        await db.release_instance_id(
                            conn, env_row["env"], row["logical_id"]
                        )
            logger.info(
                f"reconciler: fenced session for kv_key={kv_key!r} "
                f"owner={row['owner_session_id']!r}"
            )
        except Exception as exc:
            logger.warning(f"reconciler: error fencing {kv_key!r}: {exc}")


# ── NATS handlers ──────────────────────────────────────────────────────────────


def make_handlers(
    pg_pool: asyncpg.Pool, cfg: Config, reconciler: KvReconciler
):
    """Return NATS message handlers closed over shared resources."""

    async def handle_register(msg: Msg) -> None:
        try:
            req = BootstrapRegisterRequest().parse(msg.data)
        except Exception as exc:
            logger.warning(f"bootstrap: failed to parse RegisterRequest: {exc}")
            return

        logger.info(
            f"bootstrap: register request logical_id={req.logical_id!r} "
            f"instance_type={req.instance_type!r} env={req.env!r}"
        )

        async with pg_pool.acquire() as conn:
            # 1. Validate token (including env check)
            jti = await db.validate_token(
                conn, req.token, req.logical_id, req.instance_type, req.env
            )
            if jti is None:
                resp = BootstrapRegisterResponse(
                    status="TOKEN_EXPIRED",
                    error_message=(
                        "Token invalid, expired, or does not match "
                        "logical_id / instance_type / env"
                    ),
                )
                await msg.respond(bytes(resp))
                return

            # 2. Duplicate check: DB + KV-SOT liveness
            session = await db.find_session_for_logical(
                conn, req.logical_id, req.instance_type
            )
            if session is not None:
                if reconciler.is_kv_live(session["kv_key"]):
                    resp = BootstrapRegisterResponse(
                        status="DUPLICATE",
                        error_message=(
                            f"Active session still live in KV: "
                            f"{session['owner_session_id']}"
                        ),
                    )
                    await msg.respond(bytes(resp))
                    return
                else:
                    # DB says active but KV says gone — fence stale row and continue.
                    await db.fence_session(conn, session["owner_session_id"])
                    logger.info(
                        f"bootstrap: fenced stale session "
                        f"{session['owner_session_id']!r} for {req.logical_id!r} "
                        f"(KV key absent)"
                    )
                    if session["instance_type"].upper() == "ENGINE":
                        env_row = await conn.fetchrow(
                            "SELECT env FROM cfg.logical_instance WHERE logical_id = $1",
                            req.logical_id,
                        )
                        if env_row:
                            await db.release_instance_id(
                                conn, env_row["env"], req.logical_id
                            )

            # 3. Assign instance_id for engine instances
            instance_id = 0
            if req.instance_type.upper() == "ENGINE":
                assigned = await db.acquire_instance_id(
                    conn, req.env, req.logical_id, cfg.lease_ttl_minutes
                )
                if assigned is None:
                    resp = BootstrapRegisterResponse(
                        status="NO_INSTANCE_ID_AVAILABLE",
                        error_message="All Snowflake worker IDs (0–1023) are leased",
                    )
                    await msg.respond(bytes(resp))
                    return
                instance_id = assigned

            # 4. Build grant
            session_id = str(uuid4())
            kv_key = f"svc.{req.instance_type.lower()}.{req.logical_id}"
            lock_key = f"lock.{req.instance_type.lower()}.{req.logical_id}"

            # 5. Record session
            await db.record_session(
                conn,
                session_id,
                req.logical_id,
                req.instance_type,
                kv_key,
                lock_key,
                LEASE_TTL_MS,
            )

        logger.info(
            f"bootstrap: registered logical_id={req.logical_id!r} "
            f"session={session_id} kv_key={kv_key!r} instance_id={instance_id}"
        )

        resp = BootstrapRegisterResponse(
            owner_session_id=session_id,
            kv_key=kv_key,
            lock_key=lock_key,
            lease_ttl_ms=LEASE_TTL_MS,
            instance_id=instance_id,
            scoped_credential="",
            status="OK",
        )
        await msg.respond(bytes(resp))

    async def handle_deregister(msg: Msg) -> None:
        try:
            req = BootstrapDeregisterRequest().parse(msg.data)
        except Exception as exc:
            logger.warning(f"bootstrap: failed to parse DeregisterRequest: {exc}")
            return

        logger.info(f"bootstrap: deregister session={req.owner_session_id!r}")

        async with pg_pool.acquire() as conn:
            # Look up session metadata from DB — Rust sends only owner_session_id.
            session = await db.get_session(conn, req.owner_session_id)
            await db.deregister_session(conn, req.owner_session_id)
            if session and session["instance_type"].upper() == "ENGINE":
                env_row = await conn.fetchrow(
                    "SELECT env FROM cfg.logical_instance WHERE logical_id = $1",
                    session["logical_id"],
                )
                if env_row:
                    await db.release_instance_id(
                        conn, env_row["env"], session["logical_id"]
                    )

        resp = BootstrapDeregisterResponse(success=True)
        await msg.respond(bytes(resp))

    return handle_register, handle_deregister
