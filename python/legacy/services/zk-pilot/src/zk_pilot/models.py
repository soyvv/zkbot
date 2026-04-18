"""Pydantic request/response models for the Pilot HTTP admin API."""

from datetime import datetime
from typing import Any

from pydantic import BaseModel


class CreateInstanceRequest(BaseModel):
    logical_id: str
    instance_type: str
    env: str
    metadata: dict[str, Any] = {}


class InstanceResponse(BaseModel):
    logical_id: str
    instance_type: str
    env: str
    created_at: datetime | None = None


class GenerateTokenRequest(BaseModel):
    logical_id: str
    instance_type: str
    env: str
    expires_days: int = 365


class TokenResponse(BaseModel):
    token_jti: str
    token: str  # plaintext, shown once
    expires_at: str


class SessionResponse(BaseModel):
    owner_session_id: str
    logical_id: str
    instance_type: str
    kv_key: str
    status: str
    last_seen_at: datetime | None = None
    expires_at: datetime | None = None
