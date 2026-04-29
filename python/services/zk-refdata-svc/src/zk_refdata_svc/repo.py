"""Database queries for cfg.instrument_refdata and supporting tables."""

from __future__ import annotations

import json
from datetime import datetime, timezone

import asyncpg

# -- Column sets for progressive disclosure -----------------------------------

_BASIC_COLS = """
    instrument_id, venue, instrument_exch, instrument_type,
    disabled, price_tick_size, qty_lot_size,
    base_asset, quote_asset,
    extract(epoch from updated_at) * 1000 as updated_at_ms
"""

_EXTENDED_COLS = f"""{_BASIC_COLS},
    lifecycle_status, settlement_asset, contract_size,
    min_notional, max_notional, min_order_qty, max_order_qty,
    max_mkt_order_qty, price_precision, qty_precision,
    extra_properties,
    extract(epoch from first_seen_at) * 1000 as first_seen_at_ms,
    extract(epoch from last_seen_at) * 1000 as last_seen_at_ms
"""

_FULL_COLS = f"""{_EXTENDED_COLS},
    source_name, source_run_id
"""

_DISCLOSURE_MAP = {
    0: _BASIC_COLS,    # UNSPECIFIED -> BASIC
    1: _BASIC_COLS,    # BASIC
    2: _EXTENDED_COLS,  # EXTENDED
    3: _FULL_COLS,      # FULL
}

# All columns needed for diff comparison (used by refresh job).
_ALL_COLS = _FULL_COLS


def _cols_for_level(level: int) -> str:
    return _DISCLOSURE_MAP.get(level, _BASIC_COLS)


def _json_str(obj: dict | None) -> str:
    return json.dumps(obj) if obj else "{}"


