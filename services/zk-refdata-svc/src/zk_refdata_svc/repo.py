"""Database queries for cfg.instrument_refdata using asyncpg."""

from __future__ import annotations

import asyncpg


class RefdataRepo:
    def __init__(self, pool: asyncpg.Pool) -> None:
        self._pool = pool

    async def query_by_id(self, instrument_id: str) -> dict | None:
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                SELECT instrument_id, venue, instrument_exch, instrument_type,
                       disabled, price_tick_size, qty_lot_size,
                       base_asset, quote_asset,
                       extract(epoch from updated_at) * 1000 as updated_at_ms
                FROM cfg.instrument_refdata
                WHERE instrument_id = $1
                """,
                instrument_id,
            )
        return dict(row) if row else None

    async def query_by_venue_symbol(self, venue: str, instrument_exch: str) -> dict | None:
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                SELECT instrument_id, venue, instrument_exch, instrument_type,
                       disabled, price_tick_size, qty_lot_size,
                       base_asset, quote_asset,
                       extract(epoch from updated_at) * 1000 as updated_at_ms
                FROM cfg.instrument_refdata
                WHERE venue = $1 AND instrument_exch = $2
                """,
                venue,
                instrument_exch,
            )
        return dict(row) if row else None

    async def list_instruments(self, venue: str | None, enabled_only: bool) -> list[dict]:
        conditions = []
        params: list = []

        if venue:
            params.append(venue)
            conditions.append(f"venue = ${len(params)}")
        if enabled_only:
            conditions.append("disabled = false")

        where = ("WHERE " + " AND ".join(conditions)) if conditions else ""
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                f"""
                SELECT instrument_id, venue, instrument_exch, instrument_type,
                       disabled, price_tick_size, qty_lot_size,
                       base_asset, quote_asset,
                       extract(epoch from updated_at) * 1000 as updated_at_ms
                FROM cfg.instrument_refdata
                {where}
                ORDER BY instrument_id
                """,
                *params,
            )
        return [dict(r) for r in rows]

    async def watermark_ms(self) -> int:
        """Return max(updated_at) across all instrument rows as epoch-ms."""
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT extract(epoch from max(updated_at)) * 1000 AS ms FROM cfg.instrument_refdata"
            )
        return int(row["ms"]) if row and row["ms"] else 0
