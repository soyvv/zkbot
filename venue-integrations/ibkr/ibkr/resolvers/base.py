"""Resolver protocol and shared exceptions."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Protocol, runtime_checkable

# Mirror of `zk.common.v1.InstrumentType` proto enum values. The IBKR
# venue-integration deliberately avoids importing the proto module here to
# keep the resolver layer dependency-light; values are stable.
# Source of truth: zkbot/protos/zk/common/v1/common.proto and
# zkbot/python/libs/zk-core/src/zk_utils/instrument_utils.py.
INST_TYPE_UNSPECIFIED = 0
INST_TYPE_SPOT = 1
INST_TYPE_PERP = 2
INST_TYPE_FUTURE = 3
INST_TYPE_CFD = 4
INST_TYPE_OPTION = 5
INST_TYPE_ETF = 6
INST_TYPE_STOCK = 7


class UniverseFetchError(RuntimeError):
    """Raised when a resolver cannot produce any candidates.

    The refdata refresh job catches this and marks the run failed,
    leaving existing DB rows untouched.
    """


@dataclass(frozen=True)
class ResolvedInstrument:
    """A single resolver emission. ``spec`` is the IBKR-native four-part
    contract string (``{symbol}-{currency}-{secType}-{routing}``).
    ``instrument_type`` is the proto enum int — when ``UNSPECIFIED``, the
    loader falls back to inferring from the qualified contract's secType.
    Resolvers that know the type with certainty (e.g. Nasdaq directory
    distinguishes ETF from STOCK) should set it explicitly.
    """

    spec: str
    instrument_type: int = INST_TYPE_UNSPECIFIED


@runtime_checkable
class UniverseResolver(Protocol):
    async def resolve(self) -> list[ResolvedInstrument]:
        """Return resolved IBKR contract emissions, e.g. spec ``AAPL-USD-STK-SMART``."""
        ...
