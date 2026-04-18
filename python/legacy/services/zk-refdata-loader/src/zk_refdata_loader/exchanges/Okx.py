import time

from zk_refdata_loader import utils
from zk_refdata_loader.exchange_base import ExchangeBase
from zk_proto_betterproto.common import InstrumentType, InstrumentRefData

class Okx(ExchangeBase):
    def __init__(self, **kwargs):
        super().__init__(**kwargs)
    
    def fetch(self):
        records = list()
        request_url_spot = "https://www.okx.com/api/v5/public/instruments?instType=SPOT"
        request_url_swap = "https://www.okx.com/api/v5/public/instruments?instType=SWAP"
        response_spot = self.request(request_url_spot)
        response_swap = self.request(request_url_swap)
        
        for data in response_spot["data"]:
            base_asset = data["baseCcy"]
            quote_asset = data["quoteCcy"]
            settlement_asset = None
            type_suffix = ""
            exch_name = "OKX"
            tick_size = float(data["tickSz"])
            lot_size = float(data["lotSz"])
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
                min_notional=None,
                max_notional=None,
                min_price=None,
                max_price=None,
                min_order_qty=data['minSz'],
                max_order_qty=data['maxMktSz'],
                extra_properties=dict(),
                original_info=str(data)
            )
            
            records.append(instrument_ref_data)
        
        for data in response_swap["data"]:
            base_asset, quote_asset = str(data["instFamily"]).split("-", maxsplit=1)
            settlement_asset = data['settleCcy']
            type_suffix = "-P"
            exch_name = "OKXDM"
            tick_size = float(data["tickSz"])
            lot_size = float(data["lotSz"])
            contract_size = float(data["ctVal"])
            instrument_ref_data = InstrumentRefData(
                instrument_id=self.instrument_id(base_asset, type_suffix, quote_asset, exch_name),
                instrument_id_exchange=self.instrument_id_ccxt(base_asset, quote_asset, settlement_asset),
                update_ts=int(time.time()),
                exch_name=exch_name,
                instrument_type=InstrumentType.INST_TYPE_PERP,  # P perpetual
                base_asset=base_asset,
                quote_asset=quote_asset,
                settlement_asset=settlement_asset,
                contract_size=contract_size,  # default
                price_precision=utils.int_precision(tick_size),
                qty_precision=utils.int_precision(lot_size),
                price_tick_size=tick_size,
                qty_lot_size=lot_size,
                min_notional=None,
                max_notional=None,
                min_price=None,
                max_price=None,
                min_order_qty=data['minSz'],
                max_order_qty=data['maxMktSz'],
                extra_properties=dict(),
                original_info=str(data)
            )
            
            records.append(instrument_ref_data)
        
        return records
            