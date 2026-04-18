"""Database helpers for zk-pilot (asyncpg)."""

import hashlib
import json
import secrets
from datetime import datetime, timezone
from uuid import uuid4

import asyncpg


def _sha256_hex(value: str) -> str:
    return hashlib.sha256(value.encode()).hexdigest()


# ── Token validation ───────────────────────────────────────────────────────────


async def validate_token(
    conn: asyncpg.Connection,
    token: str,
    logical_id: str,
    instance_type: str,
    env: str,
) -> str | None:
    """Return token_jti if valid and active for the given identity + env, else None."""
    row = await conn.fetchrow(
        """
        SELECT t.token_jti
        FROM cfg.instance_token t
        JOIN cfg.logical_instance li ON li.logical_id = t.logical_id
        WHERE t.token_hash = $1
          AND t.status = 'active'
          AND t.expires_at > now()
          AND t.logical_id = $2
          AND t.instance_type = $3
          AND li.env = $4
        """,
        _sha256_hex(token),
        logical_id,
        instance_type,
        env,
    )
    return row["token_jti"] if row else None


# ── Runtime config loading ────────────────────────────────────────────────────


async def load_refdata_venue_configs(
    conn: asyncpg.Connection,
    env: str,
) -> list[dict]:
    """Load all enabled refdata_venue_instance rows for an env."""
    rows = await conn.fetch(
        "SELECT venue, enabled, provided_config "
        "FROM cfg.refdata_venue_instance WHERE env = $1 AND enabled = true",
        env,
    )
    return [
        {
            "venue": r["venue"],
            "enabled": r["enabled"],
            "config": json.loads(r["provided_config"])
            if isinstance(r["provided_config"], str)
            else dict(r["provided_config"]),
        }
        for r in rows
    ]


# ── Session management ─────────────────────────────────────────────────────────


async def find_session_for_logical(
    conn: asyncpg.Connection,
    logical_id: str,
    instance_type: str,
) -> dict | None:
    """Return the active session row for (logical_id, instance_type), or None.

    Does NOT check expires_at — KV liveness is the authority.
    Caller must check reconciler.is_kv_live(row['kv_key']) before acting on the result.
    """
    row = await conn.fetchrow(
        """
        SELECT owner_session_id, kv_key, instance_type
        FROM mon.active_session
        WHERE logical_id = $1 AND instance_type = $2 AND status = 'active'
        LIMIT 1
        """,
        logical_id,
        instance_type,
    )
    return dict(row) if row else None


async def get_session(
    conn: asyncpg.Connection,
    owner_session_id: str,
) -> dict | None:
    """Return session metadata for deregister/lease-release lookups."""
    row = await conn.fetchrow(
        "SELECT logical_id, instance_type FROM mon.active_session "
        "WHERE owner_session_id = $1",
        owner_session_id,
    )
    return dict(row) if row else None


async def record_session(
    conn: asyncpg.Connection,
    session_id: str,
    logical_id: str,
    instance_type: str,
    kv_key: str,
    lock_key: str,
    lease_ttl_ms: int,
) -> None:
    expires_at = datetime.now(timezone.utc).timestamp() + lease_ttl_ms / 1000.0
    expires_dt = datetime.fromtimestamp(expires_at, tz=timezone.utc)

    await conn.execute(
        """
        INSERT INTO mon.active_session
          (owner_session_id, logical_id, instance_type, kv_key, lock_key,
           lease_ttl_ms, status, expires_at)
        VALUES ($1, $2, $3, $4, $5, $6, 'active', $7)
        ON CONFLICT (owner_session_id) DO NOTHING
        """,
        session_id,
        logical_id,
        instance_type,
        kv_key,
        lock_key,
        lease_ttl_ms,
        expires_dt,
    )


async def deregister_session(
    conn: asyncpg.Connection,
    owner_session_id: str,
) -> None:
    await conn.execute(
        """
        UPDATE mon.active_session
        SET status = 'deregistered', last_seen_at = now()
        WHERE owner_session_id = $1
        """,
        owner_session_id,
    )


async def fence_session(
    conn: asyncpg.Connection,
    owner_session_id: str,
) -> None:
    """Mark session as fenced (stale writer, superseded by new registration)."""
    await conn.execute(
        "UPDATE mon.active_session SET status = 'fenced', last_seen_at = now() "
        "WHERE owner_session_id = $1",
        owner_session_id,
    )


# ── Instance ID lease ──────────────────────────────────────────────────────────


