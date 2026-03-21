"""Dexalot spot venue loader."""

from __future__ import annotations

import os

from zk_refdata_svc.loaders.base import (
    VenueLoader,
    float_precision,
    instrument_id_ccxt,
)


class Dexalot(VenueLoader):
    async def load_instruments(self) -> list[dict]:
        url = "https://api.dexalot.com/privapi/trading/pairs"
        api_key = self._config.get("api_key") or os.environ.get("ZK_DEXALOT_API_KEY", "")
        response = await self._request(
            url, headers={"x-apikey": api_key} if api_key else {}
        )
        if not response:
            return []

        records: list[dict] = []
        for sym in response:
            base_asset = sym["base"].upper()
            quote_asset = sym["quote"].upper()
            settlement_asset = None
            type_suffix = ""
            venue = "DEXALOT"
            price_precision = sym["quotedisplaydecimals"]
            qty_precision = sym["basedisplaydecimals"]
            tick_size = float_precision(price_precision)
            lot_size = float_precision(qty_precision)

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
                    "price_precision": price_precision,
                    "qty_precision": qty_precision,
                    "price_tick_size": tick_size,
                    "qty_lot_size": lot_size,
                    "min_notional": float(sym["mintrade_amnt"]),
                    "max_notional": float(sym["maxtrade_amnt"]),
                    "min_order_qty": lot_size,
                    "max_order_qty": None,
                    "max_mkt_order_qty": None,
                    "extra_properties": {},
                    "disabled": False,
                }
            )

        return records
