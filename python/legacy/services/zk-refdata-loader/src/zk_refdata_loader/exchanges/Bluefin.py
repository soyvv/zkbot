import asyncio
import time
from firefly_exchange_client import FireflyClient
from firefly_exchange_client.constants import Networks
from firefly_exchange_client.enumerations import MARKET_SYMBOLS

from zk_refdata_loader import utils
from zk_refdata_loader.exchange_base import ExchangeBase
from zk_proto_betterproto.common import InstrumentType, InstrumentRefData

class Bluefin(ExchangeBase):
    def __init__(self, **kwargs):
        super().__init__(**kwargs)
        self.loop = asyncio.new_event_loop()
        asyncio.set_event_loop(self.loop)
        self.client = FireflyClient(
            are_terms_accepted=True,
            network=Networks["MAINNET_ARBITRUM"],
            private_key="ac804d463bd7a1dfaaaf8d79648769fe5d70c93203969eaab68142d904d824a7"
        )
        
    
    def fetch(self):
        async def get_exchange_info(symbol:str):
            return await self.client.get_exchange_info(symbol)
        
        exch_info = [self.loop.run_until_complete(get_exchange_info(sym)) for sym in MARKET_SYMBOLS]
        
        records = list()
        base = 10**18
        for exch in exch_info:
            if 'error' in exch:
                continue
            
            tick_size = int(exch["tickSize"]) / base
            lot_size = int(exch["stepSize"]) / base
            base_asset = exch["baseAssetSymbol"]
            quote_asset = exch["quoteAssetSymbol"]
            settlement_asset = quote_asset
            type_suffix = "-P"
            exch_name = "BLUEFIN"
            instrument_ref_data = InstrumentRefData(
                instrument_id=self.instrument_id(base_asset, type_suffix, quote_asset, exch_name),
                instrument_id_exchange=self.instrument_id_ccxt(base_asset, quote_asset, settlement_asset),
                update_ts=int(time.time()),
                exch_name=exch_name,
                instrument_type=InstrumentType.INST_TYPE_PERP, # P perpetual
                base_asset=base_asset,
                quote_asset=quote_asset,
                settlement_asset=settlement_asset,
                contract_size=1, # default
                price_precision=utils.int_precision(tick_size),
                qty_precision=utils.int_precision(lot_size),
                price_tick_size=tick_size,
                qty_lot_size=lot_size,
                min_notional=None,
                max_notional=None,
                min_price=int(exch["minOrderPrice"]) / base,
                max_price=int(exch["maxOrderPrice"]) / base,
                min_order_qty=lot_size,
                max_order_qty=int(exch["maxLimitOrderSize"]) / base,
                extra_properties=dict(),
                original_info=str(exch)
            )
        
            records.append(instrument_ref_data)
        
        self.loop.run_until_complete(self.client.close_connections())
        
        return records