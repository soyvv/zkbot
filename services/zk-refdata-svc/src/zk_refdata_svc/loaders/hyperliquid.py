"""Hyperliquid perpetual venue loader."""

from __future__ import annotations

from zk_refdata_svc.loaders.base import VenueLoader


class Hyperliquid(VenueLoader):
    async def load_instruments(self) -> list[dict]:
        url = "https://api.hyperliquid.xyz/info"
        response = await self._request(
            url,
            method="POST",
            headers={"Content-Type": "application/json"},
            json_body={"type": "metaAndAssetCtxs"},
        )
        if not response:
            return []

        universe = response[0]["universe"]
        asset_contexts = response[1]

        if len(universe) != len(asset_contexts):
            raise ValueError(
                "base assets and asset contexts do not have the same length!"
            )

        records: list[dict] = []
        for base_info, _context in zip(universe, asset_contexts):
            base_asset = base_info["name"]
            quote_asset = "USD"
            settlement_asset = "USD"
            type_suffix = "-P"
            venue = "HYPERLIQUID"

            records.append(
                {
                    "instrument_id": self.instrument_id(
                        base_asset, type_suffix, quote_asset, venue
                    ),
                    "instrument_exch": base_asset,
                    "venue": venue,
                    "instrument_type": "PERP",
                    "base_asset": base_asset,
                    "quote_asset": quote_asset,
                    "settlement_asset": settlement_asset,
                    "contract_size": 1.0,
                    "price_precision": 5,
                    "qty_precision": base_info["szDecimals"],
                    "price_tick_size": None,
                    "qty_lot_size": None,
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
