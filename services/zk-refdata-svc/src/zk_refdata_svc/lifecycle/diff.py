"""Lifecycle diff engine for instrument refdata.

Compares fetched (upstream) records against current (PostgreSQL) state
and classifies changes into: added, changed, disabled.
"""

from __future__ import annotations

from dataclasses import dataclass, field

from zk_refdata_svc.normalize.instruments import is_close

# Fields compared when detecting changes.  Metadata fields (updated_at,
# first_seen_at, last_seen_at, source_*) are excluded.
_COMPARE_FIELDS_STR = [
    "instrument_exch",
    "instrument_type",
    "base_asset",
    "quote_asset",
    "settlement_asset",
]
_COMPARE_FIELDS_NUM = [
    "contract_size",
    "price_tick_size",
    "qty_lot_size",
    "min_notional",
    "max_notional",
    "min_order_qty",
    "max_order_qty",
    "max_mkt_order_qty",
]
_COMPARE_FIELDS_INT = [
    "price_precision",
    "qty_precision",
]


@dataclass
class DiffResult:
    added: list[dict] = field(default_factory=list)
    changed: list[dict] = field(default_factory=list)
    disabled: list[str] = field(default_factory=list)


def diff_instruments(
    current: dict[str, dict],
    fetched: dict[str, dict],
) -> DiffResult:
    """Diff fetched vs current instrument maps (keyed by instrument_id).

    Raises ``ValueError`` if *fetched* contains duplicate instrument_ids.
    """
    result = DiffResult()

    for iid, rec in fetched.items():
        if iid in current:
            if _has_changes(current[iid], rec):
                result.changed.append(rec)
        else:
            result.added.append(rec)

    # Instruments present in current but absent from fetched, and currently
    # active (not already disabled), should be disabled.
    for iid, cur in current.items():
        if iid not in fetched and not cur.get("disabled", False):
            result.disabled.append(iid)

    return result


def check_duplicates(records: list[dict]) -> None:
    """Raise ValueError if any instrument_id appears more than once."""
    seen: set[str] = set()
    for rec in records:
        iid = rec["instrument_id"]
        if iid in seen:
            raise ValueError(f"duplicate instrument_id in fetch batch: {iid}")
        seen.add(iid)


def _has_changes(current: dict, fetched: dict) -> bool:
    for f in _COMPARE_FIELDS_STR:
        cv = current.get(f) or ""
        fv = fetched.get(f) or ""
        if cv != fv:
            return True
    for f in _COMPARE_FIELDS_NUM:
        cv = float(current.get(f) or 0)
        fv = float(fetched.get(f) or 0)
        if not is_close(cv, fv):
            return True
    for f in _COMPARE_FIELDS_INT:
        cv = int(current.get(f) or 0)
        fv = int(fetched.get(f) or 0)
        if cv != fv:
            return True
    # Compare extra_properties as normalized string maps.
    cur_extra = {str(k): str(v) for k, v in (current.get("extra_properties") or {}).items()}
    fet_extra = {str(k): str(v) for k, v in (fetched.get("extra_properties") or {}).items()}
    if cur_extra != fet_extra:
        return True
    return False