class RefdataRepo:
    def __init__(self, pool: asyncpg.Pool) -> None:
        self._pool = pool

    def acquire(self) -> asyncpg.pool.PoolAcquireContext:
        """Acquire a connection from the pool (for transactional operations)."""
        return self._pool.acquire()

    # -- Query methods --------------------------------------------------------

    async def query_by_id(self, instrument_id: str, level: int = 1) -> dict | None:
        cols = _cols_for_level(level)
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                f"SELECT {cols} FROM cfg.instrument_refdata WHERE instrument_id = $1",
                instrument_id,
            )
        return dict(row) if row else None

    async def query_by_venue_symbol(
        self, venue: str, instrument_exch: str, level: int = 1
    ) -> dict | None:
        cols = _cols_for_level(level)
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                f"SELECT {cols} FROM cfg.instrument_refdata"
                " WHERE venue = $1 AND instrument_exch = $2",
                venue,
                instrument_exch,
            )
        return dict(row) if row else None

    async def list_instruments(
        self, venue: str | None, enabled_only: bool, level: int = 1
    ) -> list[dict]:
        cols = _cols_for_level(level)
        conditions: list[str] = []
        params: list = []

        if venue:
            params.append(venue)
            conditions.append(f"venue = ${len(params)}")
        if enabled_only:
            conditions.append("disabled = false")

        where = ("WHERE " + " AND ".join(conditions)) if conditions else ""
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                f"SELECT {cols} FROM cfg.instrument_refdata {where} ORDER BY instrument_id",
                *params,
            )
        return [dict(r) for r in rows]

    async def watermark_ms(self) -> int:
        """Return max(updated_at) across all instrument rows as epoch-ms."""
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT extract(epoch from max(updated_at)) * 1000 AS ms"
                " FROM cfg.instrument_refdata"
            )
        return int(row["ms"]) if row and row["ms"] else 0

    async def query_market_status(self, venue: str, market: str) -> dict | None:
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT venue, market, session_state,"
                " extract(epoch from effective_at) * 1000 as effective_at_ms"
                " FROM cfg.market_session_state WHERE venue = $1 AND market = $2",
                venue,
                market,
            )
        return dict(row) if row else None

    async def query_market_calendar(
        self, venue: str, market: str, start_date: str, end_date: str
    ) -> list[dict]:
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                "SELECT venue, market, date::text, session_state,"
                " extract(epoch from open_time) * 1000 as open_time_ms,"
                " extract(epoch from close_time) * 1000 as close_time_ms,"
                " source"
                " FROM cfg.market_session_calendar"
                " WHERE venue = $1 AND market = $2 AND date >= $3::date AND date <= $4::date"
                " ORDER BY date",
                venue,
                market,
                start_date,
                end_date,
            )
        return [dict(r) for r in rows]

    # -- Lifecycle write methods (used by refresh jobs) -----------------------

    async def get_instruments_by_venue(self, venue: str) -> list[dict]:
        """Load all instruments for a venue (for diff comparison)."""
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                f"SELECT {_ALL_COLS} FROM cfg.instrument_refdata WHERE venue = $1",
                venue,
            )
        return [dict(r) for r in rows]

    async def upsert_instruments(
        self, records: list[dict], run_id: int, conn: asyncpg.Connection
    ) -> None:
        """Insert or update instrument records within an existing transaction."""
        now = datetime.now(timezone.utc)
        for rec in records:
            await conn.execute(
                """
                INSERT INTO cfg.instrument_refdata (
                    instrument_id, instrument_exch, venue, instrument_type,
                    base_asset, quote_asset, settlement_asset, contract_size,
                    price_tick_size, qty_lot_size, min_notional, max_notional,
                    min_order_qty, max_order_qty, max_mkt_order_qty,
                    price_precision, qty_precision, disabled, extra_properties,
                    lifecycle_status, first_seen_at, last_seen_at,
                    source_name, source_run_id, updated_at
                ) VALUES (
                    $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                    $11, $12, $13, $14, $15, $16, $17, $18,
                    $19::jsonb, $20, $21, $21, $22, $23, $21
                )
                ON CONFLICT (instrument_id) DO UPDATE SET
                    instrument_exch = EXCLUDED.instrument_exch,
                    instrument_type = EXCLUDED.instrument_type,
                    base_asset = EXCLUDED.base_asset,
                    quote_asset = EXCLUDED.quote_asset,
                    settlement_asset = EXCLUDED.settlement_asset,
                    contract_size = EXCLUDED.contract_size,
                    price_tick_size = EXCLUDED.price_tick_size,
                    qty_lot_size = EXCLUDED.qty_lot_size,
                    min_notional = EXCLUDED.min_notional,
                    max_notional = EXCLUDED.max_notional,
                    min_order_qty = EXCLUDED.min_order_qty,
                    max_order_qty = EXCLUDED.max_order_qty,
                    max_mkt_order_qty = EXCLUDED.max_mkt_order_qty,
                    price_precision = EXCLUDED.price_precision,
                    qty_precision = EXCLUDED.qty_precision,
                    disabled = EXCLUDED.disabled,
                    extra_properties = EXCLUDED.extra_properties,
                    lifecycle_status = EXCLUDED.lifecycle_status,
                    last_seen_at = EXCLUDED.last_seen_at,
                    source_name = EXCLUDED.source_name,
                    source_run_id = EXCLUDED.source_run_id,
                    updated_at = EXCLUDED.updated_at
                """,
                rec["instrument_id"],
                rec.get("instrument_exch", ""),
                rec.get("venue", ""),
                rec.get("instrument_type", ""),
                rec.get("base_asset"),
                rec.get("quote_asset"),
                rec.get("settlement_asset"),
                rec.get("contract_size"),
                rec.get("price_tick_size"),
                rec.get("qty_lot_size"),
                rec.get("min_notional"),
                rec.get("max_notional"),
                rec.get("min_order_qty"),
                rec.get("max_order_qty"),
                rec.get("max_mkt_order_qty"),
                rec.get("price_precision"),
                rec.get("qty_precision"),
                rec.get("disabled", False),
                _json_str(rec.get("extra_properties", {})),
                rec.get("lifecycle_status", "active"),
                now,
                rec.get("venue", ""),
                run_id,
            )

    async def disable_instruments(
        self, instrument_ids: list[str], run_id: int, conn: asyncpg.Connection
    ) -> None:
        """Mark instruments as disabled within an existing transaction."""
        if not instrument_ids:
            return
        now = datetime.now(timezone.utc)
        await conn.executemany(
            """
            UPDATE cfg.instrument_refdata
            SET lifecycle_status = 'disabled', disabled = true,
                last_seen_at = $2, source_run_id = $3, updated_at = $2
            WHERE instrument_id = $1
            """,
            [(iid, now, run_id) for iid in instrument_ids],
        )

    async def insert_refresh_run(self, source_name: str, venue: str) -> int:
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                "INSERT INTO cfg.refdata_refresh_run (source_name, venue)"
                " VALUES ($1, $2) RETURNING run_id",
                source_name,
                venue,
            )
        return row["run_id"]

    async def complete_refresh_run(
        self,
        run_id: int,
        status: str,
        *,
        added: int = 0,
        updated: int = 0,
        disabled: int = 0,
        deprecated: int = 0,
        error_detail: str | None = None,
    ) -> None:
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                UPDATE cfg.refdata_refresh_run
                SET ended_at = now(), status = $2,
                    instruments_added = $3, instruments_updated = $4,
                    instruments_disabled = $5, instruments_deprecated = $6,
                    error_detail = $7
                WHERE run_id = $1
                """,
                run_id,
                status,
                added,
                updated,
                disabled,
                deprecated,
                error_detail,
            )

    async def get_refresh_run(self, run_id: int) -> dict | None:
        """Fetch a single refresh run by id. Returns None if not found."""
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                SELECT run_id, venue, status,
                       instruments_added, instruments_updated, instruments_disabled,
                       error_detail,
                       (EXTRACT(EPOCH FROM started_at) * 1000)::bigint AS started_at_ms,
                       (EXTRACT(EPOCH FROM ended_at)   * 1000)::bigint AS ended_at_ms
                FROM cfg.refdata_refresh_run
                WHERE run_id = $1
                """,
                run_id,
            )
        return dict(row) if row else None

    async def find_running_run_for_venue(self, venue: str) -> dict | None:
        """Return the most recent in-flight (status='running') run for a venue."""
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                SELECT run_id, venue, status,
                       instruments_added, instruments_updated, instruments_disabled,
                       error_detail,
                       (EXTRACT(EPOCH FROM started_at) * 1000)::bigint AS started_at_ms,
                       (EXTRACT(EPOCH FROM ended_at)   * 1000)::bigint AS ended_at_ms
                FROM cfg.refdata_refresh_run
                WHERE venue = $1 AND status = 'running'
                ORDER BY run_id DESC
                LIMIT 1
                """,
                venue,
            )
        return dict(row) if row else None

    async def insert_change_events(
        self, events: list[dict], conn: asyncpg.Connection
    ) -> None:
        if not events:
            return
        await conn.executemany(
            """
            INSERT INTO cfg.refdata_change_event
                (run_id, instrument_id, venue, change_class, watermark_ms)
            VALUES ($1, $2, $3, $4, $5)
            """,
            [
                (
                    e["run_id"],
                    e["instrument_id"],
                    e["venue"],
                    e["change_class"],
                    e["watermark_ms"],
                )
                for e in events
            ],
        )

    async def upsert_market_session_state(
        self, venue: str, market: str, session_state: str
    ) -> None:
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO cfg.market_session_state (venue, market, session_state, effective_at)
                VALUES ($1, $2, $3, now())
                ON CONFLICT (venue, market) DO UPDATE SET
                    session_state = EXCLUDED.session_state,
                    effective_at = EXCLUDED.effective_at,
                    updated_at = now()
                """,
                venue,
                market,
                session_state,
            )

    # -- Migration helper -----------------------------------------------------

    async def run_migration(self, sql: str) -> None:
        async with self._pool.acquire() as conn:
            await conn.execute(sql)
