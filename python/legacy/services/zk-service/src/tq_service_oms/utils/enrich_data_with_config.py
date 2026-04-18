from dataclasses import dataclass
from typing import Optional

from zk_proto_betterproto import common


@dataclass
class DataFromConfig:
    account_name: str = None
    exch: str = None
    instrument_mo: str = None
    instrument_exch: str = None
    instrument_type: str = None

def enrich_data_with_config(
    instrument_tq: str, 
    account_detail: Optional[dict], 
    symbol_detail: Optional[dict], 
    instrument_type: common.InstrumentType = None
):
    data_from_config = DataFromConfig()

    if account_detail:
        data_from_config.account_name = account_detail['exch_account_id']
        data_from_config.exch = account_detail['exch']

    if symbol_detail:
        data_from_config.instrument_mo = symbol_detail['base_symbol'] + symbol_detail['quote_symbol']
        data_from_config.instrument_exch = symbol_detail['exch_symbol']
        data_from_config.instrument_type = symbol_detail['instrument_type']
    else:
        data_from_config.instrument_mo = instrument_tq
        data_from_config.instrument_exch = instrument_tq

        if instrument_type == common.InstrumentType.INST_TYPE_PERP:
            data_from_config.instrument_type = 'PERP'
        elif instrument_type == common.InstrumentType.INST_TYPE_SPOT:
            data_from_config.instrument_type = "SPOT"
        elif "/" not in instrument_tq and "@" not in instrument_tq:
            data_from_config.instrument_type = "SPOT"  # TODO: change Trade to indicate instrument type

    return data_from_config
