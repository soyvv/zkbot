"""Manifest-driven venue loader resolution.

Resolves refdata loaders from venue-integrations manifests instead of
hardcoded class imports. Falls back gracefully when a venue has no
manifest (FileNotFoundError) or no refdata capability (VenueCapabilityNotFound).
"""

from __future__ import annotations

import importlib
import json
import pathlib
import sys
from typing import Any

import yaml
from loguru import logger

_DEFAULT_INTEGRATIONS_DIR: pathlib.Path | None = None


class VenueCapabilityNotFound(Exception):
    """Venue manifest exists but does not declare the requested capability."""


def _integrations_dir() -> pathlib.Path:
    """Return the venue-integrations root directory."""
    global _DEFAULT_INTEGRATIONS_DIR
    if _DEFAULT_INTEGRATIONS_DIR is not None:
        return _DEFAULT_INTEGRATIONS_DIR

    import os

    env = os.environ.get("ZK_VENUE_INTEGRATIONS_DIR")
    if env:
        _DEFAULT_INTEGRATIONS_DIR = pathlib.Path(env)
    else:
        # Auto-detect: service is at zkbot/python/services/zk-refdata-svc/src/zk_refdata_svc/...
        # venue-integrations is at zkbot/venue-integrations/ (top-level)
        here = pathlib.Path(__file__).resolve()
        # here.parents: 0=zk_refdata_svc, 1=src, 2=zk-refdata-svc, 3=services, 4=python, 5=zkbot
        _DEFAULT_INTEGRATIONS_DIR = here.parents[5] / "venue-integrations"
    return _DEFAULT_INTEGRATIONS_DIR


def set_integrations_dir(path: pathlib.Path) -> None:
    """Override the venue-integrations root (for testing)."""
    global _DEFAULT_INTEGRATIONS_DIR
    _DEFAULT_INTEGRATIONS_DIR = path


def load_manifest(venue: str) -> dict:
    """Load and return the parsed manifest.yaml for a venue.

    Raises FileNotFoundError if the venue directory or manifest does not exist.
    """
    manifest_path = _integrations_dir() / venue / "manifest.yaml"
    if not manifest_path.exists():
        raise FileNotFoundError(f"no manifest found for venue {venue!r}: {manifest_path}")
    with open(manifest_path) as f:
        return yaml.safe_load(f)


def manifest_supports_sessions(manifest: dict) -> bool:
    """Check if a venue manifest indicates TradFi session support."""
    metadata = manifest.get("metadata", {})
    return bool(metadata.get("supports_tradfi_sessions", False))


def resolve_refdata_loader(venue: str, config: dict | None = None) -> Any:
    """Resolve and instantiate a refdata loader from a venue manifest.

    1. Load manifest.yaml
    2. Validate refdata capability exists and language == python
    3. Parse entrypoint ``python:<module>:<class>``
    4. Validate config against declared JSON schema (if any)
    5. Instantiate and return the loader

    Raises:
        FileNotFoundError: no manifest for this venue
        VenueCapabilityNotFound: manifest exists but has no refdata capability
        ValueError: invalid refdata capability config (wrong language, bad entrypoint, etc.)
        jsonschema.ValidationError: config fails schema validation
    """
    manifest = load_manifest(venue)
    venue_dir = _integrations_dir() / venue

    capabilities = manifest.get("capabilities", {})
    refdata_cap = capabilities.get("refdata")
    if refdata_cap is None:
        raise VenueCapabilityNotFound(f"venue {venue!r} manifest has no 'refdata' capability")

    language = refdata_cap.get("language", "")
    if language != "python":
        raise ValueError(
            f"venue {venue!r} refdata capability has language={language!r}, expected 'python'"
        )

    entrypoint = refdata_cap.get("entrypoint", "")
    if not entrypoint.startswith("python:"):
        raise ValueError(f"venue {venue!r} refdata entrypoint must start with 'python:': {entrypoint!r}")

    # Parse "python:<module_path>:<class_name>"
    parts = entrypoint[len("python:"):].split(":")
    if len(parts) != 2 or not parts[0] or not parts[1]:
        raise ValueError(f"invalid entrypoint format {entrypoint!r}, expected 'python:<module>:<class>'")

    module_path, class_name = parts

    # Validate config against schema if declared
    config_schema_rel = refdata_cap.get("config_schema")
    if config_schema_rel:
        schema_path = venue_dir / config_schema_rel
        if schema_path.exists():
            import jsonschema

            with open(schema_path) as f:
                schema = json.load(f)
            jsonschema.validate(instance=config or {}, schema=schema)

    # Add venue package dir to sys.path so importlib can find the module
    venue_str = str(venue_dir)
    if venue_str not in sys.path:
        sys.path.insert(0, venue_str)

    try:
        mod = importlib.import_module(module_path)
    except ModuleNotFoundError as e:
        raise ValueError(
            f"could not import module {module_path!r} for venue {venue!r}: {e}"
        ) from e

    cls = getattr(mod, class_name, None)
    if cls is None:
        raise ValueError(
            f"class {class_name!r} not found in module {module_path!r} for venue {venue!r}"
        )

    logger.debug(f"resolved refdata loader for {venue!r}: {module_path}.{class_name}")
    return cls(config=config)


