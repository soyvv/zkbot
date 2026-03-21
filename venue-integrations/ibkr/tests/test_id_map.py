"""Tests for OrderIdMap."""

from __future__ import annotations

from ibkr.id_map import OrderIdMap


class TestOrderIdMap:
    def test_register_and_lookup_by_client_id(self) -> None:
        m = OrderIdMap()
        entry = m.register(client_order_id=1, ib_order_id=100, instrument="AAPL")
        assert entry.client_order_id == 1
        assert entry.ib_order_id == 100
        assert entry.instrument == "AAPL"
        assert entry.perm_id is None

        found = m.lookup_by_client_id(1)
        assert found is entry

    def test_lookup_by_ib_order_id(self) -> None:
        m = OrderIdMap()
        m.register(client_order_id=1, ib_order_id=100)
        found = m.lookup_by_ib_order_id(100)
        assert found is not None
        assert found.client_order_id == 1

    def test_bind_perm_id_and_lookup(self) -> None:
        m = OrderIdMap()
        m.register(client_order_id=1, ib_order_id=100)
        entry = m.bind_perm_id(ib_order_id=100, perm_id=999)
        assert entry is not None
        assert entry.perm_id == 999

        found = m.lookup_by_perm_id(999)
        assert found is not None
        assert found.client_order_id == 1

    def test_bind_perm_id_unknown_ib_order_returns_none(self) -> None:
        m = OrderIdMap()
        assert m.bind_perm_id(ib_order_id=999, perm_id=1) is None

    def test_lookup_missing_returns_none(self) -> None:
        m = OrderIdMap()
        assert m.lookup_by_client_id(1) is None
        assert m.lookup_by_ib_order_id(1) is None
        assert m.lookup_by_perm_id(1) is None

    def test_remove_cleans_all_indices(self) -> None:
        m = OrderIdMap()
        m.register(client_order_id=1, ib_order_id=100)
        m.bind_perm_id(ib_order_id=100, perm_id=999)
        assert len(m) == 1

        m.remove(1)
        assert len(m) == 0
        assert m.lookup_by_client_id(1) is None
        assert m.lookup_by_ib_order_id(100) is None
        assert m.lookup_by_perm_id(999) is None

    def test_remove_nonexistent_is_noop(self) -> None:
        m = OrderIdMap()
        m.remove(42)  # should not raise

    def test_multiple_orders(self) -> None:
        m = OrderIdMap()
        m.register(1, 100, "AAPL")
        m.register(2, 101, "MSFT")
        m.bind_perm_id(100, 900)
        m.bind_perm_id(101, 901)

        assert m.lookup_by_perm_id(900).instrument == "AAPL"
        assert m.lookup_by_perm_id(901).instrument == "MSFT"
        assert len(m) == 2

    def test_overwrite_perm_id(self) -> None:
        m = OrderIdMap()
        m.register(1, 100)
        m.bind_perm_id(100, 900)
        # Overwrite with new perm_id
        m.bind_perm_id(100, 901)
        assert m.lookup_by_perm_id(901) is not None
        assert m.lookup_by_perm_id(900) is None  # old index removed
