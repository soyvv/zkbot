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
        assert _dev_env_key("8001", "token") == "ZK_SECRET_8001_TOKEN"

    def test_ref_with_slash(self):
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
        monkeypatch.setenv("ZK_SECRET_8001_TOKEN", "dev-token-123")
        config = {
            "environment": "practice",
            "account_id": "101",
            "secret_ref": "8001",
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
        assert result["environment"] == "practice"

    def test_missing_required_secret_raises(self):
        config = {"secret_ref": "8001"}
        descriptors = [{"path": "/secret_ref", "secret_ref": True, "reloadable": False}]
        with pytest.raises(SecretResolutionError, match="8001"):
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

    def test_config_without_secret_ref_field_skips(self):
        """If manifest says secret_ref exists but config doesn't have it, skip."""
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
                if path == "trading/gw/8001":
                    return {"token": "vault-token-abc"}
                raise KeyError(f"no secret at {path}")

        config = {"environment": "live", "secret_ref": "8001"}
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
