"""Chainflip spot venue loader (static asset list)."""

from __future__ import annotations

from zk_refdata_svc.loaders.base import VenueLoader, int_precision

ASSETS = [
    {
        "chainflipId": "Eth",
        "asset": "ETH",
        "chain": "Ethereum",
        "contractAddress": None,
        "decimals": 18,
        "name": "Ether",
        "symbol": "ETH",
        "isMainnet": False,
        "minimumSwapAmount": "0",
        "maximumSwapAmount": None,
        "minimumEgressAmount": "1",
    },
    {
        "chainflipId": "Flip",
        "asset": "FLIP",
        "chain": "Ethereum",
        "contractAddress": "0xdC27c60956cB065D19F08bb69a707E37b36d8086",
        "decimals": 18,
        "name": "FLIP",
        "symbol": "FLIP",
        "isMainnet": False,
        "minimumSwapAmount": "0",
        "maximumSwapAmount": None,
        "minimumEgressAmount": "1",
    },
    {
        "chainflipId": "Usdt",
        "asset": "USDT",
        "chain": "Ethereum",
        "contractAddress": "0x27CEA6Eb8a21Aae05Eb29C91c5CA10592892F584",
        "decimals": 6,
        "name": "USDT",
        "symbol": "USDT",
        "isMainnet": False,
        "minimumSwapAmount": "0",
        "maximumSwapAmount": None,
        "minimumEgressAmount": "1",
    },
    {
        "chainflipId": "Btc",
        "asset": "BTC",
        "chain": "Bitcoin",
        "contractAddress": None,
        "decimals": 8,
        "name": "Bitcoin",
        "symbol": "BTC",
        "isMainnet": False,
        "minimumSwapAmount": "0",
        "maximumSwapAmount": None,
        "minimumEgressAmount": "600",
    },
    {
        "chainflipId": "Dot",
        "asset": "DOT",
        "chain": "Polkadot",
        "contractAddress": None,
        "decimals": 10,
        "name": "Polkadot",
        "symbol": "DOT",
        "isMainnet": False,
        "minimumSwapAmount": "0",
        "maximumSwapAmount": None,
        "minimumEgressAmount": "1",
    },
]


def _get_order_qty(value: str | None) -> float | None:
    if not value:
        return None
    return float(int(value))


class Chainflip(VenueLoader):
    async def load_instruments(self) -> list[dict]:
        default_quote = "USDC"
        records: list[dict] = []

        for asset in ASSETS:
            base_asset = asset["asset"]
            if base_asset == default_quote:
                continue  # Skip the quote asset itself — no self-pair
            quote_asset = default_quote
            exch_symbol = f"{base_asset}/{quote_asset}"
            settlement_asset = None
            type_suffix = ""
            venue = "CHAINFLIP"

            # Convert all extra_properties values to str
            extra = {k: str(v) for k, v in asset.items()}

            records.append(
                {
                    "instrument_id": self.instrument_id(
                        base_asset, type_suffix, quote_asset, venue
                    ),
                    "instrument_exch": exch_symbol,
                    "venue": venue,
                    "instrument_type": "SPOT",
                    "base_asset": base_asset,
                    "quote_asset": quote_asset,
                    "settlement_asset": settlement_asset,
                    "contract_size": 1.0,
                    "price_precision": int_precision(asset["decimals"]),
                    "qty_precision": int_precision(asset["decimals"]),
                    "price_tick_size": 1.0,
                    "qty_lot_size": 1.0,
                    "min_notional": None,
                    "max_notional": None,
                    "min_order_qty": _get_order_qty(asset["minimumSwapAmount"]),
                    "max_order_qty": _get_order_qty(asset["maximumSwapAmount"]),
                    "max_mkt_order_qty": None,
                    "extra_properties": extra,
                    "disabled": False,
                }
            )

        return records
