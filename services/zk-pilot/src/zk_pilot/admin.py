"""FastAPI admin HTTP router for logical instance and token management."""

from fastapi import APIRouter, Depends, HTTPException, Request
from loguru import logger

from . import db
from .models import (
    CreateInstanceRequest,
    GenerateTokenRequest,
    InstanceResponse,
    SessionResponse,
    TokenResponse,
)

router = APIRouter(prefix="/admin", tags=["admin"])


def _pool(request: Request):
    return request.app.state.pg_pool


@router.post("/instances", status_code=201, response_model=InstanceResponse)
async def create_instance(body: CreateInstanceRequest, pool=Depends(_pool)):
    try:
        async with pool.acquire() as conn:
            row = await db.create_logical_instance(
                conn, body.logical_id, body.instance_type, body.env, body.metadata
            )
        logger.info(f"admin: created instance {body.logical_id!r}")
        return InstanceResponse(**row)
    except Exception as exc:
        raise HTTPException(status_code=409, detail=str(exc))


@router.get("/instances", response_model=list[InstanceResponse])
async def list_instances(pool=Depends(_pool)):
    async with pool.acquire() as conn:
        rows = await db.list_logical_instances(conn)
    return [InstanceResponse(**r) for r in rows]


@router.post("/tokens", status_code=201, response_model=TokenResponse)
async def generate_token(body: GenerateTokenRequest, pool=Depends(_pool)):
    async with pool.acquire() as conn:
        result = await db.generate_token(
            conn, body.logical_id, body.instance_type, body.env, body.expires_days
        )
    logger.info(f"admin: generated token jti={result['token_jti']!r} for {body.logical_id!r}")
    return TokenResponse(**result)


@router.get("/sessions", response_model=list[SessionResponse])
async def list_sessions(pool=Depends(_pool)):
    async with pool.acquire() as conn:
        rows = await db.list_active_sessions(conn)
    return [SessionResponse(**r) for r in rows]


@router.delete("/sessions/{owner_session_id}", status_code=204)
async def force_deregister(owner_session_id: str, pool=Depends(_pool)):
    async with pool.acquire() as conn:
        found = await db.force_deregister_session(conn, owner_session_id)
    if not found:
        raise HTTPException(status_code=404, detail="Session not found or already inactive")
    logger.info(f"admin: force-deregistered session {owner_session_id!r}")
