"""Async REST client for the OANDA v20 API."""

from __future__ import annotations

import httpx
from loguru import logger


class OandaApiError(Exception):
    """Raised when OANDA returns a non-2xx HTTP response."""

    def __init__(self, status_code: int, body: dict | str):
        self.status_code = status_code
        self.body = body
        super().__init__(f"OANDA API error {status_code}: {body}")


class OandaRestClient:
    """Thin async httpx wrapper around the OANDA v20 REST API.

    All methods return parsed JSON dicts. Normalization is handled separately
    in ``oanda_normalize.py``.
    """

    def __init__(self, api_base_url: str, token: str, account_id: str) -> None:
        self._account_id = account_id
        self._client = httpx.AsyncClient(
            base_url=api_base_url,
            headers={
                "Authorization": f"Bearer {token}",
                "Accept-Datetime-Format": "RFC3339",
                "Content-Type": "application/json",
            },
            timeout=httpx.Timeout(connect=10.0, read=30.0, write=10.0, pool=10.0),
        )

    def __repr__(self) -> str:
        return f"OandaRestClient(account={self._account_id}, base_url={self._client.base_url})"

    # ── helpers ──────────────────────────────────────────────────────────────

    def _acct(self) -> str:
        return f"/v3/accounts/{self._account_id}"

    async def _request(self, method: str, path: str, **kwargs) -> dict:
        resp = await self._client.request(method, path, **kwargs)
        if resp.status_code >= 400:
            try:
                body = resp.json()
            except Exception:
                body = resp.text
            raise OandaApiError(resp.status_code, body)
        return resp.json()

    # ── order commands ───────────────────────────────────────────────────────

    async def place_order(self, order_body: dict) -> dict:
        """POST /v3/accounts/{accountID}/orders"""
        return await self._request("POST", f"{self._acct()}/orders", json={"order": order_body})

    async def cancel_order(self, order_specifier: str) -> dict:
        """PUT /v3/accounts/{accountID}/orders/{orderSpecifier}/cancel"""
        return await self._request("PUT", f"{self._acct()}/orders/{order_specifier}/cancel")

    # ── order queries ────────────────────────────────────────────────────────

    async def get_order(self, order_specifier: str) -> dict:
        """GET /v3/accounts/{accountID}/orders/{orderSpecifier}"""
        return await self._request("GET", f"{self._acct()}/orders/{order_specifier}")

    async def get_pending_orders(self) -> dict:
        """GET /v3/accounts/{accountID}/pendingOrders"""
        return await self._request("GET", f"{self._acct()}/pendingOrders")

    # ── trade queries ────────────────────────────────────────────────────────

    async def get_trades(
        self,
        *,
        instrument: str | None = None,
        count: int | None = None,
        before_id: str | None = None,
    ) -> dict:
        """GET /v3/accounts/{accountID}/trades"""
        params: dict[str, str] = {}
        if instrument:
            params["instrument"] = instrument
        if count and count > 0:
            params["count"] = str(min(count, 500))  # OANDA max is 500
        if before_id:
            params["beforeID"] = before_id
        return await self._request("GET", f"{self._acct()}/trades", params=params)

    async def get_open_trades(self) -> dict:
        """GET /v3/accounts/{accountID}/openTrades"""
        return await self._request("GET", f"{self._acct()}/openTrades")

    # ── position queries ─────────────────────────────────────────────────────

    async def get_positions(self) -> dict:
        """GET /v3/accounts/{accountID}/positions"""
        return await self._request("GET", f"{self._acct()}/positions")

    # ── account queries ──────────────────────────────────────────────────────

    async def get_account_summary(self) -> dict:
        """GET /v3/accounts/{accountID}/summary"""
        return await self._request("GET", f"{self._acct()}/summary")

    async def get_account_changes(self, since_transaction_id: str) -> dict:
        """GET /v3/accounts/{accountID}/changes?sinceTransactionID=..."""
        return await self._request(
            "GET",
            f"{self._acct()}/changes",
            params={"sinceTransactionID": since_transaction_id},
        )


    # ── pricing / RTMD queries ──────────────────────────────────────────

    async def get_pricing(self, instruments: list[str]) -> dict:
        """GET /v3/accounts/{accountID}/pricing?instruments=EUR_USD,GBP_USD"""
        return await self._request(
            "GET",
            f"{self._acct()}/pricing",
            params={"instruments": ",".join(instruments)},
        )

    async def get_candles(
        self,
        instrument: str,
        *,
        granularity: str = "M1",
        count: int | None = None,
        from_time: str | None = None,
        to_time: str | None = None,
    ) -> dict:
        """GET /v3/instruments/{instrument}/candles"""
        params: dict[str, str] = {"granularity": granularity, "price": "M"}
        if count and count > 0:
            params["count"] = str(min(count, 5000))
        if from_time:
            params["from"] = from_time
        if to_time:
            params["to"] = to_time
        return await self._request("GET", f"/v3/instruments/{instrument}/candles", params=params)

    # ── instrument / refdata queries ──────────────────────────────────

    async def get_instruments(self, *, instruments: list[str] | None = None) -> dict:
        """GET /v3/accounts/{accountID}/instruments"""
        params: dict[str, str] = {}
        if instruments:
            params["instruments"] = ",".join(instruments)
        return await self._request("GET", f"{self._acct()}/instruments", params=params)

    # ── lifecycle ────────────────────────────────────────────────────────────

    async def close(self) -> None:
        await self._client.aclose()
        logger.debug("OANDA REST client closed")
