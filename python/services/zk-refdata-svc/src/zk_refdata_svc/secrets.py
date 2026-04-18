"""Secret resolution for refdata venue configs.

Resolves secret_ref metadata in provided_config into actual credential values.
Two resolution modes:
- Vault: production path using hvac client — reads kv/trading/gw/<account_id>
- Dev env: fallback using ZK_SECRET_<REF>_<FIELD> environment variables
"""

from __future__ import annotations

import os
from typing import Protocol

from loguru import logger


class SecretResolutionError(Exception):
    """Raised when a required secret cannot be resolved."""


class VaultClient(Protocol):
    """Protocol for Vault secret reading.

    read_secret(path) should return a dict of field->value pairs.
    e.g. read_secret("trading/gw/8001") -> {"token": "abc123"}
    """

    def read_secret(self, path: str) -> dict: ...


def _extract_secret_ref_fields(field_descriptors: list[dict] | None) -> list[str]:
    """Extract field names marked as secret_ref from manifest field_descriptors."""
    if not field_descriptors:
        return []
    result = []
    for fd in field_descriptors:
        if fd.get("secret_ref"):
            # path is like "/secret_ref" -> strip leading "/"
            path = fd.get("path", "")
            field_name = path.lstrip("/")
            if field_name:
                result.append(field_name)
    return result


def _dev_env_key(logical_ref: str, field: str) -> str:
    """Build env var name for dev-mode secret fallback.

    Convention: ZK_SECRET_<REF>_<FIELD> where REF has / -> _ and - -> _, all uppercased.
    """
    ref_part = logical_ref.replace("/", "_").replace("-", "_").upper()
    return f"ZK_SECRET_{ref_part}_{field.upper()}"


def _vault_path(logical_ref: str) -> str:
    """Expand a logical ref into a Vault KV path.

    Vault convention from ops.md: kv/trading/gw/<account_id>
    The logical_ref is the account_id (e.g. "8001").
    """
    return f"trading/gw/{logical_ref}"


# Credential fields to look for when resolving a secret ref.
_CREDENTIAL_FIELDS = [
    "token", "api_key", "api_secret", "passphrase", "private_key", "secret_key",
    "apikey", "secretkey",
]


def _resolve_from_vault(vault_client: VaultClient, logical_ref: str) -> dict[str, str]:
    """Resolve a logical secret ref via Vault. Returns dict of credential fields."""
    path = _vault_path(logical_ref)
    try:
        data = vault_client.read_secret(path)
    except Exception as exc:
        raise SecretResolutionError(
            f"vault read failed for ref={logical_ref!r} path={path!r}: {exc}"
        ) from exc
    if not data:
        raise SecretResolutionError(f"empty vault response for ref={logical_ref!r} path={path!r}")
    return {k: v for k, v in data.items() if isinstance(v, str)}


def _resolve_from_env(logical_ref: str) -> dict[str, str]:
    """Resolve a logical secret ref from environment variables (dev mode)."""
    resolved = {}
    for field in _CREDENTIAL_FIELDS:
        env_key = _dev_env_key(logical_ref, field)
        val = os.environ.get(env_key)
        if val:
            resolved[field] = val
    return resolved


def resolve_venue_secrets(
    *,
    venue: str,
    provided_config: dict,
    field_descriptors: list[dict] | None,
    vault_client: VaultClient | None,
    env: str,
) -> dict:
    """Resolve secret_ref fields in provided_config and return a new config dict
    with secret_ref replaced by resolved credential values.

    If no secret_ref fields exist or config doesn't contain them, returns
    a copy of provided_config unchanged.
    """
    secret_fields = _extract_secret_ref_fields(field_descriptors)
    if not secret_fields:
        return dict(provided_config)

    result = dict(provided_config)

    for field_name in secret_fields:
        logical_ref = result.get(field_name)
        if logical_ref is None:
            # Config doesn't include this secret_ref field — venue may not need auth
            continue

        if not logical_ref or not isinstance(logical_ref, str) or not logical_ref.strip():
            raise SecretResolutionError(
                f"empty or invalid secret_ref for field={field_name!r} in venue={venue!r}"
            )

        logical_ref = logical_ref.strip()

        # Resolve via Vault or env fallback
        if vault_client is not None:
            resolved = _resolve_from_vault(vault_client, logical_ref)
        else:
            resolved = _resolve_from_env(logical_ref)

        if not resolved:
            raise SecretResolutionError(
                f"no credentials resolved for ref={logical_ref!r} "
                f"field={field_name!r} venue={venue!r} "
                f"(vault_client={'present' if vault_client else 'none'}, env={env!r})"
            )

        logger.info(
            f"[{venue}] resolved secret ref={logical_ref!r}: "
            f"fields={sorted(resolved.keys())}"
        )

        # Remove the secret_ref field, inject resolved credential fields
        del result[field_name]
        result.update(resolved)

    return result
