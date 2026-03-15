"""Tests for RefdataServicer — RED phase: all tests must fail before implementation."""

import grpc
import grpc.aio
import pytest

from zk_refdata_svc import refdata_pb2
from zk_refdata_svc.repo import RefdataRepo
from zk_refdata_svc.service import RefdataServicer


# ── Helpers ────────────────────────────────────────────────────────────────────

async def _start_server(servicer: RefdataServicer) -> tuple[grpc.aio.Server, str]:
    """Start an in-process gRPC server; return (server, target_address)."""
    from zk_refdata_svc import refdata_pb2_grpc

    server = grpc.aio.server()
    refdata_pb2_grpc.add_RefdataServiceServicer_to_server(servicer, server)
    port = server.add_insecure_port("localhost:0")
    await server.start()
    return server, f"localhost:{port}"


# ── Unit tests (no DB — use in-memory stub repo) ───────────────────────────────

class StubRepo:
    """Minimal in-memory repo for unit tests."""

    def __init__(self, rows: list[dict]):
        self._rows = {r["instrument_id"]: r for r in rows}

    async def query_by_id(self, instrument_id: str) -> dict | None:
        return self._rows.get(instrument_id)

    async def query_by_venue_symbol(self, venue: str, instrument_exch: str) -> dict | None:
        for r in self._rows.values():
            if r["venue"] == venue and r["instrument_exch"] == instrument_exch:
                return r
        return None

    async def list_instruments(self, venue: str | None, enabled_only: bool) -> list[dict]:
        rows = list(self._rows.values())
        if venue:
            rows = [r for r in rows if r["venue"] == venue]
        if enabled_only:
            rows = [r for r in rows if not r["disabled"]]
        return rows

    async def watermark_ms(self) -> int:
        return 1_700_000_000_000


_BTCUSDT_ROW = {
    "instrument_id": "BTCUSDT_MOCK",
    "venue": "MOCK",
    "instrument_exch": "BTC-USDT",
    "instrument_type": "SPOT",
    "disabled": False,
    "price_tick_size": 0.01,
    "qty_lot_size": 0.00001,
    "base_asset": "BTC",
    "quote_asset": "USDT",
    "updated_at_ms": 1_700_000_000_000,
}


@pytest.fixture
def stub_repo():
    return StubRepo([_BTCUSDT_ROW])


@pytest.fixture
async def refdata_channel(stub_repo):
    servicer = RefdataServicer(stub_repo)
    server, target = await _start_server(servicer)
    channel = grpc.aio.insecure_channel(target)
    yield channel
    await channel.close()
    await server.stop(grace=0)


# ── Test: QueryInstrumentByVenueSymbol ─────────────────────────────────────────

async def test_query_by_venue_symbol_found(refdata_channel):
    from zk_refdata_svc import refdata_pb2_grpc

    stub = refdata_pb2_grpc.RefdataServiceStub(refdata_channel)
    resp = await stub.QueryInstrumentByVenueSymbol(
        refdata_pb2.QueryByVenueSymbolRequest(venue="MOCK", instrument_exch="BTC-USDT")
    )
    assert resp.instrument_id == "BTCUSDT_MOCK"
    assert resp.venue == "MOCK"
    assert resp.instrument_exch == "BTC-USDT"
    assert resp.instrument_type == "SPOT"
    assert resp.disabled is False
    assert abs(resp.price_tick_size - 0.01) < 1e-9
    assert abs(resp.qty_lot_size - 0.00001) < 1e-9


async def test_query_by_venue_symbol_not_found_raises_not_found(refdata_channel):
    from zk_refdata_svc import refdata_pb2_grpc

    stub = refdata_pb2_grpc.RefdataServiceStub(refdata_channel)
    with pytest.raises(grpc.aio.AioRpcError) as exc_info:
        await stub.QueryInstrumentByVenueSymbol(
            refdata_pb2.QueryByVenueSymbolRequest(venue="MOCK", instrument_exch="UNKNOWN")
        )
    assert exc_info.value.code() == grpc.StatusCode.NOT_FOUND


# ── Test: QueryInstrumentById ──────────────────────────────────────────────────

