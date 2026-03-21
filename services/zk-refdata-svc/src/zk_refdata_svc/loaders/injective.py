"""Injective spot venue loader (parses INI config from GitHub)."""

from __future__ import annotations

import configparser
from math import log10

from zk_refdata_svc.loaders.base import VenueLoader, instrument_id_ccxt, float_precision


class Injective(VenueLoader):
    def __init__(self, config: dict | None = None) -> None:
        super().__init__(config)
        self.handle_spot = True
        self.handle_perp = False

    async def load_instruments(self) -> list[dict]:
        url = (
            "https://raw.githubusercontent.com/InjectiveLabs/sdk-python"
            "/master/pyinjective/denoms_mainnet.ini"
        )
        response_str = await self._request_text(url)
        if not response_str:
            return []

        cfg = configparser.ConfigParser()
        cfg.read_string(response_str)

        records: list[dict] = []
        for section in cfg.sections():
            data = dict(cfg.items(section))

            if "peggy_denom" in data:
                continue

            descr = data["description"].replace("'", "")
            is_perp = descr.find("PERP") != -1
            is_spot = descr.find("Spot") != -1

            if not is_perp and not is_spot:
                continue
            if not self.handle_perp and is_perp:
                continue
            if not self.handle_spot and is_spot:
                continue

            exch_symbol = descr.split()[2]
            base_asset, quote_asset = exch_symbol.split("/")
            type_suffix = ""
            venue = "INJECTIVE"
            settlement_asset = None

            base_decs = int(data["base"])
            quote_decs = int(data["quote"])
            min_price_tick_size = float(data["min_price_tick_size"])
            min_qty_tick_size = float(data["min_quantity_tick_size"])
            min_display_qty_tick_size = float(data["min_display_quantity_tick_size"])

            price_precision = round(
                -(log10(min_price_tick_size) + base_decs - quote_decs)
            )
            size_precision = round(-(log10(min_qty_tick_size) - base_decs))

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
                    "price_precision": int(price_precision),
                    "qty_precision": int(size_precision),
                    "price_tick_size": float_precision(int(price_precision)),
                    "qty_lot_size": float_precision(int(size_precision)),
                    "min_notional": None,
                    "max_notional": None,
                    "min_order_qty": min_display_qty_tick_size,
                    "max_order_qty": None,
                    "max_mkt_order_qty": None,
                    "extra_properties": {},
                    "disabled": False,
                }
            )

        return records
