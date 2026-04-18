"""Instrument record normalization and precision utilities.

Ported from zk-refdata-loader's loader._enrich_record and utils.
"""

from __future__ import annotations

import math


def normalize_instrument(record: dict) -> dict:
    """Normalize a raw instrument dict from a venue loader.

    Applies safe defaults for missing numeric fields and coerces types
    so the record is ready for PostgreSQL insertion.
    """
    record["price_precision"] = _to_int(record.get("price_precision"), 0)
    record["qty_precision"] = _to_int(record.get("qty_precision"), 0)
    record["contract_size"] = _to_float(record.get("contract_size"), 1.0)
    record["price_tick_size"] = _to_float(record.get("price_tick_size"), 0.0)
    record["qty_lot_size"] = _to_float(record.get("qty_lot_size"), 0.0)
    record["min_notional"] = _to_float(record.get("min_notional"), 0.0)
    record["max_notional"] = _to_float(record.get("max_notional"), 0.0)
    record["min_order_qty"] = _to_float(record.get("min_order_qty"), 0.0)
    record["max_order_qty"] = _to_float(record.get("max_order_qty"), 0.0)
    record["max_mkt_order_qty"] = _to_float(record.get("max_mkt_order_qty"), 0.0)
    record.setdefault("settlement_asset", None)
    record.setdefault("disabled", False)

    # Stringify extra_properties values.
    ep = record.get("extra_properties")
    if ep and isinstance(ep, dict):
        record["extra_properties"] = {str(k): str(v) for k, v in ep.items()}
    else:
        record["extra_properties"] = {}

    return record


# -- Precision helpers -------------------------------------------------------


def int_precision(value: float) -> int:
    """Return number of decimal places needed for a float step size.

    >>> int_precision(0.01)
    2
    >>> int_precision(0.001)
    3
    """
    if value <= 0:
        return 0
    return max(0, -int(math.floor(math.log10(abs(value)))))


def float_precision(precision_int: int) -> float:
    """Convert an integer precision to a float step size.

    >>> float_precision(2)
    0.01
    """
    if precision_int <= 0:
        return 1.0
    return 10.0 ** (-precision_int)


def is_close(a: float, b: float, precision: int = 12) -> bool:
    """Fuzzy float comparison with configurable precision."""
    return round(a - b, precision) == 0


# -- Internal helpers --------------------------------------------------------


def _to_float(v, default: float) -> float:
    if v is None:
        return default
    try:
        return float(v)
    except (ValueError, TypeError):
        return default


def _to_int(v, default: int) -> int:
    if v is None:
        return default
    try:
        return int(v)
    except (ValueError, TypeError):
        return default
