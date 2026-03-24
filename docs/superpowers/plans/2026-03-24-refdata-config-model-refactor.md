# Refdata Config Model Refactor — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Refactor `zk-refdata-svc` to use the three-layer config model (bootstrap / provided / runtime), add a secret-resolution step that runs before loader instantiation, and update venue integrations (OANDA, OKX) to align with the new contract.

**Architecture:** The host service loads bootstrap config from env, then loads venue-scoped `provided_config` from either Pilot (future) or direct-mode local sources. A secret-resolution step expands `secret_ref` metadata into resolved credentials before assembling `runtime_config`. Venue loaders receive only resolved config — never raw `secret_ref` values. Registration/readiness is deferred until config + secrets + initial refresh succeed.

**Tech Stack:** Python 3.11+, asyncio, dataclasses, loguru, hvac (Vault client), pytest, pytest-asyncio

---

## File Structure

### New Files

| File | Responsibility |
|------|----------------|
| `services/zk-refdata-svc/src/zk_refdata_svc/config_model.py` | Typed config dataclasses: `RefdataBootstrapConfig`, `RefdataVenueProvidedConfig`, `RefdataRuntimeConfig` |
| `services/zk-refdata-svc/src/zk_refdata_svc/secrets.py` | Secret resolution: Vault resolver, env-fallback dev resolver, manifest `field_descriptors` scanning |
| `services/zk-refdata-svc/src/zk_refdata_svc/config_loader.py` | Config assembly: direct-mode and Pilot-mode loading into typed models |
| `services/zk-refdata-svc/tests/test_config_model.py` | Tests for config dataclasses and validation |
| `services/zk-refdata-svc/tests/test_secrets.py` | Tests for secret resolution (Vault, env fallback, failure modes) |
| `services/zk-refdata-svc/tests/test_config_loader.py` | Tests for config assembly in direct and Pilot modes |
| `services/zk-refdata-svc/tests/test_oanda_loader_config.py` | Tests for OANDA loader secret/config contract |
| `services/zk-refdata-svc/tests/test_okx_loader_config.py` | Tests for OKX loader config contract (no secrets needed) |
| `services/zk-refdata-svc/tests/test_startup_readiness.py` | Tests for startup ordering and early-failure behavior |

### Modified Files

