import time

from zk_refdata_loader import utils
from zk_refdata_loader.exchange_base import ExchangeBase
from zk_proto_betterproto.common import InstrumentRefData, InstrumentType


class Paradex(ExchangeBase):
    def __init__(self, **kwargs):
        super().__init__(**kwargs)

    def fetch(self):
        records = list()
        request_url = "https://api.testnet.paradex.trade/v1/markets"
        
        response = self.request(request_url)
        
        for sym in response["results"]:
            symbol = sym["symbol"]
            base_asset = sym["base_currency"]
            quote_asset = sym["quote_currency"]  # use USDC as quote currency
            settlement_asset = sym["settlement_currency"]
            is_perp = sym["asset_kind"] == 'PERP'
            type_suffix = "-P" if is_perp else ""
            exch_name = "PARADEX"
            tick_size = float(sym["price_tick_size"]) # tick_size => quote_increment => Min tick size for quote currency => price_tick_size
            lot_size = float(sym["order_size_increment"])
            instrument_ref_data = InstrumentRefData(
                instrument_id=self.instrument_id(base_asset, type_suffix, quote_asset, exch_name),
                instrument_id_exchange=symbol,
                update_ts=int(time.time()),
                exch_name=exch_name,
                instrument_type=InstrumentType.INST_TYPE_PERP if is_perp else InstrumentType.INST_TYPE_SPOT, # P perpetual
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
                max_order_qty=sym["max_open_orders"],
                extra_properties=dict(),
                original_info=str(sym)
            )
        
            records.append(instrument_ref_data)
            
        return records
