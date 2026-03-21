"""Blitz (Blast) spot + perpetual venue loader."""

from __future__ import annotations

from zk_refdata_svc.loaders.base import VenueLoader, int_precision


class Blitz(VenueLoader):
    @staticmethod
    def _calc_max_lev(long_weight_initial_x18: str) -> int:
        return round(1 / (1 - (float(long_weight_initial_x18) / 1e18)))

    async def load_instruments(self) -> list[dict]:
        url_sym = (
            "https://gateway.blast-prod.vertexprotocol.com/query?type=symbols"
        )
        url_pair = (
            "https://gateway.blast-prod.vertexprotocol.com/v2/pairs"
            "?=market={spot|perp}"
        )

        response_sym = await self._request(url_sym)
        response_pair = await self._request(url_pair)
        if not response_sym or not response_pair:
            return []

        symbols = response_sym["data"]["symbols"]
        base_quote_map = {each["base"]: each["quote"] for each in response_pair}

        norm = 1e18
        records: list[dict] = []
        for sym in symbols.values():
            is_perp = sym["type"] == "perp"
            exch_symbol = sym["symbol"]
            base_asset = sym["symbol"][:-5] if is_perp else sym["symbol"]
            quote_asset = base_quote_map[sym["symbol"]]
            settlement_asset = quote_asset if is_perp else None
            type_suffix = "-P" if is_perp else ""
            venue = "BLITZ"
            tick_size = int(sym["price_increment_x18"]) / norm
            lot_size = int(sym["size_increment"]) / norm

            extra: dict[str, str] = {}
            if is_perp:
                extra["max_leverage"] = str(
                    self._calc_max_lev(sym["long_weight_initial_x18"])
                )

            records.append(
                {
                    "instrument_id": self.instrument_id(
                        base_asset, type_suffix, quote_asset, venue
                    ),
                    "instrument_exch": exch_symbol,
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
                    "min_order_qty": float(int(sym["min_size"]) / norm),
                    "max_order_qty": None,
                    "max_mkt_order_qty": None,
                    "extra_properties": extra,
                    "disabled": False,
                }
            )

        return records
