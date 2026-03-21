"""Binance spot venue loader."""

from __future__ import annotations

from zk_refdata_svc.loaders.base import VenueLoader, instrument_id_ccxt, int_precision


class Binance(VenueLoader):
    async def load_instruments(self) -> list[dict]:
        url = "https://api.binance.com/api/v1/exchangeInfo"
        response = await self._request(url)
        if not response:
            return []

        records: list[dict] = []
        for sym in response["symbols"]:
            filters = {f["filterType"]: f for f in sym["filters"]}

            def get_filter_field(
                filter_type: str, field: str, tp: type = float
            ) -> float | None:
                if filter_type in filters and field in filters[filter_type]:
                    return tp(filters[filter_type][field])
                return None

            base_asset = sym["baseAsset"]
            quote_asset = sym["quoteAsset"]
            settlement_asset = None
            type_suffix = ""
            venue = "BINANCE"
            tick_size = get_filter_field("PRICE_FILTER", "tickSize", float)
            lot_size = get_filter_field("LOT_SIZE", "stepSize", float)

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
                    "min_notional": get_filter_field("NOTIONAL", "minNotional", float),
                    "max_notional": get_filter_field("NOTIONAL", "maxNotional", float),
                    "min_order_qty": get_filter_field("LOT_SIZE", "minQty", float),
                    "max_order_qty": get_filter_field("LOT_SIZE", "maxQty", float),
                    "max_mkt_order_qty": get_filter_field(
                        "MARKET_LOT_SIZE", "maxQty", float
                    ),
                    "extra_properties": {},
                    "disabled": False,
                }
            )

        return records
