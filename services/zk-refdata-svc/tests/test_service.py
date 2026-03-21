"""Tests for RefdataServicer with progressive disclosure and market calendar."""

import grpc
import grpc.aio
import pytest

from zk_refdata_svc import refdata_pb2
from zk_refdata_svc.service import RefdataServicer


# -- Helpers ------------------------------------------------------------------

async def _start_server(servicer: RefdataServicer) -> tuple[grpc.aio.Server, str]:
    from zk_refdata_svc import refdata_pb2_grpc

    server = grpc.aio.server()
    refdata_pb2_grpc.add_RefdataServiceServicer_to_server(servicer, server)
    port = server.add_insecure_port("localhost:0")
    await server.start()
    return server, f"localhost:{port}"


# -- StubRepo -----------------------------------------------------------------

class StubRepo:
    """In-memory repo for unit tests, disclosure-aware."""

    def __init__(self, rows: list[dict]):
        self._rows = {r["instrument_id"]: r for r in rows}
        self._market_status: dict[tuple[str, str], dict] = {}
        self._market_calendar: list[dict] = []

    def _filter(self, row: dict, level: int) -> dict:
        basic_keys = {
            "instrument_id", "venue", "instrument_exch", "instrument_type",
            "disabled", "price_tick_size", "qty_lot_size",
            "base_asset", "quote_asset", "updated_at_ms",
        }
        extended_keys = basic_keys | {
            "lifecycle_status", "settlement_asset", "contract_size",
            "min_notional", "max_notional", "min_order_qty", "max_order_qty",
            "max_mkt_order_qty", "price_precision", "qty_precision",
            "extra_properties", "first_seen_at_ms", "last_seen_at_ms",
        }
        full_keys = extended_keys | {"source_name", "source_run_id"}
        if level >= 3:
            keys = full_keys
        elif level >= 2:
            keys = extended_keys
        else:
            keys = basic_keys
        return {k: v for k, v in row.items() if k in keys}

    async def query_by_id(self, instrument_id: str, level: int = 1) -> dict | None:
        row = self._rows.get(instrument_id)
        return self._filter(row, level) if row else None

    async def query_by_venue_symbol(
        self, venue: str, instrument_exch: str, level: int = 1
    ) -> dict | None:
        for r in self._rows.values():
            if r["venue"] == venue and r["instrument_exch"] == instrument_exch:
                return self._filter(r, level)
        return None

    async def list_instruments(
        self, venue: str | None, enabled_only: bool, level: int = 1
    ) -> list[dict]:
        rows = list(self._rows.values())
        if venue:
            rows = [r for r in rows if r["venue"] == venue]
        if enabled_only:
            rows = [r for r in rows if not r["disabled"]]
        return [self._filter(r, level) for r in rows]

    async def watermark_ms(self) -> int:
        return 1_700_000_000_000

    async def query_market_status(self, venue: str, market: str) -> dict | None:
        return self._market_status.get((venue, market))

    async def query_market_calendar(
        self, venue: str, market: str, start_date: str, end_date: str
    ) -> list[dict]:
        return [
            e for e in self._market_calendar
            if e["venue"] == venue and e["market"] == market
            and start_date <= e["date"] <= end_date
        ]


_FULL_ROW = {
    "instrument_id": "BTC/USDT@MOCK",
    "venue": "MOCK",
    "instrument_exch": "BTC-USDT",
    "instrument_type": "SPOT",
    "disabled": False,
    "price_tick_size": 0.01,
    "qty_lot_size": 0.00001,
    "base_asset": "BTC",
    "quote_asset": "USDT",
    "updated_at_ms": 1_700_000_000_000,
    # EXTENDED
    "lifecycle_status": "active",
    "settlement_asset": "USDT",
    "contract_size": 1.0,
    "min_notional": 10.0,
    "max_notional": 1_000_000.0,
    "min_order_qty": 0.00001,
    "max_order_qty": 9999.0,
    "max_mkt_order_qty": 100.0,
    "price_precision": 2,
    "qty_precision": 5,
    "extra_properties": {"margin_type": "cross"},
    "first_seen_at_ms": 1_690_000_000_000,
    "last_seen_at_ms": 1_700_000_000_000,
    # FULL
    "source_name": "MOCK",
    "source_run_id": 42,
}


