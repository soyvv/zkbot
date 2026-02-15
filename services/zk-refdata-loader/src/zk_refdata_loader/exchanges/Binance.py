import time

from zk_datamodel.common import InstrumentRefData, InstrumentType

from zk_refdata_loader import utils
from zk_refdata_loader.exchange_base import ExchangeBase

class Binance(ExchangeBase):
    def __init__(self, **kwargs):
        super().__init__(**kwargs)

    def fetch(self):
        records = list()
        request_url = "https://api.binance.com/api/v1/exchangeInfo"
        response = self.request(request_url)

        for sym in response["symbols"]:            
            filters = {f["filterType"]: f for f in sym["filters"]}

            def get_filter_field(filter_type, field, type):
                if filter_type in filters and field in filters[filter_type]:
                    return type(filters[filter_type][field])
                return None

            base_asset = sym["baseAsset"]
            quote_asset = sym["quoteAsset"]
            settlement_asset = None
            type_suffix = ""
            exch_name = "BINANCE"
            tick_size = get_filter_field("PRICE_FILTER", "tickSize", float)
            lot_size = get_filter_field("LOT_SIZE", "stepSize", float)
            instrument_ref_data = InstrumentRefData(
                instrument_id=self.instrument_id(base_asset, type_suffix, quote_asset, exch_name),
                instrument_id_exchange=self.instrument_id_ccxt(base_asset, quote_asset, settlement_asset),
                update_ts=int(time.time() * 1000),
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
                min_notional=get_filter_field("NOTIONAL", "minNotional", float),
                max_notional=get_filter_field("NOTIONAL", "maxNotional", float),
                min_price=get_filter_field("PRICE_FILTER", "minPrice", float),
                max_price=get_filter_field("PRICE_FILTER", "maxPrice", float),
                min_order_qty=get_filter_field("LOT_SIZE", "minQty", float),
                max_order_qty=get_filter_field("LOT_SIZE", "maxQty", float),
                extra_properties=dict(),
                original_info=str(sym),
                max_mkt_order_qty=get_filter_field("MARKET_LOT_SIZE", "maxQty", float)
            )

            records.append(instrument_ref_data)

        return records