async def acquire_instance_id(
    conn: asyncpg.Connection,
    env: str,
    logical_id: str,
    lease_ttl_minutes: int,
) -> int | None:
    """
    Find and lease the lowest available Snowflake worker ID (0–1023) for the
    given logical_id. Returns None if all IDs are taken.

    Uses a serializable transaction to avoid races.
    """
    async with conn.transaction(isolation="serializable"):
        # Release any expired leases for this logical_id first.
        await conn.execute(
            """
            DELETE FROM cfg.instance_id_lease
            WHERE env = $1 AND logical_id = $2 AND leased_until < now()
            """,
            env,
            logical_id,
        )

        # Find the lowest available instance_id not currently leased.
        row = await conn.fetchrow(
            """
            SELECT s.id AS instance_id
            FROM generate_series(0, 1023) AS s(id)
            WHERE NOT EXISTS (
                SELECT 1 FROM cfg.instance_id_lease
                WHERE env = $1 AND instance_id = s.id AND leased_until >= now()
            )
            ORDER BY s.id
            LIMIT 1
            """,
            env,
        )
        if row is None:
            return None

        instance_id = row["instance_id"]
        await conn.execute(
            """
            INSERT INTO cfg.instance_id_lease (env, instance_id, logical_id, leased_until)
            VALUES ($1, $2, $3, now() + $4 * interval '1 minute')
            ON CONFLICT (env, instance_id)
            DO UPDATE SET logical_id = EXCLUDED.logical_id,
                          leased_until = EXCLUDED.leased_until
            """,
            env,
            instance_id,
            logical_id,
            lease_ttl_minutes,
        )
        return instance_id


async def release_instance_id(
    conn: asyncpg.Connection,
    env: str,
    logical_id: str,
) -> None:
    await conn.execute(
        """
        DELETE FROM cfg.instance_id_lease
        WHERE env = $1 AND logical_id = $2
        """,
        env,
        logical_id,
    )


# ── Admin helpers ──────────────────────────────────────────────────────────────


async def create_logical_instance(
    conn: asyncpg.Connection,
    logical_id: str,
    instance_type: str,
    env: str,
    metadata: dict,
) -> dict:
    row = await conn.fetchrow(
        """
        INSERT INTO cfg.logical_instance (logical_id, instance_type, env, metadata)
        VALUES ($1, $2, $3, $4::jsonb)
        RETURNING logical_id, instance_type, env, created_at
        """,
        logical_id,
        instance_type,
        env,
        json.dumps(metadata),
    )
    return dict(row)


async def list_logical_instances(conn: asyncpg.Connection) -> list[dict]:
    rows = await conn.fetch(
        """
        SELECT logical_id, instance_type, env, enabled, created_at, updated_at
        FROM cfg.logical_instance
        ORDER BY created_at DESC
        """
    )
    return [dict(r) for r in rows]


async def generate_token(
    conn: asyncpg.Connection,
    logical_id: str,
    instance_type: str,
    env: str,
    expires_days: int = 365,
) -> dict:
    """
    Generate a new token, store its hash, and return the plaintext token once.
    The plaintext token is NOT stored — distribute to deployment out-of-band.
    """
    token = secrets.token_hex(32)
    jti = str(uuid4())
    token_hash = _sha256_hex(token)

    row = await conn.fetchrow(
        """
        INSERT INTO cfg.instance_token
          (token_jti, logical_id, instance_type, token_hash, status, expires_at)
        VALUES ($1, $2, $3, $4, 'active', now() + $5 * interval '1 day')
        RETURNING token_jti, expires_at
        """,
        jti,
        logical_id,
        instance_type,
        token_hash,
        expires_days,
    )
    return {
        "token_jti": row["token_jti"],
        "token": token,  # shown once
        "expires_at": row["expires_at"].isoformat(),
    }


async def list_active_sessions(conn: asyncpg.Connection) -> list[dict]:
    rows = await conn.fetch(
        """
        SELECT owner_session_id, logical_id, instance_type, kv_key,
               status, last_seen_at, expires_at
        FROM mon.active_session
        WHERE status = 'active'
        ORDER BY last_seen_at DESC
        """
    )
    return [dict(r) for r in rows]


async def force_deregister_session(
    conn: asyncpg.Connection,
    owner_session_id: str,
) -> bool:
    result = await conn.execute(
        """
        UPDATE mon.active_session
        SET status = 'force_deregistered', last_seen_at = now()
        WHERE owner_session_id = $1 AND status = 'active'
        """,
        owner_session_id,
    )
    return result == "UPDATE 1"
