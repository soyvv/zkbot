"""Fake refdata loader for integration testing."""


class FakeRefdataLoader:
    def __init__(self, config: dict):
        self.config = config

    async def load_instruments(self) -> list[dict]:
        return [
            {
                "instrument_id": "BTC-USDT-PERP",
                "instrument_id_exchange": "BTC-USDT-SWAP",
                "exchange_name": "test",
                "instrument_type": "perp",
                "base_asset": "BTC",
                "quote_asset": "USDT",
                "price_precision": 2,
                "qty_precision": 4,
                "price_tick_size": 0.01,
                "qty_lot_size": 0.0001,
            },
            {
                "instrument_id": "ETH-USDT-SPOT",
                "instrument_id_exchange": "ETH-USDT",
                "exchange_name": "test",
                "instrument_type": "spot",
                "base_asset": "ETH",
                "quote_asset": "USDT",
                "price_precision": 2,
                "qty_precision": 4,
            },
        ]

    async def load_market_sessions(self) -> list[dict]:
        return []
