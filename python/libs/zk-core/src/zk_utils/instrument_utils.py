"""Shared instrument type classification constants and helpers.

The proto enum ``zk.common.v1.InstrumentType`` (mirrored as
``zk_proto_betterproto.common.InstrumentType``) is the single source of truth.

DB column ``cfg.instrument_refdata.instrument_type`` stores the canonical
proto short name (UPPERCASE: ``SPOT``, ``PERP``, ``FUTURE``, ``CFD``,
``OPTION``, ``ETF``, ``STOCK``, ``UNSPECIFIED``) — exact match to
``InstrumentType.Name(v).removeprefix("INST_TYPE_")``.

Conversion at the proto boundary goes through this module; comparisons
elsewhere should use the enum value (Python/Rust) or the canonical UPPERCASE
name (DB/UI/REST). Avoid lowercase / venue-specific aliases anywhere.
"""

from __future__ import annotations

from zk_proto_betterproto import common

InstrumentType = common.InstrumentType  # re-export for convenience


class InvalidInstrumentType(ValueError):
    """Raised by ``to_proto`` / ``normalize`` when the input does not match
    any proto-defined ``InstrumentType`` short name."""


# -- canonical-name helpers --------------------------------------------------

# {6: "ETF", 7: "STOCK", ...} — keyed by the int value of the enum.
_INT_TO_CANONICAL: dict[int, str] = {
    member.value: member.name.removeprefix("INST_TYPE_")
    for member in InstrumentType
}

# {"ETF": 6, "STOCK": 7, ...} — keyed by the canonical UPPERCASE short name.
_CANONICAL_TO_INT: dict[str, int] = {
    name: value for value, name in _INT_TO_CANONICAL.items()
}


def from_proto(value: int | InstrumentType) -> str:
    """Convert a proto enum int (or member) to the canonical UPPERCASE short name.

    >>> from_proto(InstrumentType.INST_TYPE_ETF)
    'ETF'
    >>> from_proto(7)
    'STOCK'
    """
    int_value = int(value)
    name = _INT_TO_CANONICAL.get(int_value)
    if name is None:
        raise InvalidInstrumentType(f"unknown InstrumentType int value: {int_value!r}")
    return name


def to_proto(value: str) -> int:
    """Convert a canonical name (case-insensitive on input) to the proto int.

    >>> to_proto("ETF")
    6
    >>> to_proto("etf")
    6
    >>> to_proto("STOCK")
    7
    """
    if not isinstance(value, str):
        raise InvalidInstrumentType(f"expected str, got {type(value).__name__}")
    upper = value.strip().upper()
    if upper not in _CANONICAL_TO_INT:
        raise InvalidInstrumentType(
            f"unknown InstrumentType name: {value!r} "
            f"(valid: {sorted(_CANONICAL_TO_INT)})"
        )
    return _CANONICAL_TO_INT[upper]


def normalize(value: str) -> str:
    """Return the canonical UPPERCASE form of *value*. The form to put in the DB.

    >>> normalize("etf")
    'ETF'
    >>> normalize("Stock")
    'STOCK'
    """
    return from_proto(to_proto(value))


# -- spot-like vs margin classification (existing) ---------------------------

# Instrument types that behave like spot: full-value trading, position tracked by base_asset.
SPOT_LIKE_TYPES = frozenset({
    InstrumentType.INST_TYPE_SPOT,
    InstrumentType.INST_TYPE_ETF,
    InstrumentType.INST_TYPE_STOCK,
})

# Instrument types that use margin: position tracked by instrument_id,
# fund symbol uses settlement_asset.
MARGIN_TYPES = frozenset({
    InstrumentType.INST_TYPE_PERP,
    InstrumentType.INST_TYPE_FUTURE,
    InstrumentType.INST_TYPE_CFD,
    InstrumentType.INST_TYPE_OPTION,
})

# Map from symbol suffix to instrument type.
# Spot-like instruments have no suffix; derivatives get suffixes.
SUFFIX_TO_TYPE = {
    "-CFD": InstrumentType.INST_TYPE_CFD,
    "-P": InstrumentType.INST_TYPE_PERP,
    "-F": InstrumentType.INST_TYPE_FUTURE,
}


def infer_instrument_type_from_symbol(base: str) -> InstrumentType:
    """Infer instrument type from the base portion of a symbol.

    Returns INST_TYPE_SPOT if no derivative suffix is found.
    """
    for suffix, inst_type in SUFFIX_TO_TYPE.items():
        if base.endswith(suffix):
            return inst_type
    return InstrumentType.INST_TYPE_SPOT
