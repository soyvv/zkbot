"""Tests for IbkrGwConfig."""

from __future__ import annotations

import pytest

from ibkr.config import IbkrGwConfig


class TestIbkrGwConfig:
    def test_from_dict_valid(self, sample_gw_config: dict) -> None:
        cfg = IbkrGwConfig.from_dict(sample_gw_config)
        assert cfg.host == "127.0.0.1"
        assert cfg.port == 7497
        assert cfg.client_id == 1
        assert cfg.account_code == "DU123456"
        assert cfg.mode == "paper"
        assert cfg.read_only is False

    def test_from_dict_defaults(self) -> None:
        cfg = IbkrGwConfig.from_dict({
            "host": "localhost",
            "port": 4001,
            "client_id": 0,
            "mode": "live",
        })
        assert cfg.account_code == ""
        assert cfg.read_only is False
        assert cfg.max_msg_rate == 40.0
        assert cfg.reconnect_delay_s == 5.0
        assert cfg.next_valid_id_timeout_s == 10.0

    def test_from_dict_ignores_unknown_keys(self) -> None:
        cfg = IbkrGwConfig.from_dict({
            "host": "h",
            "port": 1,
            "client_id": 0,
            "mode": "paper",
            "unknown_key": "value",
        })
        assert cfg.host == "h"

    def test_rejects_invalid_mode(self) -> None:
        with pytest.raises(ValueError, match="mode must be one of"):
            IbkrGwConfig.from_dict({
                "host": "h",
                "port": 1,
                "client_id": 0,
                "mode": "demo",
            })

    def test_rejects_invalid_port(self) -> None:
        with pytest.raises(ValueError, match="port must be >= 1"):
            IbkrGwConfig.from_dict({
                "host": "h",
                "port": 0,
                "client_id": 0,
                "mode": "paper",
            })

    def test_rejects_negative_client_id(self) -> None:
        with pytest.raises(ValueError, match="client_id must be >= 0"):
            IbkrGwConfig.from_dict({
                "host": "h",
                "port": 1,
                "client_id": -1,
                "mode": "paper",
            })

    def test_frozen(self, sample_gw_config: dict) -> None:
        cfg = IbkrGwConfig.from_dict(sample_gw_config)
        with pytest.raises(AttributeError):
            cfg.host = "other"  # type: ignore[misc]
