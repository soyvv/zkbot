import time
from urllib import response

from zk_refdata_loader import utils
from zk_refdata_loader.exchange_base import ExchangeBase
from zk_proto_betterproto.common import InstrumentRefData, InstrumentType


class Blitz(ExchangeBase):
    def __init__(self, **kwargs):
        super().__init__(**kwargs)
        
    def calc_max_lev(self, long_weight_initial_x18: str):
        long_ = round(1 / (1 - (float(long_weight_initial_x18)/1e18)))
        
        return long_ 

    def fetch(self):
        records = list()
        request_sym_url = "https://gateway.blast-prod.vertexprotocol.com/query?type=symbols"
        request_pair_url = "https://gateway.blast-prod.vertexprotocol.com/v2/pairs?=market={spot|perp}"
        
        response_sym = self.request(request_sym_url)["data"]["symbols"]
        response_pair = self.request(request_pair_url)
        
        base_quote_map = {each["base"]: each["quote"] for each in response_pair}
        
        norm = 1e18
                
        for sym in response_sym.values():
            is_perp = sym["type"] == "perp"
            exch_symbol = sym["symbol"]
            base_asset = sym["symbol"][:-5] if is_perp else sym["symbol"]
            quote_asset = base_quote_map[sym["symbol"]]
            settlement_asset = quote_asset if is_perp else None
            type_suffix = "-P" if is_perp else ""
            exch_name = "BLITZ"
            tick_size = int(sym["price_increment_x18"]) / norm
            lot_size = int(sym["size_increment"]) / norm
            instrument_ref_data = InstrumentRefData(
                instrument_id=self.instrument_id(base_asset, type_suffix, quote_asset, exch_name),
                instrument_id_exchange=exch_symbol,
                update_ts=int(time.time()),
                exch_name=exch_name,
                instrument_type=InstrumentType.INST_TYPE_PERP if is_perp else InstrumentType.INST_TYPE_SPOT, 
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
                min_order_qty=float(int(sym["min_size"]) / norm),
                max_order_qty=None,
                extra_properties=dict({"max_leverage": str(self.calc_max_lev(sym['long_weight_initial_x18']))}) if is_perp else dict(),
                original_info=str(sym)
            )
        
            records.append(instrument_ref_data)

        return records