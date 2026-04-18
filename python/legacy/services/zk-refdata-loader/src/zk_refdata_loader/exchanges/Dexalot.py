import time

from zk_refdata_loader import utils
from zk_refdata_loader.exchange_base import ExchangeBase
from zk_proto_betterproto.common import InstrumentRefData, InstrumentType


class Dexalot(ExchangeBase):
    def __init__(self, **kwargs):
        super().__init__(**kwargs)

    def fetch(self):
        records = list()
        request_url = "https://api.dexalot.com/privapi/trading/pairs"
        response = self.request(
            request_url,headers={"x-apikey": "02f1e66b-ab1e-498c-b730-2bcfb7cff4a2"}
        )
        
        for sym in response:
            base_asset = sym["base"].upper()
            quote_asset = sym["quote"].upper()
            settlement_asset = None
            type_suffix = ""
            exch_name = "DEXALOT"
            price_precision = sym["quotedisplaydecimals"]
            qty_precision = sym["basedisplaydecimals"]
            tick_size = utils.float_precision(price_precision)
            lot_size = utils.float_precision(qty_precision)
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
                price_precision=price_precision,
                qty_precision=qty_precision,
                price_tick_size=tick_size,
                qty_lot_size=lot_size,
                min_notional=float(sym["mintrade_amnt"]),
                max_notional=float(sym["maxtrade_amnt"]),
                min_price=None,
                max_price=None,
                min_order_qty=lot_size,
                max_order_qty=None,
                extra_properties=dict(),
                original_info=str(sym)
            )
            records.append(instrument_ref_data)
        
        return records