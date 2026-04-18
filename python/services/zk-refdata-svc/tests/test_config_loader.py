"""Tests for config assembly in direct and Pilot modes."""

from __future__ import annotations

import json
import pathlib
from unittest.mock import patch

import pytest

from zk_refdata_svc.config_model import RefdataBootstrapConfig, RefdataRuntimeConfig
from zk_refdata_svc.config_loader import (
    load_provided_configs_direct,
    assemble_runtime_config,
    ConfigAssemblyError,
)

# Descriptors matching the OANDA manifest refdata field_descriptors
_OANDA_DESCRIPTORS = [
    {"path": "/secret_ref", "secret_ref": True, "reloadable": False},
    {"path": "/environment", "reloadable": False},
]


def _mock_field_descriptors(venue: str) -> list[dict]:
    """Return field descriptors for test venues."""
    if venue == "oanda":
        return _OANDA_DESCRIPTORS
    return []


class TestLoadProvidedConfigsDirect:
    def test_loads_from_venues_and_configs_file(self, tmp_path: pathlib.Path):
        config_file = tmp_path / "venue_configs.json"
        config_file.write_text(json.dumps({
            "oanda": {
                "environment": "practice",
                "account_id": "101-001-123",
                "secret_ref": "8001",
            },
            "okx": {
                "api_base_url": "https://www.okx.com",
            },
        }))
        provided = load_provided_configs_direct(["oanda", "okx"], str(config_file))
        assert len(provided) == 2
        assert provided[0].venue == "oanda"
        assert provided[0].config["secret_ref"] == "8001"
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
    @patch("zk_refdata_svc.config_loader._load_field_descriptors", side_effect=_mock_field_descriptors)
    @patch("zk_refdata_svc.config_loader._validate_provided_config")
    def test_direct_mode_single_venue_no_secrets(self, mock_validate, mock_fd, tmp_path):
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

    @patch("zk_refdata_svc.config_loader._load_field_descriptors", side_effect=_mock_field_descriptors)
    @patch("zk_refdata_svc.config_loader._validate_provided_config")
    def test_direct_mode_secret_resolution_failure_raises(self, mock_validate, mock_fd, tmp_path):
        config_file = tmp_path / "venue_configs.json"
        config_file.write_text(json.dumps({
            "oanda": {
                "environment": "practice",
                "account_id": "101",
                "secret_ref": "8001",
            },
        }))
        bootstrap = RefdataBootstrapConfig(
            nats_url="nats://localhost:4222",
            pg_url="postgres://localhost/zkbot",
            mode="direct",
            env="dev",
        )
        # No vault client, no env vars → resolution should fail
        with pytest.raises(ConfigAssemblyError, match="oanda"):
            assemble_runtime_config(
                bootstrap=bootstrap,
                venues=["oanda"],
                venue_configs_path=str(config_file),
                vault_client=None,
            )

    @patch("zk_refdata_svc.config_loader._load_field_descriptors", side_effect=_mock_field_descriptors)
    @patch("zk_refdata_svc.config_loader._validate_provided_config")
    def test_direct_mode_with_dev_env_secrets(self, mock_validate, mock_fd, tmp_path, monkeypatch):
        monkeypatch.setenv("ZK_SECRET_8001_TOKEN", "dev-tok")
        config_file = tmp_path / "venue_configs.json"
        config_file.write_text(json.dumps({
            "oanda": {
                "environment": "practice",
                "account_id": "101",
                "secret_ref": "8001",
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
