"""Bluefin SUI perpetual venue loader."""

from __future__ import annotations

from zk_refdata_svc.loaders.base import VenueLoader, int_precision


class BluefinSui(VenueLoader):
    async def load_instruments(self) -> list[dict]:
        url = "https://dapi.api.sui-prod.bluefin.io/exchangeInfo"
        response = await self._request(url)
        if not response:
            return []

        records: list[dict] = []
        base = 10**18
        for sym in response:
            if "error" in sym:
                continue

            tick_size = float(int(sym["tickSize"]) / base)
            lot_size = float(int(sym["stepSize"]) / base)
            base_asset = sym["baseAssetSymbol"]
            quote_asset = sym["quoteAssetSymbol"]
            settlement_asset = quote_asset
            type_suffix = "-P"
            venue = "BLUEFINSUI"

            records.append(
                {
                    "instrument_id": self.instrument_id(
                        base_asset, type_suffix, quote_asset, venue
                    ),
                    "instrument_exch": f"{base_asset}-PERP",
                    "venue": venue,
                    "instrument_type": "PERP",
                    "base_asset": base_asset,
                    "quote_asset": quote_asset,
                    "settlement_asset": settlement_asset,
                    "contract_size": 1.0,
                    "price_precision": int_precision(tick_size),
                    "qty_precision": int_precision(lot_size),
                    "price_tick_size": tick_size,
                    "qty_lot_size": lot_size,
                    "min_notional": None,
                    "max_notional": None,
                    "min_order_qty": float(sym["minOrderSize"]) / base,
                    "max_order_qty": float(sym["maxMarketOrderSize"]) / base,
                    "max_mkt_order_qty": None,
                    "extra_properties": {},
                    "disabled": False,
                }
            )

        return records
