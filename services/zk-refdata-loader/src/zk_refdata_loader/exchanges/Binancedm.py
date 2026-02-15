import time

from zk_refdata_loader.exchange_base import ExchangeBase
from zk_datamodel.common import InstrumentType, InstrumentRefData


class Binancedm(ExchangeBase):
    def __init__(self, **kwargs):
        super().__init__(**kwargs)

    def fetch(self):       
        records = list()
        request_url = "https://fapi.binance.com/fapi/v1/exchangeInfo"
        response = self.request(request_url)

        for sym in response["symbols"]:
            # skip non-perpetual symbols
            if sym["contractType"] != "PERPETUAL":
                continue
            
            filters = {f["filterType"]: f for f in sym["filters"]}

            def getFilterField(filterType, field, type):
                if filterType in filters and field in filters[filterType]:
                    return type(filters[filterType][field])
                return None

            base_asset = sym["baseAsset"]
            quote_asset = sym["quoteAsset"]
            settlement_asset = sym["marginAsset"]
            type_suffix = "-P"
            exch_name = "BINANCEDM"
            tick_size = getFilterField("PRICE_FILTER", "tickSize", float)
            lot_size = getFilterField("LOT_SIZE", "stepSize", float)
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
                price_precision=int(sym["pricePrecision"]),
                qty_precision=int(sym["quantityPrecision"]),
                price_tick_size=tick_size,
                qty_lot_size=lot_size,
                min_notional=getFilterField("MIN_NOTIONAL", "notional", float),
                max_notional=None,
                min_price=getFilterField("PRICE_FILTER", "minPrice", float),
                max_price=getFilterField("PRICE_FILTER", "maxPrice", float),
                min_order_qty=getFilterField("LOT_SIZE", "minQty", float),
                max_order_qty=getFilterField("LOT_SIZE", "maxQty", float),
                extra_properties=dict(),
                original_info=str(sym),
                max_mkt_order_qty=getFilterField("MARKET_LOT_SIZE", "maxQty", float)
            )

            records.append(instrument_ref_data)

        return records
