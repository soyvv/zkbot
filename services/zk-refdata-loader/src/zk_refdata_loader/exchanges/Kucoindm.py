import time

from zk_refdata_loader import utils
from zk_refdata_loader.exchange_base import ExchangeBase
from zk_datamodel.common import InstrumentType, InstrumentRefData


class Kucoindm(ExchangeBase):
    def __init__(self, **kwargs):
        super().__init__(**kwargs)

    def fetch(self):
        records = list()
        request_url = "https://api-futures.kucoin.com/api/v1/contracts/active"
        
        response_spot = self.request(request_url)
        for sym in response_spot['data']:
            base_asset = sym['baseCurrency']
            quote_asset = sym['quoteCurrency']
            settlement_asset = sym['settleCurrency']
            type_suffix = "-P"
            exch_name = "KUCOINDM"
            tick_size = sym['tickSize']
            lot_size = sym['lotSize']
            instrument_ref_data = InstrumentRefData(
                instrument_id=self.instrument_id(base_asset, type_suffix, quote_asset, exch_name),
                instrument_id_exchange=self.instrument_id_ccxt(base_asset, quote_asset, settlement_asset),
                update_ts=int(time.time()),
                exch_name=exch_name,
                instrument_type=InstrumentType.INST_TYPE_PERP,  # P perpetual
                base_asset=base_asset,
                quote_asset=quote_asset,
                settlement_asset=settlement_asset,
                contract_size=1,  # default
                price_precision=utils.int_precision(tick_size),
                qty_precision=utils.int_precision(lot_size),
                price_tick_size=tick_size,
                qty_lot_size=lot_size,
                min_notional=None,
                max_notional=None,
                min_price=None,
                max_price=float(sym['maxPrice']),
                min_order_qty=None,
                max_order_qty=float(sym['maxOrderQty']),
                extra_properties=dict(),
                original_info=str(sym)
            )
            
            records.append(instrument_ref_data)
        
        return records