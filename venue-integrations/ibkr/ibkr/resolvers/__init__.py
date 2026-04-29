"""Universe resolvers for IBKR refdata.

A resolver turns a source-specific candidate catalogue (explicit list, official
symbol directory, scanner output, etc.) into IBKR contract spec strings like
``AAPL-USD-STK-SMART``. The refdata loader then passes those specs through the
existing ``reqContractDetails`` qualification path.
"""

from ibkr.resolvers.base import (
    INST_TYPE_CFD,
    INST_TYPE_ETF,
    INST_TYPE_FUTURE,
    INST_TYPE_OPTION,
    INST_TYPE_PERP,
    INST_TYPE_SPOT,
    INST_TYPE_STOCK,
    INST_TYPE_UNSPECIFIED,
    ResolvedInstrument,
    UniverseFetchError,
    UniverseResolver,
)
from ibkr.resolvers.explicit import ExplicitResolver
from ibkr.resolvers.factory import build_resolver
from ibkr.resolvers.nasdaq_symbol_directory import NasdaqSymbolDirectoryResolver

__all__ = [
    "ExplicitResolver",
    "INST_TYPE_CFD",
    "INST_TYPE_ETF",
    "INST_TYPE_FUTURE",
    "INST_TYPE_OPTION",
    "INST_TYPE_PERP",
    "INST_TYPE_SPOT",
    "INST_TYPE_STOCK",
    "INST_TYPE_UNSPECIFIED",
    "NasdaqSymbolDirectoryResolver",
    "ResolvedInstrument",
    "UniverseFetchError",
    "UniverseResolver",
    "build_resolver",
]
