from typing import Union
from zk_datamodel import oms
from zk_datamodel import common

from dataclasses import dataclass
import betterproto
import json
import ast
from datetime import datetime

from .enrich_data_with_config import enrich_data_with_config

import logging
logging.basicConfig(format="%(levelname)s:%(filename)s:%(lineno)d:%(asctime)s:%(message)s", level=logging.DEBUG)

logger = logging.getLogger(__name__)


@dataclass
class BalanceSnapshot:
    account_id: int = None
    account_name: str = None
    exch: str = None
    instrument_mo: str = None
    instrument_exch: str = None
    instrument_tq: str = None
    instrument_type: str = None
    sync_timestamp: int = None
    update_timestamp: int = None
    side: str = None
    total_qty: float = None
    avail_qty: float = None
    frozen_qty: float = None
    raw_data: str = None


@dataclass
class PositionExtraData:
    contracts: float = None
    contract_size: float = None
    pos_qty: float = None  # contracts * contract_size

    unsettled_pnl: float = None
    avg_entry_price: float = None
    index_price: float = None
    last_trade_price: float = None
    leverage: float = None
    
    liquidation_price: float = None
    margin: float = None  # initial margin


def _try_parse_float(s: str) -> float:
    if s is None:
        return .0
    return float(s)

'''
ccxt position example:
{
   'info': { ... },             // json response returned from the exchange as is
   'id': '1234323',             // string, position id to reference the position, similar to an order id
   'symbol': 'BTC/USD',         // uppercase string literal of a pair of currencies
   'timestamp': 1607723554607,  // integer unix time since 1st Jan 1970 in milliseconds
   'datetime': '2020-12-11T21:52:34.607Z',  // ISO8601 representation of the unix time above
   'isolated': true,            // boolean, whether or not the position is isolated, as opposed to cross where margin is added automatically
   'hedged': false,             // boolean, whether or not the position is hedged, i.e. if trading in the opposite direction will close this position or make a new one
   'side': 'long',              // string, long or short
   'contracts': 5,              // float, number of contracts bought, aka the amount or size of the position
   'contractSize': 100,         // float, the size of one contract in quote units
   'entryPrice': 20000,         // float, the average entry price of the position
   'markPrice': 20050,          // float, a price that is used for funding calculations
   'notional': 100000,          // float, the value of the position in the settlement currency
   'leverage': 100,             // float, the leverage of the position, related to how many contracts you can buy with a given amount of collateral
   'collateral': 5300,          // float, the maximum amount of collateral that can be lost, affected by pnl
   'initialMargin': 5000,       // float, the amount of collateral that is locked up in this position
   'maintenanceMargin': 1000,   // float, the mininum amount of collateral needed to avoid being liquidated
   'initialMarginPercentage': 0.05,      // float, the initialMargin as a percentage of the notional
   'maintenanceMarginPercentage': 0.01,  // float, the maintenanceMargin as a percentage of the notional
   'unrealizedPnl': 300,        // float, the difference between the market price and the entry price times the number of contracts, can be negative
   'liquidationPrice': 19850,   // float, the price at which collateral becomes less than maintenanceMargin
   'marginMode': 'cross',       // string, can be cross or isolated
   'percentage': 3.32,          // float, represents unrealizedPnl / initialMargin * 100
}
'''
def convert_ccxt_position(position: oms.Position) -> PositionExtraData:
    extra_data = PositionExtraData()
    exch_raw_data = ast.literal_eval(position.exch_data_raw)

    extra_data.contracts = _try_parse_float(exch_raw_data['contracts'])
    extra_data.contract_size = _try_parse_float(exch_raw_data['contractSize'])
    extra_data.pos_qty = extra_data.contracts * extra_data.contract_size

    extra_data.unsettled_pnl = _try_parse_float(exch_raw_data['unrealizedPnl'])
    extra_data.avg_entry_price = _try_parse_float(exch_raw_data['entryPrice'])
    extra_data.index_price = _try_parse_float(exch_raw_data['markPrice'])
    extra_data.last_trade_price = _try_parse_float(exch_raw_data['markPrice'])
    extra_data.leverage = _try_parse_float(exch_raw_data['leverage'])

    extra_data.liquidation_price = _try_parse_float(exch_raw_data['liquidationPrice'])
    extra_data.margin = _try_parse_float(exch_raw_data['initialMargin'])
    return extra_data


