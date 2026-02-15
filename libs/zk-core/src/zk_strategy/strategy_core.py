#import asyncio
import functools
import numpy as np
from dataclasses import dataclass
# import betterproto
# import tqrpc_lib.rtmd as rtmd
# import tqrpc_lib.oms as oms
#
# import asyncio
# import math
# from enum import Enum
import importlib.util
from typing import Type, Union, Callable
from pathlib import Path
import traceback as tb
#from zk_client.tqclient import TQClient
import zk_utils.zk_utils as tqutils

import zk_utils.zk_utils
import zk_datamodel.oms as oms
import zk_datamodel.tqrpc_oms as oms_rpc
import zk_datamodel.common as common
import zk_datamodel.rtmd as rtmd
import zk_datamodel.strategy as strat_pb
# from contextlib import suppress
# import uuid
# import signal
import logging
from datetime import datetime
import zk_utils.zk_utils as utils
from .strategy_base import StrategyBase
from .api import TokkaQuant
from .strategy_stateproxy import StrategyStateProxy
from .timer import TimerManager
from .models import SAction, TimerEvent, SActionType, StrategyOrder, StrategyCancel, StrategyLog, TimerSubscription


from loguru import logger
# logging.basicConfig(format="%(levelname)s:%(filename)s:%(lineno)d:%(asctime)s:%(message)s", level=logging.DEBUG)
# logger = logging.getLogger(__name__)


@dataclass
class StrategyConfig:
    strategy_name: str = None
    strategy_file: str = None
    account_ids: list[int] = None # accounts to subscribe from OMS
    symbols: list[str] = None  # symbols to subscribe from RTMD
    symbols_kline: list[str] = None  # symbols to subscribe from RTMD-klines

    custom_params: dict = None # strategy specific config

    daily_start_time = None # none means no daily start
    daily_stop_time = None # none means no daily stop
    diable_logging = False # logging can be disabled for backtesting

    symbol_info: list[common.InstrumentRefData] = None




def load_strategy(file_path: str) -> Union[Type[StrategyBase], None]:
    """
    Load a strategy class from a Python script at the given file_path.

    :param file_path: Path to the Python script file.
    :return: A subclass of StrategyBase if found, otherwise None.
    """
    import sys, os
    if not Path(file_path).exists():
        raise FileNotFoundError(f"File '{file_path}' not found")

    # module_name = os.path.basename(file_path)
    # module_name = os.path.splitext(module_name)[0]
    #



    module_name = os.path.splitext(os.path.basename(file_path))[0]
    module_dir = os.path.dirname(file_path)

    print(module_name)
    print(module_dir)

    # Add the module's directory to sys.path
    if module_dir not in sys.path:
        sys.path.insert(0, module_dir)


    spec = importlib.util.spec_from_file_location(module_name, file_path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)

    strategy_cls = None
    for name, obj in module.__dict__.items():
        if isinstance(obj, type) and issubclass(obj, StrategyBase) and obj != StrategyBase:
            # this will exclude other classes that are imported in the strategy file
            if hasattr(obj, "__module__") and obj.__module__ == module.__name__:
                strategy_cls = obj
                break

    init_func = None
    for name, obj in module.__dict__.items():
        if callable(obj) and name == "__tq_init__":
            init_func = obj
            break

    if strategy_cls:
        if init_func:
            strategy_cls.__tq_init__ = init_func
        return strategy_cls
    else:
        raise Exception(f"Strategy class not found in file '{file_path}'")


def handle_event(handler):
    @functools.wraps(handler)
    def wrapped(ref, event) -> list[SAction]:
        actions = []
        try:
            handler(ref, event)
            actions.extend(ref.tq._collect_pending_actions())
        except Exception as ex:
            err_msg = tb.format_exc()
            logger.error(err_msg)
            ref.tq.set_error_state(err_msg)
        finally:
            ref.tq._clear_pending_actions()

        return actions

    return wrapped


