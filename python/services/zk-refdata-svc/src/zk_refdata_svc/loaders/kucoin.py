"""KuCoin spot venue loader."""

from __future__ import annotations

from zk_refdata_svc.loaders.base import VenueLoader, instrument_id_ccxt, int_precision


class Kucoin(VenueLoader):
    async def load_instruments(self) -> list[dict]:
        url1 = "https://openapi-v2.kucoin.com/api/v1/symbols"
        url2 = "https://api.kucoin.com/api/v1/symbols"
        response = await self._request(url1)
        if not response or "data" not in response:
            response = await self._request(url2)
        if not response:
            return []

        records: list[dict] = []
        for data in response["data"]:
            base_asset = data["baseCurrency"]
            quote_asset = data["quoteCurrency"]
            settlement_asset = None
            type_suffix = ""
            venue = "KUCOIN"
            tick_size = float(data["priceIncrement"])
            lot_size = float(data["baseIncrement"])

            records.append(
                {
                    "instrument_id": self.instrument_id(
                        base_asset, type_suffix, quote_asset, venue
                    ),
                    "instrument_exch": instrument_id_ccxt(
                        base_asset, quote_asset, settlement_asset
                    ),
                    "venue": venue,
                    "instrument_type": "SPOT",
                    "base_asset": base_asset,
                    "quote_asset": quote_asset,
                    "settlement_asset": settlement_asset,
                    "contract_size": 1.0,
                    "price_precision": int_precision(tick_size),
                    "qty_precision": int_precision(lot_size),
                    "price_tick_size": tick_size,
                    "qty_lot_size": lot_size,
                    "min_notional": float(data["quoteMinSize"]),
                    "max_notional": float(data["quoteMaxSize"]),
                    "min_order_qty": float(data["baseMinSize"]),
                    "max_order_qty": float(data["baseMaxSize"]),
                    "max_mkt_order_qty": None,
                    "extra_properties": {},
                    "disabled": False,
                }
            )

        return records
