import asyncio
import time

from zk_refdata_loader import utils
from zk_refdata_loader.exchange_base import ExchangeBase
from zk_datamodel.common import InstrumentType, InstrumentRefData

class BluefinSui(ExchangeBase):
    def __init__(self, **kwargs):
        super().__init__(**kwargs)
        
    def fetch(self):
        request_url = "https://dapi.api.sui-prod.bluefin.io/exchangeInfo"
        response = self.request(request_url)
        
        records = list()
        base = 10**18
        for sym in response:
            if 'error' in sym:
                continue
            
            tick_size = float(int(sym["tickSize"]) / base)
            lot_size = float(int(sym["stepSize"]) / base)
            base_asset = sym["baseAssetSymbol"]
            quote_asset = sym["quoteAssetSymbol"]
            settlement_asset = quote_asset
            type_suffix = "-P"
            exch_name = "BLUEFINSUI"
            instrument_ref_data = InstrumentRefData(
                instrument_id=self.instrument_id(base_asset, type_suffix, quote_asset, exch_name),
                instrument_id_exchange=f"{base_asset}-PERP",
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
                min_price=float(sym["minOrderPrice"]) / base,
                max_price=float(sym["maxOrderPrice"]) / base,
                min_order_qty=float(sym["minOrderSize"]) / base,
                max_order_qty=float(sym["maxMarketOrderSize"]) / base,
                extra_properties=dict(),
                original_info=str(sym)
            )
        
            records.append(instrument_ref_data)
                
        return records