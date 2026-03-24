"""Tests for typed config model dataclasses."""

from __future__ import annotations

import json

import pytest

from zk_refdata_svc.config_model import (
    RefdataBootstrapConfig,
    RefdataRuntimeConfig,
    RefdataVenueProvidedConfig,
    VenueRuntimeConfig,
)


class TestRefdataBootstrapConfig:
    def test_from_env_minimal(self, monkeypatch):
        monkeypatch.setenv("ZK_NATS_URL", "nats://localhost:4222")
        monkeypatch.setenv("ZK_PG_URL", "postgres://localhost/zkbot")
        cfg = RefdataBootstrapConfig.from_env()
        assert cfg.nats_url == "nats://localhost:4222"
        assert cfg.pg_url == "postgres://localhost/zkbot"
        assert cfg.grpc_port == 50052
        assert cfg.logical_id == "refdata_dev_1"
        assert cfg.mode == "direct"

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
                "secret_ref": "8001",
            },
        }
        vpc = RefdataVenueProvidedConfig.from_dict(raw)
        assert vpc.venue == "oanda"
        assert vpc.enabled is True
        assert vpc.config["secret_ref"] == "8001"

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
            provided_config={"environment": "practice", "secret_ref": "8001"},
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
            provided_config={"secret_ref": "8001"},
            resolved_config={"token": "secret-value", "environment": "practice"},
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
        assert d["venues"]["oanda"]["resolved_config"]["environment"] == "practice"
        # Make sure the raw secret value doesn't appear anywhere
        assert "secret-value" not in json.dumps(d)