async def test_query_by_id_found(refdata_channel):
    from zk_refdata_svc import refdata_pb2_grpc

    stub = refdata_pb2_grpc.RefdataServiceStub(refdata_channel)
    resp = await stub.QueryInstrumentById(
        refdata_pb2.QueryInstrumentByIdRequest(instrument_id="BTCUSDT_MOCK")
    )
    assert resp.instrument_id == "BTCUSDT_MOCK"
    assert resp.base_asset == "BTC"


async def test_query_by_id_not_found_raises_not_found(refdata_channel):
    from zk_refdata_svc import refdata_pb2_grpc

    stub = refdata_pb2_grpc.RefdataServiceStub(refdata_channel)
    with pytest.raises(grpc.aio.AioRpcError) as exc_info:
        await stub.QueryInstrumentById(
            refdata_pb2.QueryInstrumentByIdRequest(instrument_id="NONEXISTENT")
        )
    assert exc_info.value.code() == grpc.StatusCode.NOT_FOUND


# ── Test: ListInstruments ──────────────────────────────────────────────────────

async def test_list_instruments_by_venue(refdata_channel):
    from zk_refdata_svc import refdata_pb2_grpc

    stub = refdata_pb2_grpc.RefdataServiceStub(refdata_channel)
    resp = await stub.ListInstruments(
        refdata_pb2.ListInstrumentsRequest(venue="MOCK", enabled_only=True)
    )
    assert len(resp.instruments) == 1
    assert resp.instruments[0].instrument_id == "BTCUSDT_MOCK"


async def test_list_instruments_no_venue_filter_returns_all(refdata_channel):
    from zk_refdata_svc import refdata_pb2_grpc

    stub = refdata_pb2_grpc.RefdataServiceStub(refdata_channel)
    resp = await stub.ListInstruments(refdata_pb2.ListInstrumentsRequest())
    assert len(resp.instruments) == 1


async def test_list_instruments_enabled_only_excludes_disabled(stub_repo):
    stub_repo._rows["DISABLED_MOCK"] = {
        "instrument_id": "DISABLED_MOCK",
        "venue": "MOCK",
        "instrument_exch": "DIS-USDT",
        "instrument_type": "SPOT",
        "disabled": True,
        "price_tick_size": 0.1,
        "qty_lot_size": 1.0,
        "base_asset": "DIS",
        "quote_asset": "USDT",
        "updated_at_ms": 1_700_000_000_000,
    }
    servicer = RefdataServicer(stub_repo)
    server, target = await _start_server(servicer)
    channel = grpc.aio.insecure_channel(target)
    try:
        from zk_refdata_svc import refdata_pb2_grpc

        stub = refdata_pb2_grpc.RefdataServiceStub(channel)
        resp = await stub.ListInstruments(
            refdata_pb2.ListInstrumentsRequest(enabled_only=True)
        )
        ids = [i.instrument_id for i in resp.instruments]
        assert "BTCUSDT_MOCK" in ids
        assert "DISABLED_MOCK" not in ids
    finally:
        await channel.close()
        await server.stop(grace=0)


# ── Test: QueryRefdataWatermark ────────────────────────────────────────────────

async def test_query_watermark_returns_ms(refdata_channel):
    from zk_refdata_svc import refdata_pb2_grpc

    stub = refdata_pb2_grpc.RefdataServiceStub(refdata_channel)
    resp = await stub.QueryRefdataWatermark(refdata_pb2.QueryWatermarkRequest())
    assert resp.watermark_ms == 1_700_000_000_000


# ── Test: QueryMarketStatus ────────────────────────────────────────────────────

async def test_market_status_returns_stub_closed(refdata_channel):
    from zk_refdata_svc import refdata_pb2_grpc

    stub = refdata_pb2_grpc.RefdataServiceStub(refdata_channel)
    resp = await stub.QueryMarketStatus(
        refdata_pb2.QueryMarketStatusRequest(venue="MOCK", market="MOCK")
    )
    assert resp.session_state == "closed"
    assert resp.venue == "MOCK"
    assert resp.market == "MOCK"