| File | Change |
|------|--------|
| `services/zk-refdata-svc/src/zk_refdata_svc/config.py` | Thin wrapper: instantiate `RefdataBootstrapConfig` from env, delegate to `config_loader` |
| `services/zk-refdata-svc/src/zk_refdata_svc/main.py` | New startup sequence: bootstrap → load provided → resolve secrets → assemble runtime → init infra → initial refresh → register |
| `services/zk-refdata-svc/src/zk_refdata_svc/venue_registry.py` | Accept pre-resolved config dict (no changes to resolution logic itself) |
| `services/zk-refdata-svc/src/zk_refdata_svc/jobs/refresh_refdata.py` | Accept `RuntimeConfig` with pre-resolved venue configs |
| `services/zk-refdata-svc/src/zk_refdata_svc/jobs/refresh_sessions.py` | Same: accept pre-resolved venue configs |
| `venue-integrations/oanda/oanda/refdata.py` | Remove `secret_ref` fallback, require resolved `token`, fail early if missing |
| `venue-integrations/oanda/schemas/refdata_config.schema.json` | Keep `secret_ref` in schema (it's valid provided_config metadata); document that `token` is injected at runtime |

### Unchanged Files (no modifications needed)

| File | Why |
|------|-----|
| `venue-integrations/okx/okx/refdata.py` | Already auth-free; no secret handling to fix |
| `venue-integrations/okx/schemas/refdata_config.schema.json` | Already minimal, no secret_ref |
| `services/zk-refdata-svc/src/zk_refdata_svc/registry.py` | Generic NATS KV registry — no config model coupling |
| `services/zk-refdata-svc/src/zk_refdata_svc/loaders/base.py` | Base class stays as-is |

---

## Task 1: Define Typed Config Dataclasses

**Files:**
- Create: `services/zk-refdata-svc/src/zk_refdata_svc/config_model.py`
- Test: `services/zk-refdata-svc/tests/test_config_model.py`

- [ ] **Step 1: Write failing tests for config dataclasses**

```python
# tests/test_config_model.py
"""Tests for typed config model dataclasses."""

from __future__ import annotations

import pytest

from zk_refdata_svc.config_model import (
    RefdataBootstrapConfig,
    RefdataVenueProvidedConfig,
    RefdataRuntimeConfig,
    VenueRuntimeConfig,
)


class TestRefdataBootstrapConfig:
    def test_from_env_minimal(self, monkeypatch):
        monkeypatch.setenv("ZK_NATS_URL", "nats://localhost:4222")
        monkeypatch.setenv("ZK_PG_URL", "postgres://localhost/zkbot")
        cfg = RefdataBootstrapConfig.from_env()
        assert cfg.nats_url == "nats://localhost:4222"
        assert cfg.pg_url == "postgres://localhost/zkbot"
        assert cfg.grpc_port == 50052  # default
        assert cfg.logical_id == "refdata_dev_1"  # default
        assert cfg.mode == "direct"  # default

    def test_from_env_pilot_mode(self, monkeypatch):
        monkeypatch.setenv("ZK_NATS_URL", "nats://localhost:4222")
        monkeypatch.setenv("ZK_PG_URL", "postgres://localhost/zkbot")
        monkeypatch.setenv("ZK_MODE", "pilot")
        monkeypatch.setenv("ZK_PILOT_URL", "http://pilot:8080")
        cfg = RefdataBootstrapConfig.from_env()
        assert cfg.mode == "pilot"
        assert cfg.pilot_url == "http://pilot:8080"

    def test_from_env_missing_required(self, monkeypatch):
        monkeypatch.delenv("ZK_NATS_URL", raising=False)
        monkeypatch.delenv("ZK_PG_URL", raising=False)
        with pytest.raises(KeyError):
            RefdataBootstrapConfig.from_env()


class TestRefdataVenueProvidedConfig:
    def test_from_dict_oanda(self):
        raw = {
            "venue": "oanda",
            "enabled": True,
            "config": {
                "environment": "practice",
                "account_id": "101-001-123",
                "secret_ref": "oanda/main",
            },
        }
        vpc = RefdataVenueProvidedConfig.from_dict(raw)
        assert vpc.venue == "oanda"
        assert vpc.enabled is True
        assert vpc.config["secret_ref"] == "oanda/main"

    def test_from_dict_disabled(self):
        raw = {"venue": "oanda", "enabled": False, "config": {}}
        vpc = RefdataVenueProvidedConfig.from_dict(raw)
        assert vpc.enabled is False

    def test_from_dict_missing_venue_raises(self):
        with pytest.raises(ValueError, match="venue"):
            RefdataVenueProvidedConfig.from_dict({"enabled": True, "config": {}})


class TestVenueRuntimeConfig:
    def test_has_resolved_config(self):
        vrc = VenueRuntimeConfig(
            venue="oanda",
            enabled=True,
            provided_config={"environment": "practice", "secret_ref": "oanda/main"},
            resolved_config={"environment": "practice", "token": "resolved-token-xyz"},
        )
        assert vrc.resolved_config["token"] == "resolved-token-xyz"
        assert "secret_ref" not in vrc.resolved_config


class TestRefdataRuntimeConfig:
    def test_enabled_venues(self):
        vrc1 = VenueRuntimeConfig("oanda", True, {}, {})
        vrc2 = VenueRuntimeConfig("okx", False, {}, {})
        rtc = RefdataRuntimeConfig(
            logical_id="refdata_dev_1",
            nats_url="nats://localhost:4222",
            pg_url="postgres://localhost/zkbot",
            grpc_port=50052,
            advertise_host="localhost",
            refresh_interval_s=300,
            heartbeat_interval_s=5,
            lease_ttl_s=20,
            mode="direct",
            venues={"oanda": vrc1, "okx": vrc2},
        )
        assert rtc.enabled_venues() == ["oanda"]

    def test_redacted_dict_hides_secrets(self):
        vrc = VenueRuntimeConfig(
            venue="oanda",
            enabled=True,
            provided_config={"secret_ref": "oanda/main"},
            resolved_config={"token": "secret-value"},
        )
        rtc = RefdataRuntimeConfig(
            logical_id="test",
            nats_url="nats://localhost",
            pg_url="postgres://localhost",
            grpc_port=50052,
            advertise_host="localhost",
            refresh_interval_s=300,
            heartbeat_interval_s=5,
            lease_ttl_s=20,
            mode="direct",
            venues={"oanda": vrc},
        )
        d = rtc.redacted_dict()
        assert d["venues"]["oanda"]["resolved_config"]["token"] == "***"
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd zkbot/services/zk-refdata-svc && uv run pytest tests/test_config_model.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'zk_refdata_svc.config_model'`

- [ ] **Step 3: Implement config_model.py**

```python
# src/zk_refdata_svc/config_model.py
"""Typed config model for zk-refdata-svc.

Three-layer config model:
- bootstrap_config: minimal env/deployment-owned startup inputs
- provided_config: Pilot-managed or direct-mode venue-scoped config
- runtime_config: effective assembled config with resolved secrets
"""

from __future__ import annotations

import copy
import os
from dataclasses import dataclass, field


@dataclass
class RefdataBootstrapConfig:
    """Minimal startup inputs supplied by deployment/orchestration."""

    nats_url: str
    pg_url: str
    grpc_port: int = 50052
    logical_id: str = "refdata_dev_1"
    advertise_host: str = "localhost"
    refresh_interval_s: int = 300
    heartbeat_interval_s: int = 5
    lease_ttl_s: int = 20
    mode: str = "direct"  # "direct" or "pilot"
    pilot_url: str | None = None
    env: str = "dev"
    # Vault bootstrap inputs (optional, for secret resolution)
    vault_addr: str | None = None
    vault_role_id: str | None = None
    vault_secret_id: str | None = None
    vault_token: str | None = None

    @classmethod
    def from_env(cls) -> RefdataBootstrapConfig:
        return cls(
            nats_url=os.environ["ZK_NATS_URL"],
            pg_url=os.environ["ZK_PG_URL"],
            grpc_port=int(os.getenv("ZK_REFDATA_GRPC_PORT", "50052")),
            logical_id=os.getenv("ZK_REFDATA_LOGICAL_ID", "refdata_dev_1"),
            advertise_host=os.getenv("ZK_REFDATA_ADVERTISE_HOST", "localhost"),
            refresh_interval_s=int(os.getenv("ZK_REFDATA_REFRESH_INTERVAL_S", "300")),
            heartbeat_interval_s=int(os.getenv("ZK_REFDATA_HEARTBEAT_INTERVAL_S", "5")),
            lease_ttl_s=int(os.getenv("ZK_REFDATA_LEASE_TTL_S", "20")),
            mode=os.getenv("ZK_MODE", "direct"),
            pilot_url=os.getenv("ZK_PILOT_URL"),
            env=os.getenv("ZK_ENV", "dev"),
            vault_addr=os.getenv("VAULT_ADDR"),
            vault_role_id=os.getenv("VAULT_ROLE_ID"),
            vault_secret_id=os.getenv("VAULT_SECRET_ID"),
            vault_token=os.getenv("VAULT_TOKEN"),
        )


@dataclass
class RefdataVenueProvidedConfig:
    """Venue-scoped config from Pilot or direct-mode local source.

    This is the control-plane config shape. It may contain secret_ref
    metadata but never resolved secret values.
    """

    venue: str
    enabled: bool
    config: dict = field(default_factory=dict)

    @classmethod
    def from_dict(cls, raw: dict) -> RefdataVenueProvidedConfig:
        venue = raw.get("venue")
        if not venue:
            raise ValueError("venue is required in provided config")
        return cls(
            venue=venue,
            enabled=raw.get("enabled", True),
            config=raw.get("config", {}),
        )


@dataclass
class VenueRuntimeConfig:
    """Effective per-venue config with secrets resolved."""

    venue: str
    enabled: bool
    provided_config: dict  # original provided_config (may contain secret_ref)
    resolved_config: dict  # config with secret_ref replaced by resolved values


# Fields that must be redacted in introspection output.
_SECRET_FIELDS = {"token", "api_key", "api_secret", "passphrase", "secret_key", "private_key"}


@dataclass
class RefdataRuntimeConfig:
    """Effective assembled config used by the service at runtime."""

    logical_id: str
    nats_url: str
    pg_url: str
    grpc_port: int
    advertise_host: str
    refresh_interval_s: int
    heartbeat_interval_s: int
    lease_ttl_s: int
    mode: str
    venues: dict[str, VenueRuntimeConfig] = field(default_factory=dict)

    def enabled_venues(self) -> list[str]:
        """Return sorted list of enabled venue names."""
        return sorted(v.venue for v in self.venues.values() if v.enabled)

    def venue_resolved_config(self, venue: str) -> dict:
        """Return the resolved config dict for a venue. Raises KeyError if not found."""
        return self.venues[venue].resolved_config

    def redacted_dict(self) -> dict:
        """Return a dict representation with secret values replaced by '***'."""
        result: dict = {
            "logical_id": self.logical_id,
            "grpc_port": self.grpc_port,
            "advertise_host": self.advertise_host,
            "refresh_interval_s": self.refresh_interval_s,
            "mode": self.mode,
            "venues": {},
        }
        for name, vrc in self.venues.items():
            redacted_resolved = {}
            for k, v in vrc.resolved_config.items():
                redacted_resolved[k] = "***" if k in _SECRET_FIELDS else v
            result["venues"][name] = {
                "venue": vrc.venue,
                "enabled": vrc.enabled,
                "resolved_config": redacted_resolved,
            }
        return result
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd zkbot/services/zk-refdata-svc && uv run pytest tests/test_config_model.py -v`
Expected: All PASS

- [ ] **Step 5: Commit**

```bash
git add services/zk-refdata-svc/src/zk_refdata_svc/config_model.py services/zk-refdata-svc/tests/test_config_model.py
git commit -m "feat(refdata): add typed config model dataclasses (bootstrap/provided/runtime)"
```

---

## Task 2: Implement Secret Resolution

**Files:**
- Create: `services/zk-refdata-svc/src/zk_refdata_svc/secrets.py`
- Test: `services/zk-refdata-svc/tests/test_secrets.py`

**Context:** The secret resolver scans venue `provided_config` for fields marked `secret_ref: true` in the manifest `field_descriptors`. For each `secret_ref` field, it resolves the logical ref via Vault (production) or env vars (dev fallback). The resolved values are injected into a new config dict that replaces `secret_ref` with the actual credential fields.

The Vault path convention from `ops.md` is:
- `kv/trading/gw/<account_id>/token` (OANDA)
- `kv/trading/gw/<account_id>/api_key`, `api_secret`, `passphrase` (OKX)

The dev-mode env var fallback convention is: `ZK_SECRET_<REF>_<FIELD>` (uppercased, `/` → `_`, `-` → `_`).

- [ ] **Step 1: Write failing tests for secret resolution**

```python
# tests/test_secrets.py
"""Tests for secret resolution."""

from __future__ import annotations

import pytest

from zk_refdata_svc.secrets import (
    SecretResolutionError,
    resolve_venue_secrets,
    _dev_env_key,
    _extract_secret_ref_fields,
)


class TestExtractSecretRefFields:
    def test_finds_secret_ref_fields_from_descriptors(self):
        descriptors = [
            {"path": "/secret_ref", "secret_ref": True, "reloadable": False},
            {"path": "/environment", "reloadable": False},
        ]
        result = _extract_secret_ref_fields(descriptors)
        assert result == ["secret_ref"]

    def test_no_secret_fields(self):
        descriptors = [{"path": "/environment", "reloadable": False}]
        assert _extract_secret_ref_fields(descriptors) == []

    def test_empty_descriptors(self):
        assert _extract_secret_ref_fields([]) == []
        assert _extract_secret_ref_fields(None) == []


class TestDevEnvKey:
    def test_simple_ref(self):
        assert _dev_env_key("oanda/main", "token") == "ZK_SECRET_OANDA_MAIN_TOKEN"

    def test_hyphenated_ref(self):
        assert _dev_env_key("okx/trading-primary", "api_key") == "ZK_SECRET_OKX_TRADING_PRIMARY_API_KEY"


class TestResolveVenueSecrets:
    def test_no_secret_refs_returns_config_unchanged(self):
        config = {"environment": "practice", "account_id": "101"}
        descriptors = [{"path": "/environment", "reloadable": False}]
        result = resolve_venue_secrets(
            venue="oanda",
            provided_config=config,
            field_descriptors=descriptors,
            vault_client=None,
            env="dev",
        )
        assert result == config

    def test_dev_mode_resolves_from_env(self, monkeypatch):
        monkeypatch.setenv("ZK_SECRET_OANDA_MAIN_TOKEN", "dev-token-123")
        config = {
            "environment": "practice",
            "account_id": "101",
            "secret_ref": "oanda/main",
        }
        descriptors = [
            {"path": "/secret_ref", "secret_ref": True, "reloadable": False},
        ]
        result = resolve_venue_secrets(
            venue="oanda",
            provided_config=config,
            field_descriptors=descriptors,
            vault_client=None,
            env="dev",
        )
        assert result["token"] == "dev-token-123"
        assert "secret_ref" not in result
        # Non-secret fields preserved
        assert result["environment"] == "practice"

    def test_missing_required_secret_raises(self):
        config = {"secret_ref": "oanda/main"}
        descriptors = [{"path": "/secret_ref", "secret_ref": True, "reloadable": False}]
        with pytest.raises(SecretResolutionError, match="oanda/main"):
            resolve_venue_secrets(
                venue="oanda",
                provided_config=config,
                field_descriptors=descriptors,
                vault_client=None,
                env="dev",
            )

    def test_malformed_secret_ref_raises(self):
        config = {"secret_ref": ""}
        descriptors = [{"path": "/secret_ref", "secret_ref": True, "reloadable": False}]
        with pytest.raises(SecretResolutionError, match="empty"):
            resolve_venue_secrets(
                venue="oanda",
                provided_config=config,
                field_descriptors=descriptors,
                vault_client=None,
                env="dev",
            )

    def test_config_without_secret_ref_field_but_descriptor_requires_it(self):
        """If manifest says secret_ref exists but config doesn't have it, skip (venue may not need auth)."""
        config = {"environment": "practice"}
        descriptors = [{"path": "/secret_ref", "secret_ref": True, "reloadable": False}]
        result = resolve_venue_secrets(
            venue="oanda",
            provided_config=config,
            field_descriptors=descriptors,
            vault_client=None,
            env="dev",
        )
        assert result == config

    def test_vault_resolution(self):
        """Vault client resolves secrets when available."""

        class FakeVaultClient:
            def read_secret(self, path: str) -> dict:
                if path == "kv/zkbot/prod/venues/oanda/accounts/main":
                    return {"token": "vault-token-abc"}
                raise KeyError(f"no secret at {path}")

        config = {"environment": "live", "secret_ref": "oanda/main"}
        descriptors = [{"path": "/secret_ref", "secret_ref": True, "reloadable": False}]
        result = resolve_venue_secrets(
            venue="oanda",
            provided_config=config,
            field_descriptors=descriptors,
            vault_client=FakeVaultClient(),
            env="prod",
        )
        assert result["token"] == "vault-token-abc"
        assert "secret_ref" not in result
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd zkbot/services/zk-refdata-svc && uv run pytest tests/test_secrets.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'zk_refdata_svc.secrets'`

- [ ] **Step 3: Implement secrets.py**

```python
# src/zk_refdata_svc/secrets.py
"""Secret resolution for refdata venue configs.

Resolves secret_ref metadata in provided_config into actual credential values.
Two resolution modes:
- Vault: production path using hvac client
- Dev env: fallback using ZK_SECRET_<REF>_<FIELD> environment variables
"""

from __future__ import annotations

import copy
import os
from typing import Any, Protocol

from loguru import logger


class SecretResolutionError(Exception):
    """Raised when a required secret cannot be resolved."""


class VaultClient(Protocol):
    """Protocol for Vault secret reading."""

    def read_secret(self, path: str) -> dict: ...


def _extract_secret_ref_fields(field_descriptors: list[dict] | None) -> list[str]:
    """Extract field names marked as secret_ref from manifest field_descriptors."""
    if not field_descriptors:
        return []
    result = []
    for fd in field_descriptors:
        if fd.get("secret_ref"):
            # path is like "/secret_ref" → strip leading "/"
            path = fd.get("path", "")
            field_name = path.lstrip("/")
            if field_name:
                result.append(field_name)
    return result


def _dev_env_key(logical_ref: str, field: str) -> str:
    """Build env var name for dev-mode secret fallback.

    Convention: ZK_SECRET_<REF>_<FIELD> where REF has / → _ and - → _, all uppercased.
    """
    ref_part = logical_ref.replace("/", "_").replace("-", "_").upper()
    return f"ZK_SECRET_{ref_part}_{field.upper()}"


def _vault_path(env: str, logical_ref: str) -> str:
    """Expand a two-part logical ref into a Vault KV path.

    "oanda/main" → "kv/zkbot/<env>/venues/oanda/accounts/main"
    """
    parts = logical_ref.split("/", 1)
    if len(parts) == 2:
        venue, account = parts
        return f"kv/zkbot/{env}/venues/{venue}/accounts/{account}"
    return f"kv/zkbot/{env}/services/refdata/{logical_ref}"


# Default credential fields to look for when resolving a secret ref.
# Vault secret may contain any subset of these.
_CREDENTIAL_FIELDS = ["token", "api_key", "api_secret", "passphrase", "private_key", "secret_key"]


def _resolve_from_vault(
    vault_client: VaultClient, env: str, logical_ref: str
) -> dict[str, str]:
    """Resolve a logical secret ref via Vault. Returns dict of credential fields."""
    path = _vault_path(env, logical_ref)
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
            resolved = _resolve_from_vault(vault_client, env, logical_ref)
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd zkbot/services/zk-refdata-svc && uv run pytest tests/test_secrets.py -v`
Expected: All PASS

- [ ] **Step 5: Commit**

```bash
git add services/zk-refdata-svc/src/zk_refdata_svc/secrets.py services/zk-refdata-svc/tests/test_secrets.py
git commit -m "feat(refdata): add secret resolution module (Vault + dev env fallback)"
```

---

## Task 3: Implement Config Loader (Direct + Pilot Assembly)

**Files:**
- Create: `services/zk-refdata-svc/src/zk_refdata_svc/config_loader.py`
- Test: `services/zk-refdata-svc/tests/test_config_loader.py`

**Context:** The config loader assembles `RefdataRuntimeConfig` from bootstrap + provided configs. In direct mode, it reads venue configs from the JSON file (existing behavior via `ZK_REFDATA_VENUE_CONFIGS` env var) and the venues list from `ZK_REFDATA_VENUES`. In Pilot mode (stub for now), it would fetch venue-scoped configs from Pilot. The loader also drives secret resolution per venue.

- [ ] **Step 1: Write failing tests for config loader**

```python
# tests/test_config_loader.py
"""Tests for config assembly in direct and Pilot modes."""

from __future__ import annotations

import json
import pathlib

import pytest

from zk_refdata_svc.config_model import RefdataBootstrapConfig, RefdataRuntimeConfig
from zk_refdata_svc.config_loader import (
    load_provided_configs_direct,
    assemble_runtime_config,
    ConfigAssemblyError,
)


class TestLoadProvidedConfigsDirect:
    def test_loads_from_venues_and_configs_file(self, tmp_path: pathlib.Path):
        config_file = tmp_path / "venue_configs.json"
        config_file.write_text(json.dumps({
            "oanda": {
                "environment": "practice",
                "account_id": "101-001-123",
                "secret_ref": "oanda/main",
            },
            "okx": {
                "api_base_url": "https://www.okx.com",
            },
        }))

        venues = ["oanda", "okx"]
        provided = load_provided_configs_direct(venues, str(config_file))
        assert len(provided) == 2
        assert provided[0].venue == "oanda"
        assert provided[0].config["secret_ref"] == "oanda/main"
        assert provided[1].venue == "okx"

    def test_venue_without_config_gets_empty_dict(self, tmp_path: pathlib.Path):
        config_file = tmp_path / "venue_configs.json"
        config_file.write_text(json.dumps({}))
        provided = load_provided_configs_direct(["okx"], str(config_file))
        assert provided[0].config == {}

    def test_no_config_file_returns_empty_configs(self):
        provided = load_provided_configs_direct(["okx"], None)
        assert len(provided) == 1
        assert provided[0].config == {}

    def test_empty_venues_returns_empty(self):
        assert load_provided_configs_direct([], None) == []


class TestAssembleRuntimeConfig:
    def test_direct_mode_single_venue_no_secrets(self, tmp_path: pathlib.Path):
        config_file = tmp_path / "venue_configs.json"
        config_file.write_text(json.dumps({"okx": {"api_base_url": "https://www.okx.com"}}))

        bootstrap = RefdataBootstrapConfig(
            nats_url="nats://localhost:4222",
            pg_url="postgres://localhost/zkbot",
            mode="direct",
            env="dev",
        )
        rtc = assemble_runtime_config(
            bootstrap=bootstrap,
            venues=["okx"],
            venue_configs_path=str(config_file),
            vault_client=None,
        )
        assert isinstance(rtc, RefdataRuntimeConfig)
        assert rtc.enabled_venues() == ["okx"]
        assert rtc.venues["okx"].resolved_config["api_base_url"] == "https://www.okx.com"

    def test_direct_mode_secret_resolution_failure_raises(self, tmp_path: pathlib.Path):
        """If a venue needs secrets and they can't be resolved, assembly fails."""
        config_file = tmp_path / "venue_configs.json"
        config_file.write_text(json.dumps({
            "oanda": {
                "environment": "practice",
                "account_id": "101",
                "secret_ref": "oanda/main",
            },
        }))

        bootstrap = RefdataBootstrapConfig(
            nats_url="nats://localhost:4222",
            pg_url="postgres://localhost/zkbot",
            mode="direct",
            env="dev",
        )
        # No vault client, no env vars set → resolution should fail
        with pytest.raises(ConfigAssemblyError, match="oanda"):
            assemble_runtime_config(
                bootstrap=bootstrap,
                venues=["oanda"],
                venue_configs_path=str(config_file),
                vault_client=None,
            )

    def test_direct_mode_with_dev_env_secrets(self, tmp_path: pathlib.Path, monkeypatch):
        monkeypatch.setenv("ZK_SECRET_OANDA_MAIN_TOKEN", "dev-tok")
        config_file = tmp_path / "venue_configs.json"
        config_file.write_text(json.dumps({
            "oanda": {
                "environment": "practice",
                "account_id": "101",
                "secret_ref": "oanda/main",
            },
        }))

        bootstrap = RefdataBootstrapConfig(
            nats_url="nats://localhost:4222",
            pg_url="postgres://localhost/zkbot",
            mode="direct",
            env="dev",
        )
        rtc = assemble_runtime_config(
            bootstrap=bootstrap,
            venues=["oanda"],
            venue_configs_path=str(config_file),
            vault_client=None,
        )
        assert rtc.venues["oanda"].resolved_config["token"] == "dev-tok"
        assert "secret_ref" not in rtc.venues["oanda"].resolved_config

    def test_malformed_config_file_raises(self, tmp_path: pathlib.Path):
        config_file = tmp_path / "bad.json"
        config_file.write_text("not json")
        bootstrap = RefdataBootstrapConfig(
            nats_url="nats://localhost:4222",
            pg_url="postgres://localhost/zkbot",
        )
        with pytest.raises(ConfigAssemblyError, match="config file"):
            assemble_runtime_config(
                bootstrap=bootstrap,
                venues=["okx"],
                venue_configs_path=str(config_file),
                vault_client=None,
            )
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd zkbot/services/zk-refdata-svc && uv run pytest tests/test_config_loader.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'zk_refdata_svc.config_loader'`

- [ ] **Step 3: Implement config_loader.py**

```python
# src/zk_refdata_svc/config_loader.py
"""Config assembly for zk-refdata-svc.

Loads provided_config from direct-mode (JSON file + env vars) or
Pilot-mode (stub), resolves secrets, and assembles runtime_config.
"""

from __future__ import annotations

import json
import pathlib

from loguru import logger

from zk_refdata_svc.config_model import (
    RefdataBootstrapConfig,
    RefdataRuntimeConfig,
    RefdataVenueProvidedConfig,
    VenueRuntimeConfig,
)
from zk_refdata_svc.secrets import SecretResolutionError, VaultClient, resolve_venue_secrets


class ConfigAssemblyError(Exception):
    """Raised when config assembly fails."""


def load_provided_configs_direct(
    venues: list[str],
    venue_configs_path: str | None,
) -> list[RefdataVenueProvidedConfig]:
    """Load venue provided configs from a JSON file (direct mode).

    The JSON file maps venue name → config dict. Each venue in the venues
    list gets a RefdataVenueProvidedConfig entry.
    """
    if not venues:
        return []

    raw_configs: dict[str, dict] = {}
    if venue_configs_path and pathlib.Path(venue_configs_path).exists():
        with open(venue_configs_path) as f:
            raw_configs = json.load(f)

    return [
        RefdataVenueProvidedConfig(
            venue=v,
            enabled=True,
            config=raw_configs.get(v, {}),
        )
        for v in venues
    ]


def _load_field_descriptors(venue: str) -> list[dict]:
    """Load field_descriptors for refdata capability from venue manifest.

    Returns empty list if manifest not found or no refdata capability.
    """
    try:
        from zk_refdata_svc.venue_registry import load_manifest
        manifest = load_manifest(venue)
        refdata_cap = manifest.get("capabilities", {}).get("refdata", {})
        return refdata_cap.get("field_descriptors", [])
    except (FileNotFoundError, Exception):
        return []


def assemble_runtime_config(
    *,
    bootstrap: RefdataBootstrapConfig,
    venues: list[str],
    venue_configs_path: str | None,
    vault_client: VaultClient | None,
) -> RefdataRuntimeConfig:
    """Assemble the full runtime config from bootstrap + provided + resolved secrets.

    Raises ConfigAssemblyError on:
    - malformed config file
    - secret resolution failure for any enabled venue
    """
    # Step 1: Load provided configs
    try:
        if bootstrap.mode == "pilot":
            # Pilot mode stub — future: fetch from Pilot API
            logger.warning("pilot mode not yet implemented, falling back to direct")
            provided_list = load_provided_configs_direct(venues, venue_configs_path)
        else:
            provided_list = load_provided_configs_direct(venues, venue_configs_path)
    except (json.JSONDecodeError, OSError) as exc:
        raise ConfigAssemblyError(f"failed to load config file: {exc}") from exc

    # Step 2: Resolve secrets and build venue runtime configs
    venue_runtime: dict[str, VenueRuntimeConfig] = {}
    for vpc in provided_list:
        if not vpc.enabled:
            venue_runtime[vpc.venue] = VenueRuntimeConfig(
                venue=vpc.venue,
                enabled=False,
                provided_config=vpc.config,
                resolved_config=vpc.config,
            )
            continue

        field_descriptors = _load_field_descriptors(vpc.venue)

        try:
            resolved = resolve_venue_secrets(
                venue=vpc.venue,
                provided_config=vpc.config,
                field_descriptors=field_descriptors,
                vault_client=vault_client,
                env=bootstrap.env,
            )
        except SecretResolutionError as exc:
            raise ConfigAssemblyError(
                f"secret resolution failed for venue={vpc.venue!r}: {exc}"
            ) from exc

        venue_runtime[vpc.venue] = VenueRuntimeConfig(
            venue=vpc.venue,
            enabled=True,
            provided_config=vpc.config,
            resolved_config=resolved,
        )

    # Step 3: Assemble final runtime config
    return RefdataRuntimeConfig(
        logical_id=bootstrap.logical_id,
        nats_url=bootstrap.nats_url,
        pg_url=bootstrap.pg_url,
        grpc_port=bootstrap.grpc_port,
        advertise_host=bootstrap.advertise_host,
        refresh_interval_s=bootstrap.refresh_interval_s,
        heartbeat_interval_s=bootstrap.heartbeat_interval_s,
        lease_ttl_s=bootstrap.lease_ttl_s,
        mode=bootstrap.mode,
        venues=venue_runtime,
    )
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd zkbot/services/zk-refdata-svc && uv run pytest tests/test_config_loader.py -v`
Expected: All PASS (note: tests using manifest loading may need `ZK_VENUE_INTEGRATIONS_DIR` or patching `_load_field_descriptors`)

- [ ] **Step 5: Commit**

```bash
git add services/zk-refdata-svc/src/zk_refdata_svc/config_loader.py services/zk-refdata-svc/tests/test_config_loader.py
git commit -m "feat(refdata): add config loader with direct-mode assembly and secret resolution"
```

---

## Task 4: Fix OANDA Loader — Remove secret_ref Fallback

**Files:**
- Modify: `venue-integrations/oanda/oanda/refdata.py:40`
- Test: `services/zk-refdata-svc/tests/test_oanda_loader_config.py`

**Context:** The OANDA loader currently does `cfg.get("token") or cfg.get("secret_ref", "")` on line 40 of `refdata.py`. This means `secret_ref` is used as a credential fallback — a violation of the new model. The loader should require a resolved `token` and fail early if it's missing.

- [ ] **Step 1: Write failing tests for OANDA loader config contract**

```python
# tests/test_oanda_loader_config.py
"""Tests for OANDA refdata loader config/secret contract."""

from __future__ import annotations

import sys
import pathlib

import pytest

# Add venue-integrations/oanda to path so we can import the loader
_VENUE_DIR = pathlib.Path(__file__).resolve().parent.parent.parent.parent / "venue-integrations" / "oanda"
if str(_VENUE_DIR) not in sys.path:
    sys.path.insert(0, str(_VENUE_DIR))

from oanda.refdata import OandaRefdataLoader


class TestOandaLoaderConfigContract:
    def test_accepts_resolved_token(self):
        """Loader should accept config with resolved token field."""
        loader = OandaRefdataLoader(config={
            "environment": "practice",
            "account_id": "101-001-123",
            "token": "resolved-token-xyz",
        })
        assert loader._token == "resolved-token-xyz"

    def test_rejects_config_with_only_secret_ref(self):
        """Loader must NOT use secret_ref as a credential."""
        with pytest.raises(ValueError, match="token"):
            OandaRefdataLoader(config={
                "environment": "practice",
                "account_id": "101-001-123",
                "secret_ref": "oanda/main",
            })

    def test_rejects_config_with_no_token(self):
        """Loader should fail early if token is missing entirely."""
        with pytest.raises(ValueError, match="token"):
            OandaRefdataLoader(config={
                "environment": "practice",
                "account_id": "101-001-123",
            })

    def test_rejects_empty_token(self):
        with pytest.raises(ValueError, match="token"):
            OandaRefdataLoader(config={
                "environment": "practice",
                "account_id": "101-001-123",
                "token": "",
            })
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd zkbot/services/zk-refdata-svc && uv run pytest tests/test_oanda_loader_config.py -v`
Expected: `test_rejects_config_with_only_secret_ref` FAILS (currently doesn't raise), `test_rejects_config_with_no_token` FAILS, `test_rejects_empty_token` FAILS

- [ ] **Step 3: Update OANDA refdata loader**

In `venue-integrations/oanda/oanda/refdata.py`, replace lines 36-41:

**Before:**
```python
    def __init__(self, config: dict | None = None) -> None:
        cfg = config or {}
        env = cfg.get("environment", "practice")
        self._account_id = cfg.get("account_id", "")
        self._token = cfg.get("token") or cfg.get("secret_ref", "")
        self._api_base_url = cfg.get("api_base_url") or _API_URLS.get(env, _API_URLS["practice"])
```

**After:**
```python
    def __init__(self, config: dict | None = None) -> None:
        cfg = config or {}
        env = cfg.get("environment", "practice")
        self._account_id = cfg.get("account_id", "")
        token = cfg.get("token", "")
        if not token:
            raise ValueError(
                "OANDA refdata loader requires a resolved 'token' in config. "
                "The host must resolve secret_ref before loader instantiation."
            )
        self._token = token
        self._api_base_url = cfg.get("api_base_url") or _API_URLS.get(env, _API_URLS["practice"])
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd zkbot/services/zk-refdata-svc && uv run pytest tests/test_oanda_loader_config.py -v`
Expected: All PASS

- [ ] **Step 5: Commit**

```bash
git add venue-integrations/oanda/oanda/refdata.py services/zk-refdata-svc/tests/test_oanda_loader_config.py
git commit -m "fix(oanda): remove secret_ref fallback, require resolved token in refdata loader"
```

---

## Task 5: Verify OKX Loader Is Auth-Free

**Files:**
- Test: `services/zk-refdata-svc/tests/test_okx_loader_config.py`

**Context:** OKX refdata uses only public API endpoints. Confirm this is explicit and covered by a test.

- [ ] **Step 1: Write tests for OKX loader config contract**

```python
# tests/test_okx_loader_config.py
"""Tests for OKX refdata loader config contract.

OKX refdata is public-only — no auth needed. These tests make that explicit.
"""

from __future__ import annotations

import sys
import pathlib

import pytest

_VENUE_DIR = pathlib.Path(__file__).resolve().parent.parent.parent.parent / "venue-integrations" / "okx"
if str(_VENUE_DIR) not in sys.path:
    sys.path.insert(0, str(_VENUE_DIR))

# OKX loader imports from zk_refdata_svc.loaders.base — ensure service is on path
_SVC_DIR = pathlib.Path(__file__).resolve().parent.parent / "src"
if str(_SVC_DIR) not in sys.path:
    sys.path.insert(0, str(_SVC_DIR))

from okx.refdata import OkxRefdataLoader


class TestOkxLoaderConfigContract:
    def test_accepts_empty_config(self):
        """OKX refdata needs no credentials — empty config is valid."""
        loader = OkxRefdataLoader(config={})
        assert hasattr(loader, "load_instruments")

    def test_accepts_none_config(self):
        loader = OkxRefdataLoader(config=None)
        assert hasattr(loader, "load_instruments")

    def test_accepts_api_base_url(self):
        loader = OkxRefdataLoader(config={"api_base_url": "https://example.com"})
        assert loader._config["api_base_url"] == "https://example.com"

    def test_no_secret_ref_in_schema(self):
        """OKX refdata schema should not require secret_ref."""
        import json
        schema_path = (
            pathlib.Path(__file__).resolve().parent.parent.parent.parent
            / "venue-integrations" / "okx" / "schemas" / "refdata_config.schema.json"
        )
        schema = json.loads(schema_path.read_text())
        assert "secret_ref" not in schema.get("required", [])
        assert "secret_ref" not in schema.get("properties", {})
```

- [ ] **Step 2: Run tests**

Run: `cd zkbot/services/zk-refdata-svc && uv run pytest tests/test_okx_loader_config.py -v`
Expected: All PASS (no code changes needed for OKX)

- [ ] **Step 3: Commit**

```bash
git add services/zk-refdata-svc/tests/test_okx_loader_config.py
git commit -m "test(okx): add explicit tests confirming OKX refdata is auth-free"
```

---

## Task 6: Wire New Config Model into main.py

**Files:**
- Modify: `services/zk-refdata-svc/src/zk_refdata_svc/config.py`
- Modify: `services/zk-refdata-svc/src/zk_refdata_svc/main.py`
- Modify: `services/zk-refdata-svc/src/zk_refdata_svc/jobs/refresh_refdata.py`
- Modify: `services/zk-refdata-svc/src/zk_refdata_svc/jobs/refresh_sessions.py`

**Context:** This task wires the new typed config model into the actual startup sequence. The key changes:
1. `config.py` becomes a thin compatibility shim that builds `RefdataBootstrapConfig` + calls `assemble_runtime_config`
2. `main.py` uses `RefdataRuntimeConfig` and defers KV registration until after initial refresh succeeds
3. Refresh jobs receive pre-resolved venue configs from `RefdataRuntimeConfig`

- [ ] **Step 1: Update config.py to build typed config**

Replace the entire `config.py` with:

```python
# src/zk_refdata_svc/config.py
"""Runtime configuration — thin wrapper over the typed config model.

Builds RefdataBootstrapConfig from env, then assembles RefdataRuntimeConfig
via the config_loader. Kept as a module-level convenience for backward compat.
"""

import os

from loguru import logger

from zk_refdata_svc.config_model import RefdataBootstrapConfig, RefdataRuntimeConfig
from zk_refdata_svc.config_loader import assemble_runtime_config


def load_config() -> RefdataRuntimeConfig:
    """Load and assemble the full runtime config from environment."""
    bootstrap = RefdataBootstrapConfig.from_env()

    venues_str = os.getenv("ZK_REFDATA_VENUES", "")
    venues = [v.strip() for v in venues_str.split(",") if v.strip()]
    venue_configs_path = os.getenv("ZK_REFDATA_VENUE_CONFIGS")

    # Vault client construction (if bootstrap provides Vault inputs)
    vault_client = None
    if bootstrap.vault_addr and (bootstrap.vault_token or bootstrap.vault_role_id):
        try:
            vault_client = _build_vault_client(bootstrap)
            logger.info("vault client initialized for secret resolution")
        except Exception as exc:
            logger.warning(f"vault client init failed, using dev env fallback: {exc}")

    rtc = assemble_runtime_config(
        bootstrap=bootstrap,
        venues=venues,
        venue_configs_path=venue_configs_path,
        vault_client=vault_client,
    )

    logger.info(
        f"config assembled | mode={rtc.mode} "
        f"venues={rtc.enabled_venues()} "
        f"logical_id={rtc.logical_id}"
    )
    return rtc


def _build_vault_client(bootstrap: RefdataBootstrapConfig):
    """Build an hvac Vault client from bootstrap inputs. Returns None on failure."""
    import hvac

    client = hvac.Client(url=bootstrap.vault_addr)
    if bootstrap.vault_token:
        client.token = bootstrap.vault_token
    elif bootstrap.vault_role_id and bootstrap.vault_secret_id:
        resp = client.auth.approle.login(
            role_id=bootstrap.vault_role_id,
            secret_id=bootstrap.vault_secret_id,
        )
        client.token = resp["auth"]["client_token"]

    if not client.is_authenticated():
        raise RuntimeError("vault client not authenticated")

    # Return a thin wrapper that matches the VaultClient protocol
    class _HvacWrapper:
        def __init__(self, hvac_client, mount_point="kv"):
            self._client = hvac_client
            self._mount = mount_point

        def read_secret(self, path: str) -> dict:
            # path is like "kv/zkbot/dev/venues/oanda/accounts/main"
            # strip the leading "kv/" to get the actual path
            actual_path = path.removeprefix("kv/")
            resp = self._client.secrets.kv.v2.read_secret_version(
                path=actual_path, mount_point=self._mount
            )
            return resp["data"]["data"]

    return _HvacWrapper(client)
```

- [ ] **Step 2: Update main.py to use new config flow and deferred registration**

Replace `main.py` with:

```python
# src/zk_refdata_svc/main.py
"""zk-refdata-svc entrypoint.

Startup sequence:
1. Load bootstrap config
2. Determine mode (direct / pilot)
3. Load provided config
4. Resolve secrets
5. Assemble runtime config
6. Initialize infra clients (PG, NATS, gRPC)
7. Run initial refresh for enabled venues
8. Register readiness (NATS KV) — only after step 7 succeeds
9. Enter steady state
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
from zk_refdata_svc.jobs.refresh_refdata import refresh_refdata
from zk_refdata_svc.jobs.refresh_sessions import refresh_sessions
from zk_refdata_svc.jobs.scheduler import run_periodic
from zk_refdata_svc.registry import ServiceRegistry
from zk_refdata_svc.repo import RefdataRepo

_MIGRATIONS_DIR = pathlib.Path(__file__).resolve().parent.parent.parent / "migrations"


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
    # -- Step 1-5: Config assembly (bootstrap + provided + secrets → runtime) --
    try:
        cfg = load_config()
    except (ConfigAssemblyError, KeyError) as exc:
        logger.error(f"config assembly failed, refusing to start: {exc}")
        sys.exit(1)

    logger.info(f"zk-refdata-svc starting | id={cfg.logical_id} port={cfg.grpc_port}")
    logger.debug(f"effective config (redacted): {cfg.redacted_dict()}")

    # -- Step 6: Initialize infra clients -----
    pg_pool = await asyncpg.create_pool(cfg.pg_url, min_size=2, max_size=10)
    logger.info("postgres pool connected")

    repo = RefdataRepo(pg_pool)
    await _apply_migrations(repo)

    nc = await nats.connect(cfg.nats_url)
    logger.info(f"nats connected to {cfg.nats_url}")

    # gRPC server
    servicer = RefdataRepo.__module__  # just to keep import
    from zk_refdata_svc.service import RefdataServicer

    servicer = RefdataServicer(repo)
    server = grpc.aio.server()
    refdata_pb2_grpc.add_RefdataServiceServicer_to_server(servicer, server)
    server.add_insecure_port(f"0.0.0.0:{cfg.grpc_port}")
    await server.start()
    logger.info(f"gRPC server listening on :{cfg.grpc_port}")

    # -- Step 7: Initial refresh for enabled venues -----
    background_tasks: list[asyncio.Task] = []
    enabled = cfg.enabled_venues()

    if enabled:
        logger.info(f"configured venues: {enabled}")

        # Build venue_configs dict from runtime config for refresh jobs
        venue_configs = {v: cfg.venue_resolved_config(v) for v in enabled}

        try:
            await refresh_refdata(repo, nc, enabled, venue_configs=venue_configs)
        except Exception:
            logger.exception("initial refdata refresh failed (will retry on schedule)")

        background_tasks.append(
            asyncio.create_task(
                run_periodic(
                    cfg.refresh_interval_s,
                    refresh_refdata,
                    repo,
                    nc,
                    enabled,
                    venue_configs=venue_configs,
                )
            )
        )
        background_tasks.append(
            asyncio.create_task(
                run_periodic(
                    cfg.refresh_interval_s,
                    refresh_sessions,
                    repo,
                    enabled,
                    venue_configs=venue_configs,
                )
            )
        )
    else:
        logger.info("no venues configured, refresh jobs disabled")

    # -- Step 8: Register readiness — ONLY after initial refresh -----
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
    )
    await registry.start(nc)
    logger.info("service registered in NATS KV (ready)")

    # -- Step 9: Wait for termination or fencing -----
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
```

- [ ] **Step 3: Verify existing tests still pass**

Run: `cd zkbot/services/zk-refdata-svc && uv run pytest tests/ -v --ignore=tests/test_service.py --ignore=tests/test_registry.py -x`
Expected: All PASS (test_service and test_registry need DB/NATS, skip if not available)

- [ ] **Step 4: Commit**

```bash
git add services/zk-refdata-svc/src/zk_refdata_svc/config.py services/zk-refdata-svc/src/zk_refdata_svc/main.py
git commit -m "feat(refdata): wire three-layer config model into startup sequence, defer registration until after initial refresh"
```

---

## Task 7: Add Startup Readiness / Early-Failure Tests

**Files:**
- Create: `services/zk-refdata-svc/tests/test_startup_readiness.py`

**Context:** These tests verify the startup ordering contract: config assembly must succeed before infra init, and the service must not report ready before initial refresh.

- [ ] **Step 1: Write tests for startup readiness contract**

```python
# tests/test_startup_readiness.py
"""Tests for startup ordering and early-failure behavior."""

from __future__ import annotations

import json
import pathlib

import pytest

from zk_refdata_svc.config import load_config
from zk_refdata_svc.config_loader import ConfigAssemblyError, assemble_runtime_config
from zk_refdata_svc.config_model import RefdataBootstrapConfig


class TestStartupEarlyFailure:
    def test_missing_nats_url_fails_early(self, monkeypatch):
        """Service must fail before any infra init if required env is missing."""
        monkeypatch.delenv("ZK_NATS_URL", raising=False)
        monkeypatch.delenv("ZK_PG_URL", raising=False)
        with pytest.raises(KeyError):
            load_config()

    def test_malformed_venue_config_fails_early(self, tmp_path, monkeypatch):
        """Malformed JSON config file should fail during config assembly."""
        bad_file = tmp_path / "bad.json"
        bad_file.write_text("{invalid json")
        monkeypatch.setenv("ZK_NATS_URL", "nats://localhost")
        monkeypatch.setenv("ZK_PG_URL", "postgres://localhost")
        monkeypatch.setenv("ZK_REFDATA_VENUES", "oanda")
        monkeypatch.setenv("ZK_REFDATA_VENUE_CONFIGS", str(bad_file))
        with pytest.raises(ConfigAssemblyError, match="config file"):
            load_config()

    def test_missing_required_secret_fails_early(self, tmp_path, monkeypatch):
        """If a venue needs secrets that can't be resolved, startup fails."""
        config_file = tmp_path / "venues.json"
        config_file.write_text(json.dumps({
            "oanda": {
                "environment": "practice",
                "account_id": "101",
                "secret_ref": "oanda/main",
            },
        }))
        monkeypatch.setenv("ZK_NATS_URL", "nats://localhost")
        monkeypatch.setenv("ZK_PG_URL", "postgres://localhost")
        monkeypatch.setenv("ZK_REFDATA_VENUES", "oanda")
        monkeypatch.setenv("ZK_REFDATA_VENUE_CONFIGS", str(config_file))
        # No vault, no ZK_SECRET_* env vars → should fail
        with pytest.raises(ConfigAssemblyError, match="oanda"):
            load_config()


class TestSecretRedaction:
    def test_secrets_not_in_redacted_output(self, tmp_path, monkeypatch):
        monkeypatch.setenv("ZK_SECRET_OANDA_MAIN_TOKEN", "super-secret")
        config_file = tmp_path / "venues.json"
        config_file.write_text(json.dumps({
            "oanda": {
                "environment": "practice",
                "account_id": "101",
                "secret_ref": "oanda/main",
            },
        }))
        bootstrap = RefdataBootstrapConfig(
            nats_url="nats://localhost",
            pg_url="postgres://localhost",
            env="dev",
        )
        rtc = assemble_runtime_config(
            bootstrap=bootstrap,
            venues=["oanda"],
            venue_configs_path=str(config_file),
            vault_client=None,
        )
        redacted = rtc.redacted_dict()
        redacted_str = json.dumps(redacted)
        assert "super-secret" not in redacted_str
        assert "***" in redacted_str
```

- [ ] **Step 2: Run tests**

Run: `cd zkbot/services/zk-refdata-svc && uv run pytest tests/test_startup_readiness.py -v`
Expected: All PASS

- [ ] **Step 3: Commit**

```bash
git add services/zk-refdata-svc/tests/test_startup_readiness.py
git commit -m "test(refdata): add startup readiness and early-failure tests"
```

---

## Task 8: Update OANDA Refdata Config Schema Documentation

**Files:**
- Modify: `venue-integrations/oanda/manifest.yaml`

**Context:** The OANDA manifest's refdata capability doesn't declare `field_descriptors` (unlike gw/rtmd). Add them so the host knows which fields are secret refs. The `secret_ref` field stays in the JSON schema (it's valid provided_config metadata) but the manifest now explicitly marks it.

- [ ] **Step 1: Update OANDA manifest to add refdata field_descriptors**

In `venue-integrations/oanda/manifest.yaml`, add `field_descriptors` to the `refdata` capability:

**Before:**
```yaml
  refdata:
    language: python
    entrypoint: python:oanda.refdata:OandaRefdataLoader
    config_schema: schemas/refdata_config.schema.json
```

**After:**
```yaml
  refdata:
    language: python
    entrypoint: python:oanda.refdata:OandaRefdataLoader
    config_schema: schemas/refdata_config.schema.json
    field_descriptors:
      - path: /secret_ref
        secret_ref: true
        reloadable: false
      - path: /environment
        reloadable: false
      - path: /account_id
        reloadable: false
      - path: /api_base_url
        reloadable: false
```

- [ ] **Step 2: Verify manifest loads correctly**

Run: `cd zkbot/services/zk-refdata-svc && uv run pytest tests/test_venue_registry.py -v -k "test_load_manifest"`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add venue-integrations/oanda/manifest.yaml
git commit -m "docs(oanda): add field_descriptors to refdata capability in manifest"
```

---

## Task 9: Add hvac Dependency (Optional, For Vault Support)

**Files:**
- Modify: `services/zk-refdata-svc/pyproject.toml`

**Context:** The Vault client (`hvac`) is needed for production secret resolution. It's already available in the broader project (`libs/zk-rpc` uses it). Add as optional dependency.

- [ ] **Step 1: Add hvac to optional dependencies**

In `pyproject.toml`, add to the existing dependencies:

```toml
[project.optional-dependencies]
dev = [
    "pytest>=8.0",
    "pytest-asyncio>=0.23",
]
vault = [
    "hvac>=2.1",
]
```

- [ ] **Step 2: Commit**

```bash
git add services/zk-refdata-svc/pyproject.toml
git commit -m "build(refdata): add hvac as optional vault dependency"
```

---

## Task 10: Run Full Validation

- [ ] **Step 1: Run all refdata-svc tests**

```bash
cd zkbot/services/zk-refdata-svc && uv run pytest tests/ -v --tb=short -x
```

Expected: All tests pass. If `test_service.py` or `test_registry.py` fail due to missing PG/NATS, that's expected in a non-devstack environment — the new tests should all pass.

- [ ] **Step 2: Run OANDA venue integration tests (if any exist)**

```bash
cd zkbot/venue-integrations/oanda && uv run pytest -v 2>/dev/null || echo "no tests or no test env"
```

- [ ] **Step 3: Summarize final state**

Document the following in the commit or PR description:
- **Config flow:** bootstrap (env) → provided (JSON file / future Pilot) → secret resolution (Vault / env fallback) → runtime config
- **Direct vs Pilot:** Direct mode reads `ZK_REFDATA_VENUES` + `ZK_REFDATA_VENUE_CONFIGS` from env. Pilot mode is stubbed (falls back to direct).
- **Secret resolution:** Manifest `field_descriptors` identify `secret_ref` fields → resolved via Vault (if available) or `ZK_SECRET_<REF>_<FIELD>` env vars → injected into resolved_config
- **OANDA:** Requires resolved `token` — fails early if missing. `secret_ref` fallback removed.
- **OKX:** Public-only, no auth needed — explicitly tested.
- **Readiness:** KV registration deferred until after config assembly + initial refresh
- **Remaining gaps:**
  - Pilot-mode loading is stubbed (needs Pilot API client)
  - `GetCurrentConfig` gRPC method not yet exposed (future)
  - Config reload/drift detection not yet wired

- [ ] **Step 4: Final commit (if any fixups needed)**

```bash
git add -A && git commit -m "chore(refdata): validation pass — all tests green"
```
