"""FastAPI router for strategy-executions lifecycle.

Implements the minimum REST surface that the Strategy Engine calls at startup
and shutdown. The scaffold allocates execution_id, records it in
cfg.strategy_instance, and returns the enriched config the engine needs.

Endpoints match the Phase 10 Pilot contract exactly so the real Pilot
replaces this scaffold with no change to engine or SDK callers.
"""

from __future__ import annotations

import json
import uuid
from datetime import datetime, timezone
from typing import Any

from fastapi import APIRouter, Depends, HTTPException, Request
from pydantic import BaseModel

router = APIRouter(tags=["strategy-executions"])


# ── Dependency ─────────────────────────────────────────────────────────────────


def _pool(request: Request):
    return request.app.state.pg_pool


# ── Request / response models ──────────────────────────────────────────────────


class StartExecutionRequest(BaseModel):
    strategy_id: str
    instance_id: int = 0
    runtime_params: dict[str, Any] = {}


class StartExecutionResponse(BaseModel):
    execution_id: str
    strategy_id: str
    instance_id: int
    status: str
    enriched_config: dict[str, Any]


class StopExecutionRequest(BaseModel):
    execution_id: str
    stop_reason: str = "graceful"


class StopExecutionResponse(BaseModel):
    execution_id: str
    status: str
    stopped_at: str


class ExecutionResponse(BaseModel):
    execution_id: str
    strategy_id: str
    status: str
    started_at: str | None
    ended_at: str | None


class StrategyResponse(BaseModel):
    strategy_id: str
    runtime_type: str
    code_ref: str
    description: str | None
    enabled: bool


# ── Helpers ────────────────────────────────────────────────────────────────────


async def _get_strategy(conn, strategy_id: str) -> dict | None:
    row = await conn.fetchrow(
        "SELECT strategy_id, runtime_type, code_ref, description, enabled "
        "FROM cfg.strategy_definition WHERE strategy_id = $1",
        strategy_id,
    )
    return dict(row) if row else None


async def _build_enriched_config(conn, strategy_id: str) -> dict[str, Any]:
    """Derive enriched runtime config from PG seed data."""
    # Account IDs bound to the default OMS instance
    account_rows = await conn.fetch(
        "SELECT DISTINCT account_id FROM cfg.account_binding ORDER BY account_id"
    )
    account_ids = [r["account_id"] for r in account_rows]

    # Instruments for those accounts
    instrument_rows = await conn.fetch(
        "SELECT DISTINCT instrument_id FROM cfg.account_instrument_config "
        "WHERE account_id = ANY($1::bigint[]) AND enabled = true",
        account_ids,
    )
    instruments = [r["instrument_id"] for r in instrument_rows]

    # Fall back to all instruments if no per-account config
    if not instruments:
        fallback = await conn.fetch(
            "SELECT instrument_id FROM cfg.instrument_refdata WHERE disabled = false"
        )
        instruments = [r["instrument_id"] for r in fallback]

    return {
        "account_ids": account_ids,
        "instruments": instruments,
        "discovery_bucket": "zk-svc-registry-v1",
        "secret_refs": {"vault_path": "kv/trading/gw"},
    }


# ── Routes ─────────────────────────────────────────────────────────────────────


@router.get("/v1/strategies/{strategy_id}", response_model=StrategyResponse)
async def get_strategy(strategy_id: str, pool=Depends(_pool)):
    async with pool.acquire() as conn:
        row = await _get_strategy(conn, strategy_id)
    if row is None:
        raise HTTPException(status_code=404, detail=f"strategy {strategy_id!r} not found")
    return StrategyResponse(**row)


@router.post("/v1/strategy-executions/start", status_code=201, response_model=StartExecutionResponse)
async def start_execution(body: StartExecutionRequest, pool=Depends(_pool)):
    async with pool.acquire() as conn:
        # Validate strategy exists
        strat = await _get_strategy(conn, body.strategy_id)
        if strat is None:
            raise HTTPException(status_code=404, detail=f"strategy {body.strategy_id!r} not found")

        execution_id = str(uuid.uuid4())
        now = datetime.now(timezone.utc)

        # Insert execution row using existing cfg.strategy_instance schema
        await conn.execute(
            """
            INSERT INTO cfg.strategy_instance
              (execution_id, strategy_id, target_oms_id, status, config_override, started_at)
            VALUES ($1, $2,
              (SELECT oms_id FROM cfg.oms_instance LIMIT 1),
              'running', $3::jsonb, $4)
            """,
            execution_id,
            body.strategy_id,
            json.dumps({"instance_id": body.instance_id, **body.runtime_params}),
            now,
        )

        enriched_config = await _build_enriched_config(conn, body.strategy_id)

    return StartExecutionResponse(
        execution_id=execution_id,
        strategy_id=body.strategy_id,
        instance_id=body.instance_id,
        status="running",
        enriched_config=enriched_config,
    )


@router.post("/v1/strategy-executions/stop", response_model=StopExecutionResponse)
async def stop_execution(body: StopExecutionRequest, pool=Depends(_pool)):
    async with pool.acquire() as conn:
        now = datetime.now(timezone.utc)
        result = await conn.execute(
            """
            UPDATE cfg.strategy_instance
            SET status = 'stopped', ended_at = $1
            WHERE execution_id = $2 AND status != 'stopped'
            """,
            now,
            body.execution_id,
        )

    if result == "UPDATE 0":
        raise HTTPException(status_code=404, detail=f"execution {body.execution_id!r} not found or already stopped")

    return StopExecutionResponse(
        execution_id=body.execution_id,
        status="stopped",
        stopped_at=now.isoformat(),
    )


@router.get("/v1/strategy-executions/{execution_id}", response_model=ExecutionResponse)
async def get_execution(execution_id: str, pool=Depends(_pool)):
    async with pool.acquire() as conn:
        row = await conn.fetchrow(
            "SELECT execution_id, strategy_id, status, started_at, ended_at "
            "FROM cfg.strategy_instance WHERE execution_id = $1",
            execution_id,
        )
    if row is None:
        raise HTTPException(status_code=404, detail=f"execution {execution_id!r} not found")
    return ExecutionResponse(
        execution_id=row["execution_id"],
        strategy_id=row["strategy_id"],
        status=row["status"],
        started_at=row["started_at"].isoformat() if row["started_at"] else None,
        ended_at=row["ended_at"].isoformat() if row["ended_at"] else None,
    )
