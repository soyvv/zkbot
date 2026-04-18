
from datetime import datetime

from .models import *

import abc

import pandas as pd
import enum

import zk_proto_betterproto.rtmd as rtmd

class COLS(enum.Enum):
    tick_type = "tick_type" # OB=Orderbook/T=Trade/M=Merged
    timestamp = 'timestamp'
    symbol = 'symbol'
    b1 = 'b1'
    b2 = 'b2'
    b3 = 'b3'
    b4 = 'b4'
    b5 = 'b5'

    s1 = 's1'
    s2 = 's2'
    s3 = 's3'
    s4 = 's4'
    s5 = 's5'

    bv1 = 'bv1'
    bv2 = 'bv2'
    bv3 = 'bv3'
    bv4 = 'bv4'
    bv5 = 'bv5'

    sv1 = 'sv1'
    sv2 = 'sv2'
    sv3 = 'sv3'
    sv4 = 'sv4'
    sv5 = 'sv5'

    trade_price = "trade_price"
    trade_qty = "trade_qty"
    trade_direction = "trade_side"

class BAR_COLS(enum.Enum):
    kline_type = "kline_type" # 1m/5m/etc.
    open = "open"
    close = "close"
    high = "high"
    low = "low"
    volume = "volume"
    amount = "amount"
    timestamp = "timestamp"
    symbol = "symbol"




class MDDataSource(abc.ABC):
    def __init__(self):
        pass

    @abc.abstractmethod
    def get_data(self, symbol: str, start_dt: datetime, end_dt: datetime) -> pd.DataFrame:
        pass


    def get_data_multiple_symbols(self, symbols: list[str], start_dt: datetime, end_dt: datetime) -> pd.DataFrame:
        dfs = []
        for symbol in symbols:
            df = self.get_data(symbol, start_dt, end_dt)
            if df is not None:
                dfs.append(df)

        if len(dfs) == 1:
            return dfs[0]
        elif len(dfs) > 1:
            merged_df =  pd.concat(dfs)
            merged_df = merged_df.sort_values(by=COLS.timestamp.value)
            return merged_df
        else:
            return None



class SignalDataSource(abc.ABC):
    def __init__(self):
        pass

    @abc.abstractmethod
    def get_data(self, start_dt: datetime, end_dt: datetime) -> pd.DataFrame:
        pass



class KlineDataSource(abc.ABC):
    def __init__(self):
        pass

    @abc.abstractmethod
    def get_data(self, symbol: str, start_dt: datetime, end_dt: datetime) -> pd.DataFrame:
        pass


    def get_data_multiple_symbols(self, symbols: list[str], start_dt: datetime, end_dt: datetime) -> pd.DataFrame:
        dfs = []
        for symbol in symbols:
            dfs.append(self.get_data(symbol, start_dt, end_dt))

        if len(dfs) == 1:
            return dfs[0]
        elif len(dfs) > 1:
            merged_df = pd.concat(dfs)
            merged_df = merged_df.sort_values(by=BAR_COLS.timestamp.value)
            return merged_df
        else:
            return None



def convert_to_obj_ob(df: pd.DataFrame, idx: int) -> rtmd.TickData:
    tick = rtmd.TickData()
    tick_type = df.loc[idx, COLS.tick_type.value]
    tick.tick_type = rtmd.TickUpdateType.from_string(tick_type)

    tick.instrument_code = df.loc[idx, COLS.symbol.value]
    tick.original_timestamp = df.loc[idx, COLS.timestamp.value]
    if COLS.b1.value in df.columns:
        tick.buy_price_levels = [
            rtmd.PriceLevel(price=df.loc[idx, COLS.b1.value], qty=df.loc[idx, COLS.bv1.value]),
            rtmd.PriceLevel(price=df.loc[idx, COLS.b2.value], qty=df.loc[idx, COLS.bv2.value]),
            rtmd.PriceLevel(price=df.loc[idx, COLS.b3.value], qty=df.loc[idx, COLS.bv3.value]),
            rtmd.PriceLevel(price=df.loc[idx, COLS.b4.value], qty=df.loc[idx, COLS.bv4.value]),
            rtmd.PriceLevel(price=df.loc[idx, COLS.b5.value], qty=df.loc[idx, COLS.bv5.value]),
        ]

        tick.sell_price_levels = [
            rtmd.PriceLevel(price=df.loc[idx, COLS.s1.value], qty=df.loc[idx, COLS.sv1.value]),
            rtmd.PriceLevel(price=df.loc[idx, COLS.s2.value], qty=df.loc[idx, COLS.sv2.value]),
            rtmd.PriceLevel(price=df.loc[idx, COLS.s3.value], qty=df.loc[idx, COLS.sv3.value]),
            rtmd.PriceLevel(price=df.loc[idx, COLS.s4.value], qty=df.loc[idx, COLS.sv4.value]),
            rtmd.PriceLevel(price=df.loc[idx, COLS.s5.value], qty=df.loc[idx, COLS.sv5.value]),
        ]

    if COLS.trade_price.value in df.columns:
        tick.latest_trade_price = df.loc[idx, COLS.trade_price.value]
        tick.latest_trade_qty = df.loc[idx, COLS.trade_qty.value]
        tick.latest_trade_side = df.loc[idx, COLS.trade_direction.value]
    return tick

def convert_to_obj_signal(df: pd.DataFrame, idx: int, signal_names: list[str]) -> rtmd.RealtimeSignal:
    signal = rtmd.RealtimeSignal()
    signal.instrument = df.loc[idx, "symbol"] # optional
    signal.timestamp = df.loc[idx, "ts"]
    signal.data = df.loc[idx, signal_names].to_dict()
    return signal


def convert_to_obj_kline(df: pd.DataFrame, idx: int) -> rtmd.Kline:
    kline = rtmd.Kline()
    kline.kline_type = rtmd.KlineKlineType.from_string(df.loc[idx, BAR_COLS.kline_type.value])
    kline.timestamp = df.loc[idx, BAR_COLS.timestamp.value]
    kline.symbol = df.loc[idx, BAR_COLS.symbol.value]
    kline.open = df.loc[idx, BAR_COLS.open.value]
    kline.close = df.loc[idx, BAR_COLS.close.value]
    kline.high = df.loc[idx, BAR_COLS.high.value]
    kline.low = df.loc[idx, BAR_COLS.low.value]
    kline.volume = df.loc[idx, BAR_COLS.volume.value]
    kline.amount = df.loc[idx, BAR_COLS.amount.value]
    kline.kline_end_timestamp = df.loc[idx, BAR_COLS.timestamp.value]
    return kline

def copy_to_obj(df: pd.DataFrame, idx: int, tick: rtmd.TickData):
    pass
