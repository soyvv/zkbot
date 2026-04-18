"""Paradex venue loader."""

from __future__ import annotations

from zk_refdata_svc.loaders.base import VenueLoader, int_precision


class Paradex(VenueLoader):
    async def load_instruments(self) -> list[dict]:
        url = "https://api.testnet.paradex.trade/v1/markets"
        response = await self._request(url)
        if not response:
            return []

        records: list[dict] = []
        for sym in response["results"]:
            symbol = sym["symbol"]
            base_asset = sym["base_currency"]
            quote_asset = sym["quote_currency"]
            settlement_asset = sym["settlement_currency"]
            is_perp = sym["asset_kind"] == "PERP"
            type_suffix = "-P" if is_perp else ""
            venue = "PARADEX"
            tick_size = float(sym["price_tick_size"])
            lot_size = float(sym["order_size_increment"])

            records.append(
                {
                    "instrument_id": self.instrument_id(
                        base_asset, type_suffix, quote_asset, venue
                    ),
                    "instrument_exch": symbol,
                    "venue": venue,
                    "instrument_type": "PERP" if is_perp else "SPOT",
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
                    "max_order_qty": sym["max_open_orders"],
                    "max_mkt_order_qty": None,
                    "extra_properties": {},
                    "disabled": False,
                }
            )

        return records
