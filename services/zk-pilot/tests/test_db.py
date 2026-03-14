"""DB helper unit tests — require a running PostgreSQL (rolls back after each test)."""

import hashlib

import pytest

from zk_pilot import db


# ── Seed helpers ───────────────────────────────────────────────────────────────


def _sha256(s: str) -> str:
    return hashlib.sha256(s.encode()).hexdigest()


async def seed_logical_instance(conn, logical_id: str, instance_type: str, env: str) -> None:
    await conn.execute(
        "INSERT INTO cfg.logical_instance (logical_id, instance_type, env) "
        "VALUES ($1, $2, $3) ON CONFLICT (logical_id) DO NOTHING",
        logical_id,
        instance_type,
        env,
    )


async def seed_token(
    conn, logical_id: str, instance_type: str, token_hash: str, jti: str = "test-jti"
) -> None:
    await conn.execute(
        "INSERT INTO cfg.instance_token "
        "(token_jti, token_hash, logical_id, instance_type, status, expires_at) "
        "VALUES ($1, $2, $3, $4, 'active', now() + interval '1 day')",
        jti,
        token_hash,
        logical_id,
        instance_type,
    )


async def seed_session(
    conn,
    session_id: str,
    logical_id: str,
    instance_type: str,
    kv_key: str,
    status: str = "active",
) -> None:
    await conn.execute(
        "INSERT INTO mon.active_session "
        "(owner_session_id, logical_id, instance_type, kv_key, lock_key, "
        " lease_ttl_ms, status, expires_at) "
        "VALUES ($1, $2, $3, $4, $5, 30000, $6, now() + interval '1 minute')",
        session_id,
        logical_id,
        instance_type,
        kv_key,
        f"lock.{kv_key}",
        status,
    )


# ── Token validation ───────────────────────────────────────────────────────────


async def test_validate_token_accepts_matching_env(conn):
    await seed_logical_instance(conn, "oms_tv1", "OMS", "dev")
    await seed_token(conn, "oms_tv1", "OMS", _sha256("tok1"), jti="jti-tv1")
    jti = await db.validate_token(conn, "tok1", "oms_tv1", "OMS", "dev")
    assert jti == "jti-tv1"


async def test_validate_token_rejects_wrong_env(conn):
    await seed_logical_instance(conn, "oms_tv2", "OMS", "dev")
    await seed_token(conn, "oms_tv2", "OMS", _sha256("tok2"), jti="jti-tv2")
    # Token registered for env=dev but requesting env=prod
    jti = await db.validate_token(conn, "tok2", "oms_tv2", "OMS", "prod")
    assert jti is None


# ── Session management ─────────────────────────────────────────────────────────


async def test_find_session_for_logical_returns_active_row(conn):
    await seed_logical_instance(conn, "oms_fs1", "OMS", "dev")
    # Two sessions for same logical_id: one active, one fenced
    await seed_session(conn, "sid-fs-a", "oms_fs1", "OMS", "svc.oms.oms_fs1", "active")
    await seed_session(conn, "sid-fs-b", "oms_fs1", "OMS", "svc.oms.oms_fs1_old", "fenced")
    row = await db.find_session_for_logical(conn, "oms_fs1", "OMS")
    assert row is not None
    assert row["owner_session_id"] == "sid-fs-a"


async def test_fence_session_marks_row_fenced(conn):
    await seed_logical_instance(conn, "oms_fn1", "OMS", "dev")
    await seed_session(conn, "sid-fn-a", "oms_fn1", "OMS", "svc.oms.oms_fn1")
    await db.fence_session(conn, "sid-fn-a")
    row = await conn.fetchrow(
        "SELECT status FROM mon.active_session WHERE owner_session_id = $1", "sid-fn-a"
    )
    assert row["status"] == "fenced"


async def test_deregister_session_marks_row_deregistered(conn):
    await seed_logical_instance(conn, "oms_dr1", "OMS", "dev")
    await seed_session(conn, "sid-dr-a", "oms_dr1", "OMS", "svc.oms.oms_dr1")
    await db.deregister_session(conn, "sid-dr-a")
    row = await conn.fetchrow(
        "SELECT status FROM mon.active_session WHERE owner_session_id = $1", "sid-dr-a"
    )
    assert row["status"] == "deregistered"


# ── Instance ID lease ──────────────────────────────────────────────────────────


async def test_acquire_instance_id_allocates_unique_ids(conn):
    await seed_logical_instance(conn, "eng_ai1", "ENGINE", "dev")
    await seed_logical_instance(conn, "eng_ai2", "ENGINE", "dev")
    id1 = await db.acquire_instance_id(conn, "dev", "eng_ai1", lease_ttl_minutes=5)
    id2 = await db.acquire_instance_id(conn, "dev", "eng_ai2", lease_ttl_minutes=5)
    assert id1 is not None and id2 is not None
    assert id1 != id2


async def test_release_instance_id_removes_lease(conn):
    await seed_logical_instance(conn, "eng_rl1", "ENGINE", "dev")
    id_ = await db.acquire_instance_id(conn, "dev", "eng_rl1", lease_ttl_minutes=5)
    assert id_ is not None
    await db.release_instance_id(conn, "dev", "eng_rl1")
    row = await conn.fetchrow(
        "SELECT 1 FROM cfg.instance_id_lease WHERE logical_id = $1", "eng_rl1"
    )
    assert row is None
