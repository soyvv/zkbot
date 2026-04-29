"""Explicit universe resolver.

Used for legacy ``universe: [...]`` shorthand and for the
``universe_source.type == "explicit"`` discriminator.

Each ``items[]`` entry can be either:

  * **string** — the IBKR contract spec ``{symbol}-{currency}-{secType}-{routing}``.
    The loader will infer ``instrument_type`` from secType (STK→STOCK, FUT→FUTURE, …).
  * **dict** — ``{"spec": str, "instrument_type": str}`` where ``instrument_type``
    is the proto-aligned canonical short name (``ETF``, ``STOCK``, ``FUTURE``, …).
    Use the dict form when curating an ETF watch-list — bare strings can't carry
    that distinction since IBKR's ``STK`` secType doesn't separate stocks from ETFs.

Examples::

    universe_source:
      type: explicit
      config:
        items:
          - "AAPL-USD-STK-SMART"                              # → STOCK (default)
          - {"spec": "SPY-USD-STK-SMART", "instrument_type": "ETF"}
          - {"spec": "QQQ-USD-STK-SMART", "instrument_type": "ETF"}
"""

from __future__ import annotations

from collections.abc import Iterable
from typing import Any

from ibkr.resolvers.base import INST_TYPE_UNSPECIFIED, ResolvedInstrument

# Mirror of zk_utils.instrument_utils canonical names → proto int. Kept inline
# to avoid pulling zk-core into venue-integrations/ibkr.
_NAME_TO_PROTO_INT: dict[str, int] = {
    "UNSPECIFIED": 0, "SPOT": 1, "PERP": 2, "FUTURE": 3,
    "CFD": 4, "OPTION": 5, "ETF": 6, "STOCK": 7,
}


def _coerce_item(item: Any) -> ResolvedInstrument:
    if isinstance(item, str):
        return ResolvedInstrument(spec=item, instrument_type=INST_TYPE_UNSPECIFIED)
    if isinstance(item, dict):
        spec = item.get("spec")
        if not isinstance(spec, str) or not spec.strip():
            raise ValueError(f"explicit resolver item missing/empty 'spec': {item!r}")
        raw_type = item.get("instrument_type")
        if raw_type is None:
            return ResolvedInstrument(spec=spec, instrument_type=INST_TYPE_UNSPECIFIED)
        if isinstance(raw_type, int):
            inst_type = raw_type
        elif isinstance(raw_type, str):
            upper = raw_type.strip().upper()
            if upper not in _NAME_TO_PROTO_INT:
                raise ValueError(
                    f"explicit resolver: unknown instrument_type {raw_type!r} "
                    f"(valid: {sorted(_NAME_TO_PROTO_INT)})"
                )
            inst_type = _NAME_TO_PROTO_INT[upper]
        else:
            raise ValueError(
                f"explicit resolver: instrument_type must be str or int, "
                f"got {type(raw_type).__name__}"
            )
        return ResolvedInstrument(spec=spec, instrument_type=inst_type)
    raise ValueError(
        f"explicit resolver: items entries must be str or dict, got {type(item).__name__}"
    )


class ExplicitResolver:
    def __init__(self, config: dict | None = None) -> None:
        raw_items: Iterable[Any] = (config or {}).get("items", ())
        # Coerce + validate eagerly so config errors surface at construction
        # time, not on first resolve() call.
        self._resolved: list[ResolvedInstrument] = [_coerce_item(it) for it in raw_items]

    async def resolve(self) -> list[ResolvedInstrument]:
        return list(self._resolved)
