import logging
from math import log10
import time

import requests
import configparser

from zk_refdata_loader import utils

from zk_refdata_loader.exchange_base import ExchangeBase
from zk_datamodel.common import InstrumentRefData, InstrumentType

class Injective(ExchangeBase):
    def __init__(self, **kwargs):
        super().__init__(**kwargs)
        self.url = "https://raw.githubusercontent.com/InjectiveLabs/sdk-python/master/pyinjective/denoms_mainnet.ini"
        self.cfg = configparser.ConfigParser()
        self.handle_spot = True
        self.handle_perp = False

    def fetch(self):
        request_url = "https://raw.githubusercontent.com/InjectiveLabs/sdk-python/master/pyinjective/denoms_mainnet.ini"
        response_str = self._request_str(request_url)
        self.cfg.read_string(response_str)
        records = list()
        
        for section in self.cfg.sections():
            data = {key: value for key, value in self.cfg.items(section)}
            
            if 'peggy_denom' in data:
                continue
            
            descr = data["description"].replace("'", "")
            
            is_perp = descr.find("PERP") != -1
            is_spot = descr.find("Spot") != -1

            if not is_perp and not is_spot:
                continue
            if(not self.handle_perp and is_perp):
                continue
            if(not self.handle_spot and is_spot):
                continue

            exch_symbol = descr.split()[2]
            base_asset, quote_asset = exch_symbol.split("/")
            type_suffix = ""
            exch_name = "INJECTIVE"
            settlement_asset = None
            
            base_decs= int(data['base'])
            quote_decs =int(data['quote'])
            min_price_tick_size = float(data['min_price_tick_size'])
            
            min_qty_tick_size = float(data['min_quantity_tick_size'])
            min_display_qty_tick_size = float(data['min_display_quantity_tick_size'])

            price_precision = round(
                            -(log10(min_price_tick_size)
                            + base_decs - quote_decs))
            size_precision = round(
                            -(log10(min_qty_tick_size)
                            - base_decs))
            
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
                price_precision=int(price_precision),
                qty_precision=int(size_precision),
                price_tick_size=utils.float_precision(price_precision),
                qty_lot_size=utils.float_precision(size_precision),
                min_notional=None,
                max_notional=None,
                min_price=None,
                max_price=None,
                min_order_qty=min_display_qty_tick_size,
                max_order_qty=None,
                extra_properties=dict(),
                original_info=str(data)
            )
    
            records.append(instrument_ref_data)        
            
        return records
    
    def _request_str(self, url:str, headers=None):
        failure_count = 0
        while True:
            try:
                msg = requests.get(url, headers=headers if headers else {}).content.decode("utf-8")
                return msg
            except Exception as e:
                failure_count += 1
                logging.exception(
                    f"Exchange {self.__class__.__name__} url request failure -> msg: {e}, url: {url}")
            
            if failure_count >= 20:
                return None
