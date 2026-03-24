"""NATS bootstrap client for Pilot-guided startup.

Sends BootstrapRegisterRequest to Pilot via NATS request-reply,
parses the response, and returns a BootstrapGrant with session info
and runtime config (venue configs).
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field

import betterproto
import nats.aio.client
from loguru import logger


# ── Proto messages (betterproto, wire-compatible with zk.pilot.v1) ────────────


@dataclass
class _BootstrapRegisterRequest(betterproto.Message):
    token: str = betterproto.string_field(1)
    logical_id: str = betterproto.string_field(2)
    instance_type: str = betterproto.string_field(3)
    env: str = betterproto.string_field(4)


@dataclass
class _BootstrapRegisterResponse(betterproto.Message):
    owner_session_id: str = betterproto.string_field(1)
    kv_key: str = betterproto.string_field(2)
    lock_key: str = betterproto.string_field(3)
    lease_ttl_ms: int = betterproto.int64_field(4)
    instance_id: int = betterproto.int32_field(5)
    scoped_credential: str = betterproto.string_field(6)
    status: str = betterproto.string_field(7)
    error_message: str = betterproto.string_field(8)
    runtime_config: str = betterproto.string_field(9)


@dataclass
class _BootstrapDeregisterRequest(betterproto.Message):
    owner_session_id: str = betterproto.string_field(1)
    logical_id: str = betterproto.string_field(2)
    instance_type: str = betterproto.string_field(3)
    env: str = betterproto.string_field(4)


@dataclass
class _BootstrapDeregisterResponse(betterproto.Message):
    success: bool = betterproto.bool_field(1)
    error_message: str = betterproto.string_field(2)


# ── Bootstrap grant ───────────────────────────────────────────────────────────


class BootstrapError(Exception):
    """Raised when bootstrap registration fails."""


@dataclass
class BootstrapGrant:
    """Session grant returned by Pilot after successful bootstrap registration."""

    owner_session_id: str
    kv_key: str
    lock_key: str
    lease_ttl_ms: int
    runtime_config: dict = field(default_factory=dict)


BOOTSTRAP_SUBJECT = "zk.bootstrap.register"
DEREGISTER_SUBJECT = "zk.bootstrap.deregister"
BOOTSTRAP_TIMEOUT_S = 10.0


async def bootstrap_register(
    nc: nats.aio.client.Client,
    token: str,
    logical_id: str,
    env: str,
    instance_type: str = "REFDATA",
) -> BootstrapGrant:
    """Send bootstrap registration request to Pilot and return the grant.

    Raises BootstrapError if Pilot rejects the request or times out.
    """
    req = _BootstrapRegisterRequest(
        token=token,
        logical_id=logical_id,
        instance_type=instance_type,
        env=env,
    )

    logger.info(
        f"bootstrap: registering with Pilot | logical_id={logical_id} "
        f"instance_type={instance_type} env={env}"
    )

    try:
        msg = await nc.request(
            BOOTSTRAP_SUBJECT, bytes(req), timeout=BOOTSTRAP_TIMEOUT_S
        )
    except Exception as exc:
        raise BootstrapError(f"bootstrap request failed: {exc}") from exc

    resp = _BootstrapRegisterResponse().parse(msg.data)

    if resp.status != "OK":
        raise BootstrapError(
            f"bootstrap rejected: status={resp.status} error={resp.error_message}"
        )

    runtime_config: dict = {}
    if resp.runtime_config:
        try:
            runtime_config = json.loads(resp.runtime_config)
        except json.JSONDecodeError as exc:
            raise BootstrapError(
                f"failed to parse runtime_config JSON: {exc}"
            ) from exc

    grant = BootstrapGrant(
        owner_session_id=resp.owner_session_id,
        kv_key=resp.kv_key,
        lock_key=resp.lock_key,
        lease_ttl_ms=resp.lease_ttl_ms,
        runtime_config=runtime_config,
    )

    logger.info(
        f"bootstrap: registered | session={grant.owner_session_id} "
        f"kv_key={grant.kv_key} venues={len(runtime_config.get('venues', []))}"
    )
    return grant


async def bootstrap_deregister(
    nc: nats.aio.client.Client,
    owner_session_id: str,
    logical_id: str,
    env: str,
    instance_type: str = "REFDATA",
) -> None:
    """Send deregister request to Pilot. Best-effort — logs but doesn't raise."""
    req = _BootstrapDeregisterRequest(
        owner_session_id=owner_session_id,
        logical_id=logical_id,
        instance_type=instance_type,
        env=env,
    )
    try:
        msg = await nc.request(DEREGISTER_SUBJECT, bytes(req), timeout=5.0)
        resp = _BootstrapDeregisterResponse().parse(msg.data)
        if not resp.success:
            logger.warning(f"bootstrap: deregister failed: {resp.error_message}")
        else:
            logger.info(f"bootstrap: deregistered session={owner_session_id}")
    except Exception as exc:
        logger.warning(f"bootstrap: deregister request failed: {exc}")
