from typing import Protocol, Callable

import zk_datamodel.oms as oms
import zk_datamodel.rtmd as rtmd
import zk_datamodel.common as common

from .api import TokkaQuant
from datetime import datetime, timedelta


INIT_FUNCTION_TYPE = Callable[[any, datetime], any]

class StrategyProtocol(Protocol):

    def on_create(self, tq: TokkaQuant):
        pass

    def on_destroy(self, tq: TokkaQuant):
        pass

    def on_reinit(self, config, tq: TokkaQuant):
        pass

    def on_pause(self, tq: TokkaQuant):
        pass

    def on_tick(self, tick: rtmd.TickData, tq: TokkaQuant):
        pass

    def on_scheduled_time(self, time_event, tq: TokkaQuant):
        pass

    def on_signal(self, signal: rtmd.RealtimeSignal, tq: TokkaQuant):
        pass

    def on_bar(self, bar: rtmd.Kline, tq: TokkaQuant):
        pass

    def on_orderupdate(self, order_update: oms.OrderUpdateEvent, tq: TokkaQuant):
        pass

    def on_positionupdate(self, position_update: oms.PositionUpdateEvent, tq: TokkaQuant):
        pass

    def on_error(self, error_event, tq: TokkaQuant):
        pass

    def on_cmd(self, cmd: any, tq: TokkaQuant):
        pass



class StrategyBase(StrategyProtocol):
    def __init__(self) -> None:
        pass

    def on_create(self, tq: TokkaQuant):
        pass

    def on_destroy(self, tq: TokkaQuant):
        pass

    def on_reinit(self, config, tq: TokkaQuant):
        pass

    def on_pause(self, tq: TokkaQuant):
        pass

    def on_tick(self, tick: rtmd.TickData, tq: TokkaQuant):
        pass

    def on_signal(self, signal: rtmd.RealtimeSignal, tq: TokkaQuant):
        pass

    def on_bar(self, bar: rtmd.Kline, tq: TokkaQuant):
        pass

    def on_scheduled_time(self, time_event, tq: TokkaQuant):
        pass

    def on_orderupdate(self, order_update: oms.OrderUpdateEvent, tq: TokkaQuant):
        pass

    def on_positionupdate(self, position_update: oms.PositionUpdateEvent, tq: TokkaQuant):
        pass

    def on_error(self, error_event, tq: TokkaQuant):
        pass

    def on_cmd(self, cmd: any, tq: TokkaQuant):
        pass
