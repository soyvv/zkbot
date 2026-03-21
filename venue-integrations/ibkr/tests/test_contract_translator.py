"""Tests for ContractTranslator."""

from __future__ import annotations

import pytest

from ibkr.ibkr_contracts import ContractTranslator


class TestContractTranslator:
    def test_to_ib_contract_stock(self) -> None:
        ct = ContractTranslator()
        c = ct.to_ib_contract("AAPL-USD-STK-SMART")
        assert c.symbol == "AAPL"
        assert c.currency == "USD"
        assert c.secType == "STK"
        assert c.exchange == "SMART"

    def test_to_ib_contract_future(self) -> None:
        ct = ContractTranslator()
        c = ct.to_ib_contract("ES-USD-FUT-CME")
        assert c.symbol == "ES"
        assert c.secType == "FUT"
        assert c.exchange == "CME"

    def test_to_ib_contract_invalid_format(self) -> None:
        ct = ContractTranslator()
        with pytest.raises(ValueError, match="instrument must be"):
            ct.to_ib_contract("AAPL")

    def test_to_ib_contract_cached(self) -> None:
        ct = ContractTranslator()
        c1 = ct.to_ib_contract("AAPL-USD-STK-SMART")
        c2 = ct.to_ib_contract("AAPL-USD-STK-SMART")
        assert c1 is c2  # same object from cache

    def test_from_ib_contract(self) -> None:
        ct = ContractTranslator()
        # Use ib_async.Contract via the translator's own output
        c = ct.to_ib_contract("MSFT-USD-STK-SMART")
        result = ct.from_ib_contract(c)
        assert result == "MSFT-USD-STK-SMART"

    def test_from_ib_contract_with_conid_cache(self) -> None:
        ct = ContractTranslator()
        c = ct.to_ib_contract("AAPL-USD-STK-SMART")
        # Simulate qualification by setting conId
        c.conId = 265598
        instrument = ct.from_ib_contract(c)
        assert instrument == "AAPL-USD-STK-SMART"
        # Second call should hit reverse cache
        instrument2 = ct.from_ib_contract(c)
        assert instrument2 == "AAPL-USD-STK-SMART"

    def test_clear_cache(self) -> None:
        ct = ContractTranslator()
        ct.to_ib_contract("AAPL-USD-STK-SMART")
        ct.clear_cache()
        # After clear, a new object should be created
        c = ct.to_ib_contract("AAPL-USD-STK-SMART")
        assert c.symbol == "AAPL"

    def test_extra_parts_ignored(self) -> None:
        """Extra dashes in the string are ignored (only first 4 parts used)."""
        ct = ContractTranslator()
        c = ct.to_ib_contract("AAPL-USD-STK-SMART-EXTRA")
        assert c.symbol == "AAPL"
        assert c.exchange == "SMART"
