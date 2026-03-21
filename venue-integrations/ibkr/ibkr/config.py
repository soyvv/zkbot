"""Typed configuration for the IBKR gateway adaptor."""

from __future__ import annotations

from dataclasses import dataclass

_VALID_MODES = frozenset({"paper", "live"})


@dataclass(frozen=True)
class IbkrGwConfig:
    """IBKR gateway configuration.

    Constructed from the dict payload provided by the venue manifest / bridge.
    """

    host: str
    port: int
    client_id: int
    mode: str
    account_code: str = ""
    read_only: bool = False
    max_msg_rate: float = 40.0
    reconnect_delay_s: float = 5.0
    reconnect_max_delay_s: float = 60.0
    next_valid_id_timeout_s: float = 10.0

    def __post_init__(self) -> None:
        if self.mode not in _VALID_MODES:
            raise ValueError(f"mode must be one of {_VALID_MODES}, got {self.mode!r}")
        if self.port < 1:
            raise ValueError(f"port must be >= 1, got {self.port}")
        if self.client_id < 0:
            raise ValueError(f"client_id must be >= 0, got {self.client_id}")
        if self.max_msg_rate <= 0:
            raise ValueError(f"max_msg_rate must be > 0, got {self.max_msg_rate}")

    @classmethod
    def from_dict(cls, d: dict) -> IbkrGwConfig:
        """Build config from a plain dict, ignoring unknown keys."""
        known = {f.name for f in cls.__dataclass_fields__.values()}
        filtered = {k: v for k, v in d.items() if k in known}
        return cls(**filtered)
