import time

from zk_refdata_loader import utils
from zk_refdata_loader.exchange_base import ExchangeBase
from zk_proto_betterproto.common import InstrumentType, InstrumentRefData


class Kucoin(ExchangeBase):
    def __init__(self, **kwargs):
        super().__init__(**kwargs)

    def fetch(self):
        records = list()
        request_url1 = "https://openapi-v2.kucoin.com/api/v1/symbols"
        request_url2 = "https://api.kucoin.com/api/v1/symbols"
        response = self.request(request_url1)
        if "data" not in response:
            response = self.request(request_url2)
        
        for data in response["data"]:
            base_asset = data["baseCurrency"]
            quote_asset = data["quoteCurrency"]
            settlement_asset = None 
            type_suffix = ""
            exch_name = "KUCOIN"
            tick_size = float(data["priceIncrement"])
            lot_size = float(data["baseIncrement"])
            instrument_ref_data = InstrumentRefData(
                instrument_id=self.instrument_id(base_asset, type_suffix, quote_asset, exch_name),
                instrument_id_exchange=self.instrument_id_ccxt(base_asset, quote_asset, settlement_asset),
                update_ts=int(time.time()),
                exch_name=exch_name,
                instrument_type=InstrumentType.INST_TYPE_SPOT,  # S spot
                base_asset=base_asset,
                quote_asset=quote_asset,
                settlement_asset=settlement_asset,
                contract_size=1, # default
                price_precision=utils.int_precision(tick_size),
                qty_precision=utils.int_precision(lot_size),
                price_tick_size=tick_size,
                qty_lot_size=lot_size,
                min_notional=float(data["quoteMinSize"]),
                max_notional=float(data["quoteMaxSize"]),
                min_price=None,
                max_price=None,
                min_order_qty=float(data["baseMinSize"]),
                max_order_qty=float(data["baseMaxSize"]),
                extra_properties=dict(),
                original_info=str(data)
            )

            records.append(instrument_ref_data)

        return records
