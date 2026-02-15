import time
import csv
from io import StringIO

import pkg_resources
from zk_datamodel.common import InstrumentRefData, InstrumentType

from zk_refdata_loader import utils
from zk_refdata_loader.exchange_base import ExchangeBase

class Ddex(ExchangeBase):
    def __init__(self, **kwargs):
        super().__init__(**kwargs)
        self.quote_assets = ['USD', 'SGD', 'HKD', 'JPY']
    
    def fetch(self): 
        records = list()
        #ddex_file = resources.open_text(static, 'ddexData.csv')
        ddex_file_content = pkg_resources.resource_string("zk_refdata_loader.static", 'ddexData.csv').decode('utf-8')
        ddex_file = StringIO(ddex_file_content)
        with ddex_file as file:
            cr = list(csv.DictReader(file))
            update_ts=int(time.time() * 1000)
            for row in cr:
                if row['FIAT/Symbol'] == 'FIAT Decimals':continue
                for quote_asset in self.quote_assets:
                    if row[quote_asset] == 'Y':
                        base_asset = row['FIAT/Symbol']
                        settlement_asset = None 
                        type_suffix = ""
                        exch_name = "DDEX"
                        instrument_ref_data = InstrumentRefData(
                            instrument_id=self.instrument_id(base_asset, type_suffix, quote_asset, exch_name),
                            instrument_id_exchange=f"{base_asset}{quote_asset}",
                            update_ts=update_ts,
                            exch_name=exch_name,
                            instrument_type=InstrumentType.INST_TYPE_SPOT,  # S spot
                            base_asset=base_asset,
                            quote_asset=quote_asset,
                            settlement_asset=settlement_asset,
                            contract_size=1 ,
                            price_precision = int(cr[-1][quote_asset]) ,
                            qty_precision = int(row['Token Decimals']) ,
                            price_tick_size=None ,
                            qty_lot_size=None,
                            min_notional=None, 
                            max_notional=None ,
                            min_price=None,
                            max_price=None ,
                            min_order_qty=float(row['Min Size']),
                            max_order_qty=float(row['Max Size']),
                            extra_properties=dict({'Date on DDEx': row['Date on DDEx'], 'Token Decimals': row['Token Decimals'], 'FIAT Decimals': str(cr[-1][quote_asset])}),
                            original_info=','.join(row)
                        )
                        records.append(instrument_ref_data)
        return records


        
        
