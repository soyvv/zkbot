"""Tests for bootstrap client and pilot-mode config loading."""

from __future__ import annotations

import json

import pytest

from zk_refdata_svc.bootstrap_client import (
    BootstrapError,
    BootstrapGrant,
    _BootstrapRegisterRequest,
    _BootstrapRegisterResponse,
)
from zk_refdata_svc.config_loader import load_provided_configs_from_bootstrap


class TestBootstrapProtoRoundTrip:
    def test_request_roundtrip(self):
        req = _BootstrapRegisterRequest(
            token="dev-refdata-token-1",
            logical_id="refdata_dev_1",
            instance_type="REFDATA",
            env="dev",
        )
        data = bytes(req)
        parsed = _BootstrapRegisterRequest().parse(data)
        assert parsed.token == "dev-refdata-token-1"
        assert parsed.logical_id == "refdata_dev_1"
        assert parsed.instance_type == "REFDATA"
        assert parsed.env == "dev"

    def test_response_roundtrip_with_runtime_config(self):
        venue_configs = {
            "venues": [
                {"venue": "oanda", "enabled": True, "config": {"secret_ref": "8001"}},
                {"venue": "okx", "enabled": True, "config": {"api_base_url": "https://www.okx.com"}},
            ]
        }
        resp = _BootstrapRegisterResponse(
            owner_session_id="sess-123",
            kv_key="svc.refdata.refdata_dev_1",
            lock_key="lock.refdata.refdata_dev_1",
            lease_ttl_ms=30000,
            instance_id=0,
            status="OK",
            runtime_config=json.dumps(venue_configs),
        )
        data = bytes(resp)
        parsed = _BootstrapRegisterResponse().parse(data)
        assert parsed.status == "OK"
        assert parsed.owner_session_id == "sess-123"
        assert parsed.kv_key == "svc.refdata.refdata_dev_1"
        rc = json.loads(parsed.runtime_config)
        assert len(rc["venues"]) == 2
        assert rc["venues"][0]["venue"] == "oanda"

    def test_response_roundtrip_error(self):
        resp = _BootstrapRegisterResponse(
            status="TOKEN_EXPIRED",
            error_message="Token invalid",
        )
        data = bytes(resp)
        parsed = _BootstrapRegisterResponse().parse(data)
        assert parsed.status == "TOKEN_EXPIRED"
        assert parsed.error_message == "Token invalid"
        assert parsed.runtime_config == ""


class TestBootstrapGrant:
    def test_grant_fields(self):
        grant = BootstrapGrant(
            owner_session_id="sess-abc",
            kv_key="svc.refdata.refdata_dev_1",
            lock_key="lock.refdata.refdata_dev_1",
            lease_ttl_ms=30000,
            runtime_config={"venues": [{"venue": "oanda", "enabled": True, "config": {}}]},
        )
        assert grant.owner_session_id == "sess-abc"
        assert grant.kv_key == "svc.refdata.refdata_dev_1"
        assert len(grant.runtime_config["venues"]) == 1


class TestLoadProvidedConfigsFromBootstrap:
    def test_parses_venue_configs(self):
        runtime_config = {
            "venues": [
                {
                    "venue": "oanda",
                    "enabled": True,
                    "config": {"environment": "practice", "secret_ref": "8001"},
                },
                {
                    "venue": "okx",
                    "enabled": True,
                    "config": {"api_base_url": "https://www.okx.com"},
                },
            ]
        }
        result = load_provided_configs_from_bootstrap(runtime_config)
        assert len(result) == 2
        assert result[0].venue == "oanda"
        assert result[0].enabled is True
        assert result[0].config["secret_ref"] == "8001"
        assert result[1].venue == "okx"
        assert result[1].config["api_base_url"] == "https://www.okx.com"

    def test_empty_venues(self):
        assert load_provided_configs_from_bootstrap({"venues": []}) == []

    def test_missing_venues_key(self):
        assert load_provided_configs_from_bootstrap({}) == []

    def test_defaults_enabled_true(self):
        result = load_provided_configs_from_bootstrap(
            {"venues": [{"venue": "sim", "config": {}}]}
        )
        assert result[0].enabled is True

    def test_disabled_venue(self):
        result = load_provided_configs_from_bootstrap(
            {"venues": [{"venue": "sim", "enabled": False, "config": {}}]}
        )
        assert result[0].enabled is False


class TestConfigModelBootstrapToken:
    def test_bootstrap_token_from_env(self, monkeypatch):
        monkeypatch.setenv("ZK_NATS_URL", "nats://localhost:4222")
        monkeypatch.setenv("ZK_PG_URL", "postgres://localhost/zkbot")
        monkeypatch.setenv("ZK_MODE", "pilot")
        monkeypatch.setenv("ZK_BOOTSTRAP_TOKEN", "dev-refdata-token-1")

        from zk_refdata_svc.config_model import RefdataBootstrapConfig

        cfg = RefdataBootstrapConfig.from_env()
        assert cfg.mode == "pilot"
        assert cfg.bootstrap_token == "dev-refdata-token-1"

    def test_bootstrap_token_default_none(self, monkeypatch):
        monkeypatch.setenv("ZK_NATS_URL", "nats://localhost:4222")
        monkeypatch.setenv("ZK_PG_URL", "postgres://localhost/zkbot")
        monkeypatch.delenv("ZK_BOOTSTRAP_TOKEN", raising=False)

        from zk_refdata_svc.config_model import RefdataBootstrapConfig

        cfg = RefdataBootstrapConfig.from_env()
        assert cfg.bootstrap_token is None
