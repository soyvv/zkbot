"""gRPC servicer implementation for RefdataService."""

from __future__ import annotations

import time

import grpc

from zk_refdata_svc import refdata_pb2, refdata_pb2_grpc


def _row_to_response(row: dict) -> refdata_pb2.InstrumentRefdataResponse:
    return refdata_pb2.InstrumentRefdataResponse(
        instrument_id=row["instrument_id"],
        venue=row["venue"],
        instrument_exch=row["instrument_exch"],
        instrument_type=row["instrument_type"],
        disabled=bool(row["disabled"]),
        price_tick_size=float(row["price_tick_size"] or 0.0),
        qty_lot_size=float(row["qty_lot_size"] or 0.0),
        base_asset=row.get("base_asset") or "",
        quote_asset=row.get("quote_asset") or "",
        updated_at_ms=int(row.get("updated_at_ms") or 0),
    )


class RefdataServicer(refdata_pb2_grpc.RefdataServiceServicer):
    def __init__(self, repo) -> None:
        # repo is RefdataRepo in production; any object with the same async methods in tests
        self._repo = repo

    async def QueryInstrumentById(
        self,
        request: refdata_pb2.QueryInstrumentByIdRequest,
        context: grpc.aio.ServicerContext,
    ) -> refdata_pb2.InstrumentRefdataResponse:
        row = await self._repo.query_by_id(request.instrument_id)
        if row is None:
            await context.abort(grpc.StatusCode.NOT_FOUND, f"instrument {request.instrument_id!r} not found")
        return _row_to_response(row)

    async def QueryInstrumentByVenueSymbol(
        self,
        request: refdata_pb2.QueryByVenueSymbolRequest,
        context: grpc.aio.ServicerContext,
    ) -> refdata_pb2.InstrumentRefdataResponse:
        row = await self._repo.query_by_venue_symbol(request.venue, request.instrument_exch)
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
        # Phase 6 stub: market session calendar not yet implemented
        return refdata_pb2.MarketStatusResponse(
            venue=request.venue,
            market=request.market,
            session_state="closed",
            effective_at_ms=int(time.time() * 1000),
        )
