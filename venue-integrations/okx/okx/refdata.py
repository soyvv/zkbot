"""OKX spot + perpetual swap refdata loader."""

from __future__ import annotations

from zk_refdata_svc.loaders.base import VenueLoader, int_precision


class OkxRefdataLoader(VenueLoader):
    """Refdata loader for OKX exchange (spot + perpetual swaps)."""

    async def load_instruments(self) -> list[dict]:
        api_base = self._config.get("api_base_url", "https://www.okx.com")
        url_spot = f"{api_base}/api/v5/public/instruments?instType=SPOT"
        url_swap = f"{api_base}/api/v5/public/instruments?instType=SWAP"
        response_spot = await self._request(url_spot)
        response_swap = await self._request(url_swap)

        records: list[dict] = []

        if response_spot:
            for data in response_spot["data"]:
                base_asset = data["baseCcy"]
                quote_asset = data["quoteCcy"]
                settlement_asset = None
                type_suffix = ""
                venue = "OKX"
                tick_size = float(data["tickSz"])
                lot_size = float(data["lotSz"])

                records.append(
                    {
                        "instrument_id": self.instrument_id(
                            base_asset, type_suffix, quote_asset, venue
                        ),
                        "instrument_exch": data["instId"],
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
                        "min_notional": None,
                        "max_notional": None,
                        "min_order_qty": float(data["minSz"]),
                        "max_order_qty": float(data["maxMktSz"]),
                        "max_mkt_order_qty": None,
                        "extra_properties": {},
                        "disabled": False,
                    }
                )

        if response_swap:
            for data in response_swap["data"]:
                base_asset, quote_asset = str(data["instFamily"]).split("-", maxsplit=1)
                settlement_asset = data["settleCcy"]
                type_suffix = "-P"
                venue = "OKXDM"
                tick_size = float(data["tickSz"])
                lot_size = float(data["lotSz"])
                contract_size = float(data["ctVal"])

                records.append(
                    {
                        "instrument_id": self.instrument_id(
                            base_asset, type_suffix, quote_asset, venue
                        ),
                        "instrument_exch": data["instId"],
                        "venue": venue,
                        "instrument_type": "PERP",
                        "base_asset": base_asset,
                        "quote_asset": quote_asset,
                        "settlement_asset": settlement_asset,
                        "contract_size": contract_size,
                        "price_precision": int_precision(tick_size),
                        "qty_precision": int_precision(lot_size),
                        "price_tick_size": tick_size,
                        "qty_lot_size": lot_size,
                        "min_notional": None,
                        "max_notional": None,
                        "min_order_qty": float(data["minSz"]),
                        "max_order_qty": float(data["maxMktSz"]),
                        "max_mkt_order_qty": None,
                        "extra_properties": {},
                        "disabled": False,
                    }
                )

        return records
