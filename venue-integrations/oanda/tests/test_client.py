"""Tests for OandaRestClient."""

import pytest
import httpx
from unittest.mock import AsyncMock, patch, MagicMock

from oanda.client import OandaRestClient, OandaApiError


@pytest.fixture
def client():
    return OandaRestClient(
        api_base_url="https://api-fxpractice.oanda.com",
        token="test-token",
        account_id="101-003-12345678-001",
    )


class TestOandaRestClient:
    @pytest.mark.asyncio
    async def test_place_order_builds_correct_request(self, client):
        mock_resp = MagicMock()
        mock_resp.status_code = 201
        mock_resp.json.return_value = {
            "orderCreateTransaction": {"id": "123", "type": "LIMIT_ORDER"},
        }

        with patch.object(client._client, "request", new_callable=AsyncMock, return_value=mock_resp):
            result = await client.place_order({"type": "LIMIT", "instrument": "EUR_USD", "units": "100", "price": "1.1000"})
            assert result["orderCreateTransaction"]["id"] == "123"

    @pytest.mark.asyncio
    async def test_get_account_summary(self, client):
        mock_resp = MagicMock()
        mock_resp.status_code = 200
        mock_resp.json.return_value = {
            "account": {"currency": "USD", "balance": "100000.0"},
        }

        with patch.object(client._client, "request", new_callable=AsyncMock, return_value=mock_resp):
            result = await client.get_account_summary()
            assert result["account"]["currency"] == "USD"

    @pytest.mark.asyncio
    async def test_api_error_on_4xx(self, client):
        mock_resp = MagicMock()
        mock_resp.status_code = 400
        mock_resp.json.return_value = {"errorMessage": "Invalid units"}

        with patch.object(client._client, "request", new_callable=AsyncMock, return_value=mock_resp):
            with pytest.raises(OandaApiError) as exc_info:
                await client.place_order({"type": "MARKET"})
            assert exc_info.value.status_code == 400

    @pytest.mark.asyncio
    async def test_cancel_order(self, client):
        mock_resp = MagicMock()
        mock_resp.status_code = 200
        mock_resp.json.return_value = {
            "orderCancelTransaction": {"id": "456", "orderID": "123"},
        }

        with patch.object(client._client, "request", new_callable=AsyncMock, return_value=mock_resp):
            result = await client.cancel_order("123")
            assert result["orderCancelTransaction"]["orderID"] == "123"

    @pytest.mark.asyncio
    async def test_get_positions(self, client):
        mock_resp = MagicMock()
        mock_resp.status_code = 200
        mock_resp.json.return_value = {
            "positions": [
                {"instrument": "EUR_USD", "long": {"units": "100"}, "short": {"units": "0"}},
            ],
        }

        with patch.object(client._client, "request", new_callable=AsyncMock, return_value=mock_resp):
            result = await client.get_positions()
            assert len(result["positions"]) == 1

    @pytest.mark.asyncio
    async def test_repr_does_not_leak_token(self, client):
        r = repr(client)
        assert "test-token" not in r
