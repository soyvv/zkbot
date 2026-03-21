"""Translate between zkbot instrument strings and ib_async Contract objects."""

from __future__ import annotations

import logging

import ib_async

log = logging.getLogger(__name__)

# Instrument string convention: {symbol}-{currency}-{secType}-{exchange}
# Example: AAPL-USD-STK-SMART
# This is a first-pass convention; refdata service will provide canonical mappings later.
_REQUIRED_PARTS = 4


class ContractTranslator:
    """Bidirectional instrument <-> ib_async.Contract translation with caching."""

    def __init__(self) -> None:
        self._cache: dict[str, ib_async.Contract] = {}
        self._reverse_cache: dict[int, str] = {}  # conId -> instrument string

    def to_ib_contract(self, instrument: str) -> ib_async.Contract:
        """Parse a zkbot instrument string into an ib_async Contract.

        Format: ``{symbol}-{currency}-{secType}-{exchange}``

        Raises ValueError if the string cannot be parsed.
        """
        if instrument in self._cache:
            return self._cache[instrument]

        parts = instrument.split("-")
        if len(parts) < _REQUIRED_PARTS:
            raise ValueError(
                f"instrument must be {{symbol}}-{{currency}}-{{secType}}-{{exchange}}, "
                f"got {instrument!r}"
            )

        symbol, currency, sec_type, exchange = parts[0], parts[1], parts[2], parts[3]
        contract = ib_async.Contract(
            symbol=symbol,
            currency=currency,
            secType=sec_type,
            exchange=exchange,
        )
        self._cache[instrument] = contract
        return contract

    def from_ib_contract(self, contract: ib_async.Contract) -> str:
        """Convert an ib_async Contract to a zkbot instrument string.

        Uses the conId reverse cache if available, otherwise builds from fields.
        """
        if contract.conId and contract.conId in self._reverse_cache:
            return self._reverse_cache[contract.conId]

        instrument = (
            f"{contract.symbol}-{contract.currency}-{contract.secType}-{contract.exchange}"
        )
        if contract.conId:
            self._reverse_cache[contract.conId] = instrument
        return instrument

    async def qualify(self, ib: ib_async.IB, instrument: str) -> ib_async.Contract:
        """Qualify a contract via the IB API (fills in conId, etc).

        Qualifies once and caches the result. Subsequent calls for the same
        instrument return the cached qualified contract.
        """
        cached = self._cache.get(instrument)
        if cached and cached.conId:
            return cached

        contract = self.to_ib_contract(instrument)
        qualified = await ib.qualifyContractsAsync(contract)
        if not qualified:
            raise ValueError(f"failed to qualify contract for {instrument!r}")
        q = qualified[0]
        self._cache[instrument] = q
        if q.conId:
            self._reverse_cache[q.conId] = instrument
        return q

    def clear_cache(self) -> None:
        self._cache.clear()
        self._reverse_cache.clear()
