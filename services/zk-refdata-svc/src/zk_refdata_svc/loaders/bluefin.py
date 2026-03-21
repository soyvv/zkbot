"""Bluefin (Arbitrum) perpetual venue loader.

NOTE: The original adaptor used the firefly-exchange-client SDK to enumerate
market symbols and fetch exchange info per symbol. That SDK dependency is
replaced here with a TODO — the Bluefin Arbitrum API does not expose a public
exchangeInfo-style endpoint without the SDK.
"""

from __future__ import annotations

from zk_refdata_svc.loaders.base import VenueLoader, instrument_id_ccxt, int_precision


class Bluefin(VenueLoader):
    async def load_instruments(self) -> list[dict]:
        # TODO: The original used firefly_exchange_client.FireflyClient to call
        # get_exchange_info(symbol) for each MARKET_SYMBOLS entry. To port this
        # without the SDK, either:
        #   1. Find a public REST endpoint for Bluefin Arbitrum exchange info, or
        #   2. Keep the firefly SDK as an optional dependency.
        # For now, this returns an empty list.
        #
        # Original logic reference:
        #   base = 10**18
        #   tick_size = int(exch["tickSize"]) / base
        #   lot_size = int(exch["stepSize"]) / base
        #   base_asset = exch["baseAssetSymbol"]
        #   quote_asset = exch["quoteAssetSymbol"]
        #   settlement_asset = quote_asset
        #   min_price = int(exch["minOrderPrice"]) / base
        #   max_price = int(exch["maxOrderPrice"]) / base
        #   min_order_qty = lot_size
        #   max_order_qty = int(exch["maxLimitOrderSize"]) / base
        return []