class StrategyTemplate:
    def __init__(self, strategy_config: StrategyConfig,
                 init_func: Callable[[any, datetime], any]=None,
                 instance_id:int=None):
        self.strategy_config = strategy_config
        self.logger_enabled = not self.strategy_config.diable_logging
        self.user_strategy_cls = load_strategy(file_path=strategy_config.strategy_file)
        self.user_strategy = self.user_strategy_cls() # user strategy init should take no args
        if init_func:
            self.user_strategy.__tq_init__ = init_func
        elif hasattr(self.user_strategy_cls, '__tq_init__'):
            self.user_strategy.__tq_init__ = self.user_strategy_cls.__tq_init__
        self.strategy_ctx = StrategyStateProxy(account_ids=strategy_config.account_ids, symbol_refs=strategy_config.symbol_info)
        self.tq: TokkaQuant = TokkaQuant(state_proxy=self.strategy_ctx, id_gen=zk_utils.zk_utils.create_id_gen(instance_id=instance_id))

        self.timer_cb = {}
        #self.execution_id = str(uuid.uuid4()) # todo: use a better id ?
        #self.strategy_event_subject = f"tq.strategy.{strategy_config.strategy_name}"

    def create_strategy(self, create_ts: datetime) -> list[SAction]:
        actions = []
        try:
            self.strategy_ctx.update_time(create_ts)
            self.user_strategy.on_create(tq=self.tq)
            actions.extend(self.tq._collect_pending_actions())
        except Exception as ex:
            tb.print_exc()
        finally:
            self.tq._clear_pending_actions()
        return actions

    def update_time(self, ts: Union[int, datetime]):
        if isinstance(ts, int) or isinstance(ts, np.int64):
            ts_dt = utils.from_unix_ts_to_datetime(ts)
        else:
            ts_dt = ts # type: datetime
        self.strategy_ctx.update_time(ts_dt)


    def init_strategy(self, custom_config, update_ts: datetime) -> list[SAction]:
        actions = []
        try:
            self.strategy_ctx.update_time(update_ts)
            self.user_strategy.on_reinit(custom_config, tq=self.tq)
            actions.extend(self.tq._collect_pending_actions())
        except Exception as ex:
            err_msg = tb.format_exc()
            logger.error("error in init strategy: " + err_msg)
            self.tq.set_error_state(err_msg)
        finally:
            self.tq._clear_pending_actions()
        return actions

    @handle_event
    def process_tick_event(self, tick: rtmd.TickData):
        self.update_time(tick.original_timestamp) # todo: use receive timestamp?
        self.user_strategy.on_tick(tick, self.tq)

    @handle_event
    def process_bar_event(self, bar: rtmd.Kline):
        self.update_time(bar.timestamp)
        self.user_strategy.on_bar(bar, self.tq)

    @handle_event
    def process_signal_event(self, sig: rtmd.RealtimeSignal):
        self.update_time(sig.timestamp)
        self.user_strategy.on_signal(sig, self.tq)

    @handle_event
    def process_order_update_event(self, order_update: oms.OrderUpdateEvent) -> list[SAction]:
        if self.logger_enabled:
            logger.info(f"order_update: {order_update.to_json()}")
        self.update_time(order_update.timestamp)
        self.strategy_ctx.process_orderupdate(order_update)
        self.user_strategy.on_orderupdate(order_update, self.tq)
        #return self.tq._collect_pending_actions()


    def process_timer_event(self, timer_event: TimerEvent) -> list[SAction]:
        tiemr_key = timer_event.timer_key
        self.strategy_ctx.update_time(timer_event.current_dt)
        cb, cb_kwargs = self.timer_cb.get(tiemr_key, None)
        actions = []
        if cb:
            kwargs = cb_kwargs if cb_kwargs else {}
            try:
                cb(timer_event, self.tq, **kwargs)
            except Exception as ex:
                tb.print_exc()
                msg = tb.format_exc()
                self.tq.set_error_state(msg)
            finally:
                actions.extend(self.tq._collect_pending_actions())
                self.tq._clear_pending_actions()
        else:
            # timer with no cb registered will be dispatched to on_scheduled_time()
            try:
                self.user_strategy.on_scheduled_time(timer_event, self.tq)
            except Exception as ex:
                err_msg = tb.format_exc()
                logger.error(err_msg)
                self.tq.set_error_state(err_msg)
            finally:
                actions.extend(self.tq._collect_pending_actions())
                self.tq._clear_pending_actions()

        return actions


    @handle_event
    def process_position_update_event(self, position_update: oms.PositionUpdateEvent) -> list[SAction]:
        if self.logger_enabled:
            logger.info(f"position_update: {position_update.to_json()}")
        self.update_time(position_update.timestamp)
        self.strategy_ctx.process_positionupdate(position_update)
        self.user_strategy.on_positionupdate(position_update, self.tq)

    @handle_event
    def process_strategy_cmd(self, cmd: strat_pb.StrategyCommand) -> list[SAction]:
        if self.logger_enabled:
            logger.info(f"strategy cmd: {cmd.to_json()}")
        if cmd.cmd_type == strat_pb.StrategyCommandType.STRAT_SPECIFIC_CMD:
            self.user_strategy.on_cmd(cmd.cmd_data, self.tq)

        # todo: handle different cmd types


    def register_timer(self, timer_key: str, cb: callable, cb_kwargs: dict = None):
        self.timer_cb[timer_key] = (cb, cb_kwargs)


    def is_in_error_state(self):
        return self.strategy_ctx.is_in_error_state

    def get_errors(self):
        return self.strategy_ctx.errors








