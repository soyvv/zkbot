"""Tests for manifest-driven venue loader resolution."""

from __future__ import annotations

import pathlib
import textwrap

import pytest
import yaml

from zk_refdata_svc.venue_registry import (
    load_manifest,
    manifest_supports_sessions,
    resolve_refdata_loader,
    set_integrations_dir,
)

# Real venue-integrations directory (relative to this test file).
_TESTS_DIR = pathlib.Path(__file__).resolve().parent
_SVC_ROOT = _TESTS_DIR.parent
_REAL_INTEGRATIONS = _SVC_ROOT.parent.parent / "venue-integrations"


@pytest.fixture(autouse=True)
def _reset_integrations_dir():
    """Point venue_registry at the real venue-integrations dir for each test."""
    set_integrations_dir(_REAL_INTEGRATIONS)
    yield
    set_integrations_dir(_REAL_INTEGRATIONS)


# ---------------------------------------------------------------------------
# load_manifest
# ---------------------------------------------------------------------------


def test_load_manifest_valid():
    manifest = load_manifest("okx")
    assert manifest["venue"] == "okx"
    assert "refdata" in manifest["capabilities"]


def test_load_manifest_missing_venue():
    with pytest.raises(FileNotFoundError, match="no_such_venue"):
        load_manifest("no_such_venue")


# ---------------------------------------------------------------------------
# manifest_supports_sessions
# ---------------------------------------------------------------------------


def test_manifest_supports_sessions_false_for_crypto():
    manifest = load_manifest("okx")
    assert manifest_supports_sessions(manifest) is False


def test_manifest_supports_sessions_true_for_tradfi():
    manifest = load_manifest("ibkr")
    assert manifest_supports_sessions(manifest) is True


def test_manifest_supports_sessions_missing_metadata():
    assert manifest_supports_sessions({}) is False


# ---------------------------------------------------------------------------
# resolve_refdata_loader
# ---------------------------------------------------------------------------


def test_resolve_loader_success():
    loader = resolve_refdata_loader("okx")
    assert hasattr(loader, "load_instruments")


def test_resolve_loader_with_config():
    loader = resolve_refdata_loader("okx", config={"api_base_url": "https://example.com"})
    assert hasattr(loader, "load_instruments")


def test_resolve_loader_missing_refdata_capability(tmp_path: pathlib.Path):
    """Venue manifest exists but has no refdata capability."""
    venue_dir = tmp_path / "test_venue"
    venue_dir.mkdir()
    manifest = {"venue": "test_venue", "version": 1, "capabilities": {"gw": {}}}
    (venue_dir / "manifest.yaml").write_text(yaml.dump(manifest))

    set_integrations_dir(tmp_path)
    with pytest.raises(ValueError, match="no 'refdata' capability"):
        resolve_refdata_loader("test_venue")


def test_resolve_loader_non_python_language(tmp_path: pathlib.Path):
    """Venue manifest has refdata but language != python."""
    venue_dir = tmp_path / "test_venue"
    venue_dir.mkdir()
    manifest = {
        "venue": "test_venue",
        "version": 1,
        "capabilities": {"refdata": {"language": "rust", "entrypoint": "rust::foo"}},
    }
    (venue_dir / "manifest.yaml").write_text(yaml.dump(manifest))

    set_integrations_dir(tmp_path)
    with pytest.raises(ValueError, match="expected 'python'"):
        resolve_refdata_loader("test_venue")


def test_resolve_loader_invalid_entrypoint_format(tmp_path: pathlib.Path):
    """Entrypoint missing module:class separator."""
    venue_dir = tmp_path / "test_venue"
    venue_dir.mkdir()
    manifest = {
        "venue": "test_venue",
        "version": 1,
        "capabilities": {"refdata": {"language": "python", "entrypoint": "python:bad_format"}},
    }
    (venue_dir / "manifest.yaml").write_text(yaml.dump(manifest))

    set_integrations_dir(tmp_path)
    with pytest.raises(ValueError, match="invalid entrypoint format"):
        resolve_refdata_loader("test_venue")


def test_resolve_loader_module_not_found(tmp_path: pathlib.Path):
    """Entrypoint points to a non-existent module."""
    venue_dir = tmp_path / "test_venue"
    venue_dir.mkdir()
    manifest = {
        "venue": "test_venue",
        "version": 1,
        "capabilities": {
            "refdata": {
                "language": "python",
                "entrypoint": "python:nonexistent.module:Loader",
            }
        },
    }
    (venue_dir / "manifest.yaml").write_text(yaml.dump(manifest))

    set_integrations_dir(tmp_path)
    with pytest.raises(ValueError, match="could not import module"):
        resolve_refdata_loader("test_venue")


def test_resolve_loader_config_schema_validation_failure(tmp_path: pathlib.Path):
    """Config fails JSON schema validation."""
    import jsonschema

    venue_dir = tmp_path / "test_venue"
    venue_dir.mkdir()
    schemas_dir = venue_dir / "schemas"
    schemas_dir.mkdir()

    # Schema requires api_key as string
    schema = {
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {"api_key": {"type": "string"}},
        "required": ["api_key"],
    }
    import json

    (schemas_dir / "config.schema.json").write_text(json.dumps(schema))

    # Create a minimal valid Python module so entrypoint resolves
    pkg_dir = venue_dir / "test_pkg"
    pkg_dir.mkdir()
    (pkg_dir / "__init__.py").write_text("")
    (pkg_dir / "loader.py").write_text(
        textwrap.dedent("""\
        class TestLoader:
            def __init__(self, config=None):
                self.config = config
        """)
    )

    manifest = {
        "venue": "test_venue",
        "version": 1,
        "capabilities": {
            "refdata": {
                "language": "python",
                "entrypoint": "python:test_pkg.loader:TestLoader",
                "config_schema": "schemas/config.schema.json",
            }
        },
    }
    (venue_dir / "manifest.yaml").write_text(yaml.dump(manifest))

    set_integrations_dir(tmp_path)
    # Missing required "api_key" → validation error
    with pytest.raises(jsonschema.ValidationError):
        resolve_refdata_loader("test_venue", config={"wrong_field": 123})


def test_resolve_loader_schema_validated_when_config_is_none(tmp_path: pathlib.Path):
    """Schema with required fields should reject None config (treated as {})."""
    import jsonschema

    venue_dir = tmp_path / "test_venue"
    venue_dir.mkdir()
    schemas_dir = venue_dir / "schemas"
    schemas_dir.mkdir()

    schema = {
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {"api_key": {"type": "string"}},
        "required": ["api_key"],
    }
    import json

    (schemas_dir / "config.schema.json").write_text(json.dumps(schema))

    pkg_dir = venue_dir / "test_pkg"
    pkg_dir.mkdir()
    (pkg_dir / "__init__.py").write_text("")
    (pkg_dir / "loader.py").write_text(
        textwrap.dedent("""\
        class TestLoader:
            def __init__(self, config=None):
                self.config = config
        """)
    )

    manifest = {
        "venue": "test_venue",
        "version": 1,
        "capabilities": {
            "refdata": {
                "language": "python",
                "entrypoint": "python:test_pkg.loader:TestLoader",
                "config_schema": "schemas/config.schema.json",
            }
        },
    }
    (venue_dir / "manifest.yaml").write_text(yaml.dump(manifest))

    set_integrations_dir(tmp_path)
    # config=None should still be validated against schema as {}
    with pytest.raises(jsonschema.ValidationError):
        resolve_refdata_loader("test_venue")
