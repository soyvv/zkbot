"""Shared instrument type classification constants and helpers.

Used by backtester, OMS core, balance manager, and test utilities to ensure
consistent treatment of spot-like vs margin instrument types.
"""

from zk_datamodel import common

# Instrument types that behave like spot: full-value trading, position tracked by base_asset.
SPOT_LIKE_TYPES = frozenset({
    common.InstrumentType.INST_TYPE_SPOT,
    common.InstrumentType.INST_TYPE_ETF,
    common.InstrumentType.INST_TYPE_STOCK,
})

# Instrument types that use margin: position tracked by instrument_id,
# fund symbol uses settlement_asset.
MARGIN_TYPES = frozenset({
    common.InstrumentType.INST_TYPE_PERP,
    common.InstrumentType.INST_TYPE_FUTURE,
    common.InstrumentType.INST_TYPE_CFD,
    common.InstrumentType.INST_TYPE_OPTION,
})

# Map from symbol suffix to instrument type.
# Spot-like instruments have no suffix; derivatives get suffixes.
SUFFIX_TO_TYPE = {
    "-CFD": common.InstrumentType.INST_TYPE_CFD,
    "-P": common.InstrumentType.INST_TYPE_PERP,
    "-F": common.InstrumentType.INST_TYPE_FUTURE,
}


def infer_instrument_type_from_symbol(base: str) -> common.InstrumentType:
    """Infer instrument type from the base portion of a symbol.

    Returns INST_TYPE_SPOT if no derivative suffix is found.
    """
    for suffix, inst_type in SUFFIX_TO_TYPE.items():
        if base.endswith(suffix):
            return inst_type
    return common.InstrumentType.INST_TYPE_SPOT