def convert_paradex_position(position: oms.Position) -> PositionExtraData:
    extra_data = PositionExtraData()
    exch_raw_data = ast.literal_eval(position.exch_data_raw)
    extra_data.unsettled_pnl = _try_parse_float(exch_raw_data['unrealized_pnl'])
    extra_data.avg_entry_price = _try_parse_float(exch_raw_data['average_entry_price'])
    extra_data.leverage = .0
    extra_data.margin = .0
    extra_data.pos_qty = position.total_qty
    extra_data.contract_size = 1
    extra_data.contracts = position.total_qty
    return extra_data



class VertexPositionEnricher:

    def __init__(self):
        pass

    def convert_vertex_position(self, position: oms.Position) -> PositionExtraData:
        pass


# def convert_vertex_position(position: oms.Position) -> PositionExtraData:
#     extra_data = PositionExtraData()
#     exch_raw_data = json.loads(position.exch_data_raw)
#     extra_data.unsettled_pnl = _try_parse_float(exch_raw_data['unrealized_pnl'])
#     extra_data.avg_entry_price = _try_parse_float(exch_raw_data['average_entry_price'])
#     extra_data.leverage = .0
#     extra_data.margin = .0
#     return extra_data

def default_position_converter(position: oms.Position) -> PositionExtraData:


    extra_data = PositionExtraData()
    #exch_raw_data = json.loads(position.exch_data_raw)
    extra_data.unsettled_pnl = .0
    extra_data.avg_entry_price = .0
    extra_data.leverage = .0
    extra_data.margin = .0

    extra_data.pos_qty = position.total_qty
    extra_data.contract_size = 1
    extra_data.contracts = position.total_qty

    return extra_data


_EXCH_SPECIFIC_POSITION_CONVERTERS = {
    'BINANCEDM': convert_ccxt_position,
    'BINANCE': convert_ccxt_position,
    'KUCOIN': convert_ccxt_position,
    'KUCOINDM': convert_ccxt_position,
    'OKX': convert_ccxt_position,
    'OKXDM': convert_ccxt_position,
    'PARADEX': convert_paradex_position,
    'VERTEX': VertexPositionEnricher().convert_vertex_position
}


def enrich_balance(balance: oms.Position,
                   account_detail: dict,
                   symbol_detail: dict) -> BalanceSnapshot:
    balance_snapshot = BalanceSnapshot()

    data_from_config = enrich_data_with_config(
        instrument_tq=balance.instrument_code,
        account_detail=account_detail,
        symbol_detail=symbol_detail,
        instrument_type=balance.instrument_type)

    balance_snapshot.account_name = data_from_config.account_name
    balance_snapshot.exch = data_from_config.exch
    balance_snapshot.instrument_mo = data_from_config.instrument_mo
    balance_snapshot.instrument_exch = data_from_config.instrument_exch
    balance_snapshot.instrument_type = data_from_config.instrument_type

    balance_snapshot.account_id = balance.account_id
    balance_snapshot.instrument_tq = balance.instrument_code
    balance_snapshot.sync_timestamp = balance.sync_timestamp
    balance_snapshot.update_timestamp = balance.update_timestamp
    balance_snapshot.side = "short" if balance.long_short_type == common.LongShortType.LS_SHORT else "long"
    
    balance_snapshot.total_qty = balance.total_qty
    balance_snapshot.avail_qty = balance.avail_qty
    balance_snapshot.frozen_qty = balance.frozen_qty
    balance_snapshot.raw_data = balance.exch_data_raw

    return balance_snapshot


def convert_position_extra_data(position: oms.Position, 
                                account_detail: dict,
                                symbol_detail: dict) -> Union[PositionExtraData, None]:
    if account_detail is None or symbol_detail is None:
        return None
    if symbol_detail['instrument_type'] == "SPOT":
        return None
    if not position.is_from_exch or position.exch_data_raw == '':
        return None
    
    exch_name = account_detail['exch']
    exch_specific_position_converter = _EXCH_SPECIFIC_POSITION_CONVERTERS.get(exch_name)    
    if exch_specific_position_converter:
        position_extra_data = exch_specific_position_converter(position)
        return position_extra_data
    else:
        position_extra_data = default_position_converter(position)
        return position_extra_data
