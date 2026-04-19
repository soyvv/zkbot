#import asyncio
import functools
import importlib
import numpy as np
from dataclasses import dataclass
from typing import Type, Union, Callable
import traceback as tb
#from zk_client.tqclient import TQClient
import zk_utils.zk_utils as tqutils

import zk_utils.zk_utils
import zk_proto_betterproto.oms as oms
import zk_proto_betterproto.tqrpc_oms as oms_rpc
import zk_proto_betterproto.common as common
import zk_proto_betterproto.rtmd as rtmd
import zk_proto_betterproto.strategy as strat_pb
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
    strategy_entrypoint: str = None  # production: "<installed.module.path>:<ClassName>"
    strategy_file: str = None  # dev/test only: path to a .py source file
    account_ids: list[int] = None # accounts to subscribe from OMS
    symbols: list[str] = None  # symbols to subscribe from RTMD
    symbols_kline: list[str] = None  # symbols to subscribe from RTMD-klines

    custom_params: dict = None # strategy specific config

    daily_start_time = None # none means no daily start
    daily_stop_time = None # none means no daily stop
    diable_logging = False # logging can be disabled for backtesting

    symbol_info: list[common.InstrumentRefData] = None


def load_strategy(entrypoint: str) -> Type[StrategyBase]:
    """Load a ``StrategyBase`` subclass from an installed module.

    ``entrypoint`` has the form ``"<module.path>:<ClassName>"`` and must refer
    to an importable, installed module. File-path loading lives in
    :mod:`zk_strategy.dev` and is dev/test only.
    """
    if not isinstance(entrypoint, str) or ":" not in entrypoint:
        raise ValueError(
            f"strategy entrypoint must be '<module>:<ClassName>', got: {entrypoint!r}"
        )
    module_path, class_name = entrypoint.split(":", 1)
    module_path = module_path.strip()
    class_name = class_name.strip()
    if not module_path or not class_name:
        raise ValueError(
            f"strategy entrypoint must be '<module>:<ClassName>', got: {entrypoint!r}"
        )

    module = importlib.import_module(module_path)
    try:
        strategy_cls = getattr(module, class_name)
    except AttributeError as exc:
        raise ImportError(
            f"class '{class_name}' not found in module '{module_path}'"
        ) from exc

    if not (isinstance(strategy_cls, type) and issubclass(strategy_cls, StrategyBase)):
        raise TypeError(
            f"{entrypoint} must resolve to a StrategyBase subclass"
        )

    init_func = getattr(module, "__tq_init__", None)
    if callable(init_func):
        strategy_cls.__tq_init__ = init_func
    return strategy_cls


def _resolve_strategy_cls(cfg: StrategyConfig) -> Type[StrategyBase]:
    """Choose the right loader based on which config field is populated.

    ``strategy_entrypoint`` (production: installed ``module:class``) takes
    precedence. ``strategy_file`` routes through :mod:`zk_strategy.dev`, the
    only sanctioned place for file-based loading.
    """
    if cfg.strategy_entrypoint:
        return load_strategy(cfg.strategy_entrypoint)
    if cfg.strategy_file:
        from . import dev  # imported lazily to keep dev-only code out of hot paths
        return dev.load_from_file(cfg.strategy_file)
    raise ValueError(
        "StrategyConfig must set either strategy_entrypoint (production)"
        " or strategy_file (dev/tests)"
    )


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
        self.user_strategy_cls = _resolve_strategy_cls(strategy_config)
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








