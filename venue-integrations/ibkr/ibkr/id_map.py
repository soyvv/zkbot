"""Order ID triple mapping: client_order_id <-> ib_order_id <-> permId."""

from __future__ import annotations

import logging
from dataclasses import dataclass, field

log = logging.getLogger(__name__)


@dataclass
class OrderIdEntry:
    """A single order's ID linkage."""

    client_order_id: int
    ib_order_id: int | None = None
    perm_id: int | None = None
    instrument: str = ""


class OrderIdMap:
    """Bidirectional triple-index for order ID translation.

    Supports lookup by any of the three ID spaces:
    - client_order_id (our correlation_id)
    - ib_order_id (session-scoped TWS orderId)
    - perm_id (durable venue-native identifier)
    """

    def __init__(self) -> None:
        self._by_client_id: dict[int, OrderIdEntry] = {}
        self._by_ib_order_id: dict[int, int] = {}  # ib_order_id -> client_order_id
        self._by_perm_id: dict[int, int] = {}  # perm_id -> client_order_id

    def register(
        self, client_order_id: int, ib_order_id: int, instrument: str = ""
    ) -> OrderIdEntry:
        """Register a new order at placement time."""
        entry = OrderIdEntry(
            client_order_id=client_order_id,
            ib_order_id=ib_order_id,
            instrument=instrument,
        )
        self._by_client_id[client_order_id] = entry
        self._by_ib_order_id[ib_order_id] = client_order_id
        return entry

    def bind_perm_id(self, ib_order_id: int, perm_id: int) -> OrderIdEntry | None:
        """Bind the durable permId once it arrives from openOrder callback.

        Returns the updated entry, or None if the ib_order_id is unknown.
        """
        client_id = self._by_ib_order_id.get(ib_order_id)
        if client_id is None:
            log.warning(
                "bind_perm_id: unknown ib_order_id=%d perm_id=%d", ib_order_id, perm_id
            )
            return None
        entry = self._by_client_id[client_id]
        if entry.perm_id is not None and entry.perm_id != perm_id:
            log.warning(
                "bind_perm_id: overwriting perm_id %d -> %d for client_order_id=%d",
                entry.perm_id,
                perm_id,
                client_id,
            )
            # Remove old perm_id index
            self._by_perm_id.pop(entry.perm_id, None)
        entry.perm_id = perm_id
        self._by_perm_id[perm_id] = client_id
        return entry

    def lookup_by_client_id(self, client_order_id: int) -> OrderIdEntry | None:
        return self._by_client_id.get(client_order_id)

    def lookup_by_ib_order_id(self, ib_order_id: int) -> OrderIdEntry | None:
        client_id = self._by_ib_order_id.get(ib_order_id)
        if client_id is None:
            return None
        return self._by_client_id.get(client_id)

    def lookup_by_perm_id(self, perm_id: int) -> OrderIdEntry | None:
        client_id = self._by_perm_id.get(perm_id)
        if client_id is None:
            return None
        return self._by_client_id.get(client_id)

    def remove(self, client_order_id: int) -> None:
        """Remove an order from all indices."""
        entry = self._by_client_id.pop(client_order_id, None)
        if entry is None:
            return
        if entry.ib_order_id is not None:
            self._by_ib_order_id.pop(entry.ib_order_id, None)
        if entry.perm_id is not None:
            self._by_perm_id.pop(entry.perm_id, None)

    def __len__(self) -> int:
        return len(self._by_client_id)