@pytest.fixture
def stub_repo():
    return StubRepo([_FULL_ROW])


@pytest.fixture
async def refdata_channel(stub_repo):
    servicer = RefdataServicer(stub_repo)
    server, target = await _start_server(servicer)
    channel = grpc.aio.insecure_channel(target)
    yield channel
    await channel.close()
    await server.stop(grace=0)


# -- QueryInstrumentById ------------------------------------------------------

async def test_query_by_id_found(refdata_channel):
    from zk_refdata_svc import refdata_pb2_grpc

    stub = refdata_pb2_grpc.RefdataServiceStub(refdata_channel)
    resp = await stub.QueryInstrumentById(
        refdata_pb2.QueryInstrumentByIdRequest(instrument_id="BTC/USDT@MOCK")
    )
    assert resp.instrument_id == "BTC/USDT@MOCK"
    assert resp.base_asset == "BTC"


async def test_query_by_id_not_found(refdata_channel):
    from zk_refdata_svc import refdata_pb2_grpc

    stub = refdata_pb2_grpc.RefdataServiceStub(refdata_channel)
    with pytest.raises(grpc.aio.AioRpcError) as exc_info:
        await stub.QueryInstrumentById(
            refdata_pb2.QueryInstrumentByIdRequest(instrument_id="NOPE")
        )
    assert exc_info.value.code() == grpc.StatusCode.NOT_FOUND


# -- Disclosure levels --------------------------------------------------------

async def test_disclosure_basic_omits_extended_fields(refdata_channel):
    from zk_refdata_svc import refdata_pb2_grpc

    stub = refdata_pb2_grpc.RefdataServiceStub(refdata_channel)
    resp = await stub.QueryInstrumentById(
        refdata_pb2.QueryInstrumentByIdRequest(
            instrument_id="BTC/USDT@MOCK", level=refdata_pb2.BASIC
        )
    )
    assert resp.instrument_id == "BTC/USDT@MOCK"
    # BASIC does not populate lifecycle_status
    assert resp.lifecycle_status == ""
    assert resp.source_name == ""


async def test_disclosure_extended_populates_lifecycle(refdata_channel):
    from zk_refdata_svc import refdata_pb2_grpc

    stub = refdata_pb2_grpc.RefdataServiceStub(refdata_channel)
    resp = await stub.QueryInstrumentById(
        refdata_pb2.QueryInstrumentByIdRequest(
            instrument_id="BTC/USDT@MOCK", level=refdata_pb2.EXTENDED
        )
    )
    assert resp.lifecycle_status == "active"
    assert resp.settlement_asset == "USDT"
    assert resp.price_precision == 2
    assert resp.qty_precision == 5
    assert resp.extra_properties["margin_type"] == "cross"
    # EXTENDED does not populate source_name
    assert resp.source_name == ""


async def test_disclosure_full_populates_source(refdata_channel):
    from zk_refdata_svc import refdata_pb2_grpc

    stub = refdata_pb2_grpc.RefdataServiceStub(refdata_channel)
    resp = await stub.QueryInstrumentById(
        refdata_pb2.QueryInstrumentByIdRequest(
            instrument_id="BTC/USDT@MOCK", level=refdata_pb2.FULL
        )
    )
    assert resp.source_name == "MOCK"
    assert resp.source_run_id == 42
    assert resp.lifecycle_status == "active"


# -- QueryInstrumentByVenueSymbol ---------------------------------------------

async def test_query_by_venue_symbol_found(refdata_channel):
    from zk_refdata_svc import refdata_pb2_grpc

    stub = refdata_pb2_grpc.RefdataServiceStub(refdata_channel)
    resp = await stub.QueryInstrumentByVenueSymbol(
        refdata_pb2.QueryByVenueSymbolRequest(venue="MOCK", instrument_exch="BTC-USDT")
    )
    assert resp.instrument_id == "BTC/USDT@MOCK"


