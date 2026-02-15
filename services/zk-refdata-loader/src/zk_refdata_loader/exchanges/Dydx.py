import time

from zk_refdata_loader import utils
from zk_refdata_loader.exchange_base import ExchangeBase
from zk_datamodel.common import InstrumentRefData, InstrumentType


class Dydx(ExchangeBase):
    def __init__(self, **kwargs):
        super().__init__(**kwargs)

    def fetch(self):
        records = list()
        request_url = "https://indexer.v4testnet.dydx.exchange/v4/perpetualMarkets"
        response = self.request(request_url)
        
        for data in response["markets"].values():
            base_asset, quote_asset = data["ticker"].split("-")
            settlement_asset = None
            type_suffix = ""
            exch_name = "DYDX"
            tick_size = float(data["tickSize"])
            lot_size = float(data["stepSize"])
            instrument_ref_data = InstrumentRefData(
                instrument_id=self.instrument_id(base_asset, type_suffix, quote_asset, exch_name),
                instrument_id_exchange=self.instrument_id_ccxt(base_asset, quote_asset, settlement_asset),
                update_ts=int(time.time()),
                exch_name=exch_name,
                instrument_type=InstrumentType.INST_TYPE_SPOT, # S Spot
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
                min_order_qty=None,
                max_order_qty=None,
                extra_properties=dict(),
                original_info=str(data)
            )
        
            records.append(instrument_ref_data)
        
        return records