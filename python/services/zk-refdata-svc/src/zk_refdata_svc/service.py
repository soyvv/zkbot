"""gRPC servicer implementation for RefdataService."""

from __future__ import annotations

import time

import grpc

from zk_refdata_svc import refdata_pb2, refdata_pb2_grpc


def _row_to_response(row: dict) -> refdata_pb2.InstrumentRefdataResponse:
    """Build proto response from a repo row dict, disclosure-aware."""
    kwargs: dict = {
        "instrument_id": row["instrument_id"],
        "venue": row["venue"],
        "instrument_exch": row["instrument_exch"],
        "instrument_type": row["instrument_type"],
        "disabled": bool(row["disabled"]),
        "price_tick_size": float(row["price_tick_size"] or 0.0),
        "qty_lot_size": float(row["qty_lot_size"] or 0.0),
        "base_asset": row.get("base_asset") or "",
        "quote_asset": row.get("quote_asset") or "",
        "updated_at_ms": int(row.get("updated_at_ms") or 0),
    }
    # EXTENDED fields (present when disclosure level >= EXTENDED)
    if "lifecycle_status" in row:
        kwargs["lifecycle_status"] = row.get("lifecycle_status") or ""
        kwargs["settlement_asset"] = row.get("settlement_asset") or ""
        kwargs["contract_size"] = float(row.get("contract_size") or 0.0)
        kwargs["min_notional"] = float(row.get("min_notional") or 0.0)
        kwargs["max_notional"] = float(row.get("max_notional") or 0.0)
        kwargs["min_order_qty"] = float(row.get("min_order_qty") or 0.0)
        kwargs["max_order_qty"] = float(row.get("max_order_qty") or 0.0)
        kwargs["max_mkt_order_qty"] = float(row.get("max_mkt_order_qty") or 0.0)
        kwargs["price_precision"] = int(row.get("price_precision") or 0)
        kwargs["qty_precision"] = int(row.get("qty_precision") or 0)
        # extra_properties: PG returns dict (jsonb), proto expects map<string,string>
        ep = row.get("extra_properties") or {}
        kwargs["extra_properties"] = {str(k): str(v) for k, v in ep.items()}
        kwargs["first_seen_at_ms"] = int(row.get("first_seen_at_ms") or 0)
        kwargs["last_seen_at_ms"] = int(row.get("last_seen_at_ms") or 0)
    # FULL fields (present when disclosure level == FULL)
    if "source_name" in row:
        kwargs["source_name"] = row.get("source_name") or ""
        kwargs["source_run_id"] = int(row.get("source_run_id") or 0)
    return refdata_pb2.InstrumentRefdataResponse(**kwargs)


class RefdataServicer(refdata_pb2_grpc.RefdataServiceServicer):
    def __init__(self, repo) -> None:
        self._repo = repo

    async def QueryInstrumentById(
        self,
        request: refdata_pb2.QueryInstrumentByIdRequest,
        context: grpc.aio.ServicerContext,
    ) -> refdata_pb2.InstrumentRefdataResponse:
        row = await self._repo.query_by_id(request.instrument_id, request.level)
        if row is None:
            await context.abort(
                grpc.StatusCode.NOT_FOUND,
                f"instrument {request.instrument_id!r} not found",
            )
        return _row_to_response(row)

    async def QueryInstrumentByVenueSymbol(
        self,
        request: refdata_pb2.QueryByVenueSymbolRequest,
        context: grpc.aio.ServicerContext,
    ) -> refdata_pb2.InstrumentRefdataResponse:
        row = await self._repo.query_by_venue_symbol(
            request.venue, request.instrument_exch, request.level
        )
        if row is None:
            await context.abort(
                grpc.StatusCode.NOT_FOUND,
                f"instrument ({request.venue!r}, {request.instrument_exch!r}) not found",
            )
        return _row_to_response(row)

    async def ListInstruments(
        self,
        request: refdata_pb2.ListInstrumentsRequest,
        context: grpc.aio.ServicerContext,
    ) -> refdata_pb2.ListInstrumentsResponse:
        rows = await self._repo.list_instruments(
            venue=request.venue or None,
            enabled_only=request.enabled_only,
            level=request.level,
        )
        return refdata_pb2.ListInstrumentsResponse(
            instruments=[_row_to_response(r) for r in rows]
        )

    async def QueryRefdataWatermark(
        self,
        request: refdata_pb2.QueryWatermarkRequest,
        context: grpc.aio.ServicerContext,
    ) -> refdata_pb2.WatermarkResponse:
        ms = await self._repo.watermark_ms()
        return refdata_pb2.WatermarkResponse(watermark_ms=ms)

    async def QueryMarketStatus(
        self,
        request: refdata_pb2.QueryMarketStatusRequest,
        context: grpc.aio.ServicerContext,
    ) -> refdata_pb2.MarketStatusResponse:
        row = await self._repo.query_market_status(request.venue, request.market)
        if row:
            return refdata_pb2.MarketStatusResponse(
                venue=row["venue"],
                market=row["market"],
                session_state=row["session_state"],
                effective_at_ms=int(row["effective_at_ms"] or 0),
            )
        # Fallback: no session state recorded yet.
        return refdata_pb2.MarketStatusResponse(
            venue=request.venue,
            market=request.market,
            session_state="closed",
            effective_at_ms=int(time.time() * 1000),
        )

    async def QueryMarketCalendar(
        self,
        request: refdata_pb2.QueryMarketCalendarRequest,
        context: grpc.aio.ServicerContext,
    ) -> refdata_pb2.QueryMarketCalendarResponse:
        if not request.start_date or not request.end_date:
            await context.abort(
                grpc.StatusCode.INVALID_ARGUMENT,
                "start_date and end_date are required",
            )
        rows = await self._repo.query_market_calendar(
            request.venue, request.market, request.start_date, request.end_date
        )
        entries = [
            refdata_pb2.MarketCalendarEntry(
                venue=r["venue"],
                market=r["market"],
                date=r["date"],
                session_state=r["session_state"],
                open_time_ms=int(r.get("open_time_ms") or 0),
                close_time_ms=int(r.get("close_time_ms") or 0),
                source=r.get("source") or "",
            )
            for r in rows
        ]
        return refdata_pb2.QueryMarketCalendarResponse(entries=entries)
