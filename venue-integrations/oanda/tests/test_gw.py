"""Tests for OandaGatewayAdaptor."""

import pytest
from unittest.mock import AsyncMock

from oanda.gw import OandaGatewayAdaptor

import oanda.proto  # noqa: F401
from zk.exch_gw.v1 import exch_gw_pb2 as gw_pb


def _parse_report(event: dict) -> gw_pb.OrderReport:
    """Deserialize an order_report event's payload_bytes."""
    assert event["event_type"] == "order_report"
    report = gw_pb.OrderReport()
    report.ParseFromString(event["payload_bytes"])
    return report


@pytest.fixture
def adaptor_config():
    return {
        "environment": "practice",
        "account_id": "101-003-12345678-001",
        "token": "test-token",
        "oms_account_id": 9002,
    }


class TestOandaGatewayAdaptor:
    def test_init(self, adaptor_config):
        adaptor = OandaGatewayAdaptor(adaptor_config)
        assert adaptor._oanda_account_id == "101-003-12345678-001"
        assert adaptor._api_base_url == "https://api-fxpractice.oanda.com"
        assert not adaptor.connected

    @pytest.mark.asyncio
    async def test_place_order_market_buy(self, adaptor_config):
        adaptor = OandaGatewayAdaptor(adaptor_config)
        mock_client = AsyncMock()
        mock_client.place_order.return_value = {
            "orderFillTransaction": {
                "id": "100",
                "orderID": "99",
                "instrument": "EUR_USD",
                "units": "1000",
                "price": "1.1050",
                "clientExtensions": {"id": "42"},
            },
        }
        adaptor._client = mock_client
        adaptor._connected = True

        ack = await adaptor.place_order({
            "correlation_id": 42,
            "instrument": "EUR_USD",
            "qty": 1000,
            "buysell_type": 1,
            "order_type": 2,
            "price": 0,
        })

        assert ack["success"] is True
        assert ack["exch_order_ref"] == "99"

        # Check the order body sent to OANDA
        call_args = mock_client.place_order.call_args[0][0]
        assert call_args["type"] == "MARKET"
        assert call_args["units"] == "1000"
        assert call_args["clientExtensions"]["id"] == "42"

        # Verify the fill event was queued as protobuf bytes
        event = adaptor._event_queue.get_nowait()
        report = _parse_report(event)
        assert report.exch_order_ref == "99"
        assert report.order_id == 42

    @pytest.mark.asyncio
    async def test_place_order_limit_sell(self, adaptor_config):
        adaptor = OandaGatewayAdaptor(adaptor_config)
        mock_client = AsyncMock()
        mock_client.place_order.return_value = {
            "orderCreateTransaction": {
                "id": "101",
                "instrument": "EUR_USD",
                "units": "-500",
                "price": "1.2000",
                "clientExtensions": {"id": "43"},
            },
        }
        mock_client.get_order.return_value = {
            "order": {"id": "101", "state": "PENDING", "units": "-500"},
        }
        adaptor._client = mock_client
        adaptor._connected = True

        ack = await adaptor.place_order({
            "correlation_id": 43,
            "instrument": "EUR_USD",
            "qty": 500,
            "buysell_type": 2,
            "order_type": 1,
            "price": 1.2000,
        })

        assert ack["success"] is True
        assert ack["exch_order_ref"] == "101"

        call_args = mock_client.place_order.call_args[0][0]
        assert call_args["type"] == "LIMIT"
        assert call_args["units"] == "-500"
        assert call_args["price"] == "1.2"

        # Verify the create event was queued as protobuf
        event = adaptor._event_queue.get_nowait()
        report = _parse_report(event)
        assert report.exch_order_ref == "101"
        linkage = report.order_report_entries[0]
        assert linkage.order_id_linkage_report.order_id == 43

    @pytest.mark.asyncio
    async def test_cancel_order_success(self, adaptor_config):
        adaptor = OandaGatewayAdaptor(adaptor_config)
        mock_client = AsyncMock()
        mock_client.cancel_order.return_value = {
            "orderCancelTransaction": {"id": "200", "orderID": "101"},
        }
        adaptor._client = mock_client

        ack = await adaptor.cancel_order({"exch_order_ref": "101"})
        assert ack["success"] is True

        event = adaptor._event_queue.get_nowait()
        report = _parse_report(event)
        assert report.exch_order_ref == "101"
        entry = report.order_report_entries[0]
        assert entry.order_state_report.exch_order_status == gw_pb.EXCH_ORDER_STATUS_CANCELLED

    @pytest.mark.asyncio
    async def test_query_balance(self, adaptor_config):
        adaptor = OandaGatewayAdaptor(adaptor_config)
        mock_client = AsyncMock()
        mock_client.get_account_summary.return_value = {
            "account": {
                "currency": "USD",
                "balance": "100000.0",
                "marginAvailable": "95000.0",
                "marginUsed": "5000.0",
            },
        }
        adaptor._client = mock_client

        facts = await adaptor.query_balance({})
        assert len(facts) == 1
        assert facts[0]["asset"] == "USD"
        assert facts[0]["total_qty"] == 100000.0
        assert facts[0]["avail_qty"] == 95000.0

    @pytest.mark.asyncio
    async def test_query_positions(self, adaptor_config):
        adaptor = OandaGatewayAdaptor(adaptor_config)
        mock_client = AsyncMock()
        mock_client.get_positions.return_value = {
            "positions": [
                {
                    "instrument": "EUR_USD",
                    "long": {"units": "1000"},
                    "short": {"units": "-500"},
                },
            ],
        }
        adaptor._client = mock_client

        facts = await adaptor.query_positions({})
        assert len(facts) == 2
        long_fact = [f for f in facts if f["long_short_type"] == 1][0]
        short_fact = [f for f in facts if f["long_short_type"] == 2][0]
        assert long_fact["qty"] == 1000.0
        assert short_fact["qty"] == 500.0
        assert long_fact["instrument_type"] == 4  # CFD
        assert short_fact["instrument_type"] == 4

    @pytest.mark.asyncio
    async def test_query_funding_fees_returns_empty(self, adaptor_config):
        adaptor = OandaGatewayAdaptor(adaptor_config)
        result = await adaptor.query_funding_fees({})
        assert result == []

    @pytest.mark.asyncio
    async def test_next_event(self, adaptor_config):
        adaptor = OandaGatewayAdaptor(adaptor_config)
        await adaptor._event_queue.put({"event_type": "system", "payload": {"event_type": "connected", "message": "test", "timestamp": 0}})
        event = await adaptor.next_event()
        assert event["event_type"] == "system"