def validate_venue_config(venue: str, config: dict | None = None) -> None:
    """Validate venue config against the manifest-declared JSON schema.

    Use this to validate provided_config (which may contain secret_ref)
    before secret resolution. Does nothing if no schema is declared.

    Raises:
        FileNotFoundError: no manifest for this venue
        VenueCapabilityNotFound: manifest has no refdata capability
        jsonschema.ValidationError: config fails schema validation
    """
    manifest = load_manifest(venue)
    venue_dir = _integrations_dir() / venue
    refdata_cap = manifest.get("capabilities", {}).get("refdata")
    if refdata_cap is None:
        raise VenueCapabilityNotFound(f"venue {venue!r} manifest has no 'refdata' capability")

    config_schema_rel = refdata_cap.get("config_schema")
    if config_schema_rel:
        schema_path = venue_dir / config_schema_rel
        if schema_path.exists():
            import jsonschema

            with open(schema_path) as f:
                schema = json.load(f)
            jsonschema.validate(instance=config or {}, schema=schema)


def instantiate_refdata_loader(venue: str, config: dict | None = None):
    """Instantiate a refdata loader without schema validation.

    Use this when config has already been validated and secrets resolved.
    The resolved config (with token instead of secret_ref) would fail
    schema validation, so we skip it here.

    Raises:
        FileNotFoundError: no manifest for this venue
        VenueCapabilityNotFound: manifest has no refdata capability
        ValueError: invalid entrypoint or module not found
    """
    manifest = load_manifest(venue)
    venue_dir = _integrations_dir() / venue

    capabilities = manifest.get("capabilities", {})
    refdata_cap = capabilities.get("refdata")
    if refdata_cap is None:
        raise VenueCapabilityNotFound(f"venue {venue!r} manifest has no 'refdata' capability")

    language = refdata_cap.get("language", "")
    if language != "python":
        raise ValueError(
            f"venue {venue!r} refdata capability has language={language!r}, expected 'python'"
        )

    entrypoint = refdata_cap.get("entrypoint", "")
    if not entrypoint.startswith("python:"):
        raise ValueError(
            f"venue {venue!r} refdata entrypoint must start with 'python:': {entrypoint!r}"
        )

    parts = entrypoint[len("python:"):].split(":")
    if len(parts) != 2 or not parts[0] or not parts[1]:
        raise ValueError(
            f"invalid entrypoint format {entrypoint!r}, expected 'python:<module>:<class>'"
        )

    module_path, class_name = parts

    venue_str = str(venue_dir)
    if venue_str not in sys.path:
        sys.path.insert(0, venue_str)

    try:
        mod = importlib.import_module(module_path)
    except ModuleNotFoundError as e:
        raise ValueError(
            f"could not import module {module_path!r} for venue {venue!r}: {e}"
        ) from e

    cls = getattr(mod, class_name, None)
    if cls is None:
        raise ValueError(
            f"class {class_name!r} not found in module {module_path!r} for venue {venue!r}"
        )

    logger.debug(f"instantiated refdata loader for {venue!r}: {module_path}.{class_name}")
    return cls(config=config)
