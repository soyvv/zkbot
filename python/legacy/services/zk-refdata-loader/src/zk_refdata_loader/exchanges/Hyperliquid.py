import time

from zk_refdata_loader import utils
from zk_refdata_loader.exchange_base import ExchangeBase
from zk_proto_betterproto.common import InstrumentRefData, InstrumentType


class Hyperliquid(ExchangeBase):
    def __init__(self, **kwargs):
        super().__init__(**kwargs)

    def fetch(self):
        records = list()
        request_url = "https://api.hyperliquid.xyz/info"
        response = self.request(
            method="POST",
            url=request_url,
            headers={"Content-Type": "application/json"},
            json={"type": "metaAndAssetCtxs"},            
        )

        universe = response[0]["universe"]
        asset_contexts = response[1]
        
        if len(universe) != len(asset_contexts):
            raise ValueError("base assets and asset contexts do not have the same length!")
        
        for base_info, context in zip(universe, asset_contexts):
            base_asset = base_info["name"]
            quote_asset = "USD"
            settlement_asset = "USD"
            type_suffix = "-P"
            exch_name = "HYPERLIQUID"
            tick_size = None
            lot_size = None
            instrument_ref_data = InstrumentRefData(
                instrument_id=self.instrument_id(base_asset, type_suffix, quote_asset, exch_name),
                instrument_id_exchange=base_asset,
                update_ts=int(time.time()),
                exch_name=exch_name,
                exch_symbol=base_asset,
                instrument_type=InstrumentType.INST_TYPE_PERP, # P perpetual
                base_asset=base_asset,
                quote_asset=quote_asset,
                settlement_asset=settlement_asset,
                contract_size=1,  # default
                price_precision=5,
                qty_precision=base_info["szDecimals"],
                price_tick_size=None,
                qty_lot_size=None,
                min_notional=None,
                max_notional=None,
                min_price=None,
                max_price=None,
                min_order_qty=None,
                max_order_qty=None,
                extra_properties=dict(),
                original_info=str((base_info, context)),
            )
            
            records.append(instrument_ref_data)
        
        
        return records

    