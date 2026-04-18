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

    The JSON file maps venue name -> config dict. Each venue in the venues
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


def load_provided_configs_from_bootstrap(
    runtime_config: dict,
) -> list[RefdataVenueProvidedConfig]:
    """Parse venue provided configs from Pilot bootstrap response.

    The runtime_config dict has shape: {"venues": [{"venue": "...", "enabled": ..., "config": {...}}, ...]}
    """
    venues_data = runtime_config.get("venues", [])
    return [
        RefdataVenueProvidedConfig(
            venue=v["venue"],
            enabled=v.get("enabled", True),
            config=v.get("config", {}),
        )
        for v in venues_data
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


def _validate_provided_config(venue: str, config: dict) -> None:
    """Validate provided_config against venue manifest schema.

    This validates the original config (with secret_ref), not the resolved config.
    Silently skips if no manifest or schema found.
    """
    try:
        from zk_refdata_svc.venue_registry import validate_venue_config, VenueCapabilityNotFound

        validate_venue_config(venue, config)
    except FileNotFoundError:
        logger.debug(f"no manifest for {venue!r}, skipping schema validation")
    except VenueCapabilityNotFound:
        logger.debug(f"venue {venue!r} has no refdata capability, skipping schema validation")
    except Exception as exc:
        raise ConfigAssemblyError(
            f"config validation failed for venue={venue!r}: {exc}"
        ) from exc


def assemble_runtime_config(
    *,
    bootstrap: RefdataBootstrapConfig,
    venues: list[str],
    venue_configs_path: str | None,
    vault_client: VaultClient | None,
    bootstrap_runtime_config: dict | None = None,
) -> RefdataRuntimeConfig:
    """Assemble the full runtime config from bootstrap + provided + resolved secrets.

    In pilot mode, bootstrap_runtime_config contains venue configs from Pilot.
    In direct mode, venues + venue_configs_path are used.

    Raises ConfigAssemblyError on:
    - malformed config file
    - config schema validation failure
    - secret resolution failure for any enabled venue
    """
    # Step 1: Load provided configs
    try:
        if bootstrap.mode == "pilot" and bootstrap_runtime_config is not None:
            provided_list = load_provided_configs_from_bootstrap(bootstrap_runtime_config)
        else:
            provided_list = load_provided_configs_direct(venues, venue_configs_path)
    except (json.JSONDecodeError, OSError) as exc:
        raise ConfigAssemblyError(f"failed to load config file: {exc}") from exc

    # Step 2: Validate, resolve secrets, and build venue runtime configs
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

        # Validate provided_config against schema (before secret resolution)
        _validate_provided_config(vpc.venue, vpc.config)

        # Load field_descriptors to know which fields are secret refs
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
