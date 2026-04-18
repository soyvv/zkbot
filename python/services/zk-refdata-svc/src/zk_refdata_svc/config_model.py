"""Typed config model for zk-refdata-svc.

Three-layer config model:
- bootstrap_config: minimal env/deployment-owned startup inputs
- provided_config: Pilot-managed or direct-mode venue-scoped config
- runtime_config: effective assembled config with resolved secrets
"""

from __future__ import annotations

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
    bootstrap_token: str | None = None  # for pilot mode
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
            bootstrap_token=os.getenv("ZK_BOOTSTRAP_TOKEN"),
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
_SECRET_FIELDS = {
    "token", "api_key", "api_secret", "passphrase", "secret_key", "private_key",
    "apikey", "secretkey",
}


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
