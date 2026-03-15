"""Tests for /v1/strategy-executions and /v1/strategies endpoints.

RED phase: all tests must fail before the router is implemented.

Uses a real PostgreSQL connection (rolls back after each test) via the shared
`conn` fixture from conftest.py, and an httpx AsyncClient for HTTP tests.
"""

import uuid

import pytest
from fastapi import FastAPI
from httpx import ASGITransport, AsyncClient


# ── App fixture ────────────────────────────────────────────────────────────────


@pytest.fixture
async def app(pg_pool):
    """Build a minimal FastAPI app with only the strategy-executions router."""
    from zk_pilot.strategy_executions import router

    _app = FastAPI()
    _app.state.pg_pool = pg_pool
    _app.include_router(router)
    return _app


@pytest.fixture
async def client(app):
    async with AsyncClient(transport=ASGITransport(app=app), base_url="http://test") as c:
        yield c


# ── DB seed helpers ────────────────────────────────────────────────────────────


async def _seed_strategy(conn, strategy_id: str = "strat_test") -> None:
    """Ensure cfg.strategy_definition row exists (idempotent)."""
    await conn.execute(
        """
        INSERT INTO cfg.strategy_definition (strategy_id, runtime_type, code_ref)
        VALUES ($1, 'RUST', 'test_ref')
        ON CONFLICT (strategy_id) DO NOTHING
        """,
        strategy_id,
    )


# ── GET /v1/strategies/{strategy_id} ──────────────────────────────────────────


async def test_get_strategy_found(client, conn):
    await _seed_strategy(conn, "strat_test")
    resp = await client.get("/v1/strategies/strat_test")
    assert resp.status_code == 200
    body = resp.json()
    assert body["strategy_id"] == "strat_test"
    assert body["runtime_type"] == "RUST"


async def test_get_strategy_not_found(client):
    resp = await client.get("/v1/strategies/does_not_exist")
    assert resp.status_code == 404


# ── POST /v1/strategy-executions/start ────────────────────────────────────────


async def test_start_execution_returns_execution_id(client, conn):
    await _seed_strategy(conn, "strat_test")
    resp = await client.post(
        "/v1/strategy-executions/start",
        json={"strategy_id": "strat_test", "instance_id": 1, "runtime_params": {}},
    )
    assert resp.status_code == 201
    body = resp.json()
    assert "execution_id" in body
    # Must be a UUID
    uuid.UUID(body["execution_id"])


async def test_start_execution_inserts_pg_row(client, conn):
    await _seed_strategy(conn, "strat_test")
    resp = await client.post(
        "/v1/strategy-executions/start",
        json={"strategy_id": "strat_test", "instance_id": 2, "runtime_params": {}},
    )
    assert resp.status_code == 201
    execution_id = resp.json()["execution_id"]

    row = await conn.fetchrow(
        "SELECT execution_id, strategy_id, status FROM cfg.strategy_instance WHERE execution_id = $1",
        execution_id,
    )
    assert row is not None
    assert row["strategy_id"] == "strat_test"
    assert row["status"] == "running"


async def test_start_execution_returns_enriched_config(client, conn):
    await _seed_strategy(conn, "strat_test")
    resp = await client.post(
        "/v1/strategy-executions/start",
        json={"strategy_id": "strat_test", "instance_id": 1, "runtime_params": {}},
    )
    assert resp.status_code == 201
    body = resp.json()
    assert "enriched_config" in body
    cfg = body["enriched_config"]
    assert "account_ids" in cfg
    assert 9001 in cfg["account_ids"]


async def test_start_execution_unknown_strategy_returns_404(client):
    resp = await client.post(
        "/v1/strategy-executions/start",
        json={"strategy_id": "unknown_strat", "instance_id": 1, "runtime_params": {}},
    )
    assert resp.status_code == 404


# ── POST /v1/strategy-executions/stop ─────────────────────────────────────────


async def test_stop_execution_updates_status(client, conn):
    await _seed_strategy(conn, "strat_test")
    # Start first
    start_resp = await client.post(
        "/v1/strategy-executions/start",
        json={"strategy_id": "strat_test", "instance_id": 1, "runtime_params": {}},
    )
    execution_id = start_resp.json()["execution_id"]

    # Stop
    stop_resp = await client.post(
        "/v1/strategy-executions/stop",
        json={"execution_id": execution_id, "stop_reason": "graceful"},
    )
    assert stop_resp.status_code == 200
    body = stop_resp.json()
    assert body["execution_id"] == execution_id
    assert body["status"] == "stopped"
    assert "stopped_at" in body


async def test_stop_execution_updates_pg_row(client, conn):
    await _seed_strategy(conn, "strat_test")
    start_resp = await client.post(
        "/v1/strategy-executions/start",
        json={"strategy_id": "strat_test", "instance_id": 1, "runtime_params": {}},
    )
    execution_id = start_resp.json()["execution_id"]
    await client.post(
        "/v1/strategy-executions/stop",
        json={"execution_id": execution_id, "stop_reason": "graceful"},
    )

    row = await conn.fetchrow(
        "SELECT status, ended_at FROM cfg.strategy_instance WHERE execution_id = $1",
        execution_id,
    )
    assert row["status"] == "stopped"
    assert row["ended_at"] is not None


async def test_stop_unknown_execution_returns_404(client):
    resp = await client.post(
        "/v1/strategy-executions/stop",
        json={"execution_id": str(uuid.uuid4()), "stop_reason": "graceful"},
    )
    assert resp.status_code == 404


# ── GET /v1/strategy-executions/{execution_id} ────────────────────────────────


async def test_get_execution_found(client, conn):
    await _seed_strategy(conn, "strat_test")
    start_resp = await client.post(
        "/v1/strategy-executions/start",
        json={"strategy_id": "strat_test", "instance_id": 1, "runtime_params": {}},
    )
    execution_id = start_resp.json()["execution_id"]

    resp = await client.get(f"/v1/strategy-executions/{execution_id}")
    assert resp.status_code == 200
    body = resp.json()
    assert body["execution_id"] == execution_id
    assert body["status"] == "running"


async def test_get_execution_not_found(client):
    resp = await client.get(f"/v1/strategy-executions/{uuid.uuid4()}")
    assert resp.status_code == 404
