import uuid
from typing import Union, Callable

import zk_proto_betterproto.oms as oms
import zk_proto_betterproto.rtmd as rtmd
import zk_proto_betterproto.strategy as strategy_pb 
import zk_utils.zk_utils as tqutils
from .models import *
import logging

logger = logging.getLogger(__name__)

from .strategy_stateproxy import StrategyStateProxy


class TokkaQuant:
    def __init__(self, state_proxy: StrategyStateProxy,
                 id_gen=None) -> None:
        self.state_proxy = state_proxy
        self._account_ids = list(state_proxy.account_states.keys())
        self._pending_actions = []
        self._id_gen = id_gen if id_gen else tqutils.create_id_gen()
        self.__tq_init_output__ = None
        self.__last_export_output__ = None

    def set_error_state(self, error_msg:str):
        self.state_proxy.append_error(error_msg)

    def get_custom_init_data(self) -> any:
        return self.__tq_init_output__

    def get_last_export_output(self) -> any:
        return self.__last_export_output__


    def account_ids(self) -> list[int]:
        return self._account_ids

    def set_timer_cron(self, timer_name, cron_expression=None, start_ts=None, end_ts=None, cb=None, **kwargs_cb):
        timersub = TimerSubscription()
        timersub.timer_name=timer_name
        timersub.cron_expr = cron_expression
        timersub.cron_start_ts = start_ts
        timersub.cron_end_ts = end_ts
        timersub.cb = cb
        timersub.cb_extra_kwarg=kwargs_cb
        action = SAction(action_type=SActionType.TIMER_SUB, action_data=timersub)
        self._pending_actions.append(action)

    def set_timer_clock(self, timer_name, date_time: datetime, cb=None, **kwargs_cb):
        timersub = TimerSubscription()
        timersub.timer_name = timer_name
        timersub.scheduled_clock = date_time
        timersub.cb = cb
        timersub.cb_extra_kwarg = kwargs_cb
        action = SAction(action_type=SActionType.TIMER_SUB, action_data=timersub)
        self._pending_actions.append(action)

    def buy(self, account_id: int, symbol: str, qty: float, price: float, **kwargs) -> int:
        return self._new_order(account_id=account_id,
                               symbol=symbol,
                               qty=qty, price=price,
                               side=common.BuySellType.BS_BUY, **kwargs)

    def sell(self, account_id: int, symbol: str, qty: float, price: float, **kwargs) -> int:
        return self._new_order(account_id=account_id,
                               symbol=symbol,
                               qty=qty, price=price,
                               side=common.BuySellType.BS_SELL, **kwargs)

    def cancel_order(self, order_id, order_ref=None, **kwargs):
        account_id = self.state_proxy.get_order_account_id(order_id=order_id)
        if account_id is None:
            raise ValueError(f"Order {order_id} not found when cancelling")
        cancel = StrategyCancel(order_id=order_id, exch_order_ref=order_ref, account_id=account_id)
        action = SAction(action_type=SActionType.CANCEL, action_data=cancel)
        self._pending_actions.append(action)
        self.state_proxy.book_cancel(cancel)

    def amend_order(self, order_id, order_ref):
        pass

    def log(self, logline: str, loglevel=None):
        #loglevel = SLogLevel.value if isinstance(loglevel, str)
        loglevel = SLogLevel[loglevel] if isinstance(loglevel, str) else loglevel
        log = StrategyLog(ts_dt=self.get_current_ts(),
                          logline=logline,
                          loglevel=loglevel if loglevel else SLogLevel.INFO)

        action = SAction(action_type=SActionType.LOG, action_data=log)
        self._pending_actions.append(action)

    def send_signal(self, symbol: str,  data_dict: dict[str, any], message:str=None, namespace:str=None):

        strategy_signal = rtmd.RealtimeSignal()
        strategy_signal.instrument = symbol
        if namespace:
            strategy_signal.namespace = namespace
        strategy_signal.timestamp = self.get_current_ts(convert_to_epoch_ts=True)
        _float_data_dict = {k: v for k, v in data_dict.items() if isinstance(v, float)}
        _int_data_dict = {k: v for k, v in data_dict.items() if isinstance(v, int)}
        _str_data_dict = {k: v for k, v in data_dict.items() if isinstance(v, str)}
        strategy_signal.data = _float_data_dict
        strategy_signal.data_int = _int_data_dict
        strategy_signal.data_str = _float_data_dict

        strategy_signal.message = message

        action = SAction(action_type=SActionType.SIGNAL, action_data=strategy_signal)
        self._pending_actions.append(action)

    def send_request(self, payload: any, subject: str, method: str, timeout_in_secs=5, retry_on_timeout=True,
                     resp_cb: Callable[[any, bool], None] = None):
        rpc_request = RPCRequest(
            request_id=str(uuid.uuid4()),
            payload=payload,
            subject=subject,
            method=method,
            timeout_in_secs=timeout_in_secs,
            retry_on_timeout=retry_on_timeout,
            resp_cb=resp_cb
        )
        self._pending_actions.append(SAction(action_type=SActionType.RPC_REQUEST, action_data=rpc_request))


    def send_notification(self, notification: str, meta_data: dict[str,str]):
        strat_notification = strategy_pb.StrategyNotification()
        strat_notification.message = notification
        strat_notification.message_meta_info = meta_data
        strat_notification.timestamp = self.get_current_ts(convert_to_epoch_ts=True)

        action = SAction(action_type=SActionType.NOTIFY, action_data=strat_notification)
        self._pending_actions.append(action)
        

    def get_current_ts(self, convert_to_epoch_ts: bool = False) -> Union[int, datetime]:
        if convert_to_epoch_ts:
            return tqutils.from_datetime_to_unix_ts(self.state_proxy.current_ts)
        return self.state_proxy.current_ts

    def get_account_balance(self, account_id: int = None) -> dict[str, oms.Position]:
        return self.state_proxy.get_account_balance(account_id=account_id)

    def get_position(self, account_id: int, instrument_code: str):
        positions = self.get_account_balance(account_id)
        return positions.get(instrument_code, None)

    def get_order(self, order_id: int, exch_order_ref: str = None) \
            -> Union[StrategyOrder, oms.Order]:
        order = self.state_proxy.get_order(order_id)
        return order

    def get_open_orders(self, account_id: int) -> list[oms.Order]:
        return self.state_proxy.get_open_orders(account_id)

    def get_pending_orders(self, account_id: int):
        return self.state_proxy.get_pending_orders(account_id)

    def get_pending_cancels(self, account_id: int):
        return self.state_proxy.get_pending_cancels(account_id)


    # def cron_helper_every_seconds(self, seconds: int, cb, **kwargs_cb):
    #     return self.set_timer_cron(timer_name=f"every_{seconds}_seconds",
    #                                cron_expression=f"*/{seconds} * * * * *",
    #                                cb=cb, **kwargs_cb)

    def get_symbol_info(self, symbol: str) -> common.InstrumentRefData:
        #return self.state_proxy.get_symbol_info(symbol)
        return self.state_proxy.get_symbol_info(symbol)


    def get_all_symbol_info(self) -> dict[str, common.InstrumentRefData]:
        return self.state_proxy.symbol_refs

    def _collect_pending_actions(self):
        return self._pending_actions

    def _clear_pending_actions(self):
        self._pending_actions = []

    def _new_order(self, account_id: int, symbol: str, qty: float, price: float, side: common.BuySellType,  **kwargs) -> int:
        so = StrategyOrder()
        so.symbol = symbol
        so.order_id = self._gen_order_id()
        so.account_id = account_id
        so.qty = qty
        so.price = price
        so.side = side
        so.extra_params = kwargs

        self.state_proxy.book_order(so)

        action = SAction(action_type=SActionType.ORDER, action_data=so)
        self._pending_actions.append(action)

        return so.order_id

    def _gen_order_id(self):
        return next(self._id_gen)