async def test_query_by_venue_symbol_not_found(refdata_channel):
    from zk_refdata_svc import refdata_pb2_grpc

    stub = refdata_pb2_grpc.RefdataServiceStub(refdata_channel)
    with pytest.raises(grpc.aio.AioRpcError) as exc_info:
        await stub.QueryInstrumentByVenueSymbol(
            refdata_pb2.QueryByVenueSymbolRequest(venue="MOCK", instrument_exch="UNKNOWN")
        )
    assert exc_info.value.code() == grpc.StatusCode.NOT_FOUND


# -- ListInstruments ----------------------------------------------------------

async def test_list_instruments_by_venue(refdata_channel):
    from zk_refdata_svc import refdata_pb2_grpc

    stub = refdata_pb2_grpc.RefdataServiceStub(refdata_channel)
    resp = await stub.ListInstruments(
        refdata_pb2.ListInstrumentsRequest(venue="MOCK", enabled_only=True)
    )
    assert len(resp.instruments) == 1
    assert resp.instruments[0].instrument_id == "BTC/USDT@MOCK"


async def test_list_instruments_enabled_only_excludes_disabled(stub_repo):
    stub_repo._rows["DISABLED/USDT@MOCK"] = {
        **_FULL_ROW,
        "instrument_id": "DISABLED/USDT@MOCK",
        "instrument_exch": "DIS-USDT",
        "disabled": True,
        "lifecycle_status": "disabled",
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
        assert "BTC/USDT@MOCK" in ids
        assert "DISABLED/USDT@MOCK" not in ids
    finally:
        await channel.close()
        await server.stop(grace=0)


# -- Watermark ----------------------------------------------------------------

async def test_query_watermark(refdata_channel):
    from zk_refdata_svc import refdata_pb2_grpc

    stub = refdata_pb2_grpc.RefdataServiceStub(refdata_channel)
    resp = await stub.QueryRefdataWatermark(refdata_pb2.QueryWatermarkRequest())
    assert resp.watermark_ms == 1_700_000_000_000


# -- MarketStatus -------------------------------------------------------------

async def test_market_status_fallback_closed(refdata_channel):
    from zk_refdata_svc import refdata_pb2_grpc

    stub = refdata_pb2_grpc.RefdataServiceStub(refdata_channel)
    resp = await stub.QueryMarketStatus(
        refdata_pb2.QueryMarketStatusRequest(venue="MOCK", market="default")
    )
    assert resp.session_state == "closed"


async def test_market_status_reads_real_state(stub_repo):
    stub_repo._market_status[("MOCK", "default")] = {
        "venue": "MOCK",
        "market": "default",
        "session_state": "open",
        "effective_at_ms": 1_700_000_000_000,
    }
    servicer = RefdataServicer(stub_repo)
    server, target = await _start_server(servicer)
    channel = grpc.aio.insecure_channel(target)
    try:
        from zk_refdata_svc import refdata_pb2_grpc

        stub = refdata_pb2_grpc.RefdataServiceStub(channel)
        resp = await stub.QueryMarketStatus(
            refdata_pb2.QueryMarketStatusRequest(venue="MOCK", market="default")
        )
        assert resp.session_state == "open"
    finally:
        await channel.close()
        await server.stop(grace=0)


# -- MarketCalendar -----------------------------------------------------------

async def test_market_calendar_returns_entries(stub_repo):
    stub_repo._market_calendar = [
        {
            "venue": "MOCK",
            "market": "default",
            "date": "2025-01-15",
            "session_state": "open",
            "open_time_ms": 1_705_276_800_000,
            "close_time_ms": 1_705_363_200_000,
            "source": "test",
        },
    ]
    servicer = RefdataServicer(stub_repo)
    server, target = await _start_server(servicer)
    channel = grpc.aio.insecure_channel(target)
    try:
        from zk_refdata_svc import refdata_pb2_grpc

        stub = refdata_pb2_grpc.RefdataServiceStub(channel)
        resp = await stub.QueryMarketCalendar(
            refdata_pb2.QueryMarketCalendarRequest(
                venue="MOCK", market="default",
                start_date="2025-01-01", end_date="2025-12-31",
            )
        )
        assert len(resp.entries) == 1
        assert resp.entries[0].session_state == "open"
        assert resp.entries[0].date == "2025-01-15"
    finally:
        await channel.close()
        await server.stop(grace=0)
