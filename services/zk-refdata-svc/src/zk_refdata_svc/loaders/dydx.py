"""dYdX perpetual markets venue loader."""

from __future__ import annotations

from zk_refdata_svc.loaders.base import VenueLoader, instrument_id_ccxt, int_precision


class Dydx(VenueLoader):
    async def load_instruments(self) -> list[dict]:
        url = "https://indexer.dydx.trade/v4/perpetualMarkets"
        response = await self._request(url)
        if not response:
            return []

        records: list[dict] = []
        for data in response["markets"].values():
            base_asset, quote_asset = data["ticker"].split("-")
            settlement_asset = None
            type_suffix = ""
            venue = "DYDX"
            tick_size = float(data["tickSize"])
            lot_size = float(data["stepSize"])

            records.append(
                {
                    "instrument_id": self.instrument_id(
                        base_asset, type_suffix, quote_asset, venue
                    ),
                    "instrument_exch": instrument_id_ccxt(
                        base_asset, quote_asset, settlement_asset
                    ),
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
                    "min_order_qty": None,
                    "max_order_qty": None,
                    "max_mkt_order_qty": None,
                    "extra_properties": {},
                    "disabled": False,
                }
            )

        return records
