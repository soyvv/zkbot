import zk_proto_betterproto.oms as oms
import zk_proto_betterproto.common as common
#from zk_client.tqclient import TQClient
from .models import SAction, TimerEvent, SActionType, StrategyOrder, StrategyCancel, StrategyLog, TimerSubscription
import logging
from datetime import datetime
import zk_utils.zk_utils as tqutils
from typing import Union

from loguru import logger


class AccountStateProxy:
    def __init__(self, account_id: int):
        self.account_id: int = account_id
        self.pending_orders: dict[int, StrategyOrder] = {}
        self.pending_cancels = set()
        self.open_orders: dict[int, oms.Order] = {}
        self.terminal_orders: dict[int, oms.Order] = {}

        self.balances: dict[str, oms.Position] = {}


    def init_balances(self, init_balances: list[oms.Position]):
        logger.info(f"init balacne/positions for {self.account_id}: {init_balances}")
        for b in init_balances:
            assert b.account_id == self.account_id
            self.balances[b.instrument_code] = b


    def init_orders(self, init_orders: list[oms.Order]):
        logger.info(f"init orders for {self.account_id}: {init_orders}")
        for o in init_orders:
            if self._is_in_terminal_state(o):
                self.terminal_orders[o.order_id] = o
            else:
                self.open_orders[o.order_id] = o


    def book_order(self, order: StrategyOrder):
        self.pending_orders[order.order_id] = order

    def book_cancel(self, cancel: StrategyCancel):
        self.pending_cancels.add(cancel.order_id)

    def process_orderupdate(self, ou: oms.OrderUpdateEvent):
        #logger.debug(f"order update received: {ou.to_json()}")
        o: oms.Order = ou.order_snapshot
        if o.order_id not in self.open_orders and o.order_id not in self.pending_orders:
            logger.error("received order update for order not found")
            # todo: check terminal orders also
            return

        # if there is cancel rejection, then remove the order from pending cancels
        if ou.exec_message.exec_type == oms.ExecType.EXEC_TYPE_CANCEL \
            and not ou.exec_message.exec_success:
            if o.order_id in self.pending_cancels:
                self.pending_cancels.remove(o.order_id)

        if self._is_in_terminal_state(o):
            self.terminal_orders[o.order_id] = o
            if o.order_id in self.open_orders:
                self.open_orders.pop(o.order_id)
            if o.order_id in self.pending_orders:
                self.pending_orders.pop(o.order_id)
            if o.order_id in self.pending_cancels:
                self.pending_cancels.remove(o.order_id)
        else:
            self.open_orders[o.order_id] = o
            if o.order_id in self.pending_orders:
                self.pending_orders.pop(o.order_id)

    def process_positionupdate(self, pu: oms.Position):
        self.balances[pu.instrument_code] = pu

    def get_account_balance(self):
        return self.balances

    def get_open_order(self) -> list[oms.Order]:
        return self.open_orders.values()


    def get_pending_order(self) -> list[StrategyOrder]:
        return self.pending_orders.values()


    def get_pending_cancels(self) -> set[int]:
        return self.pending_cancels

    def _is_in_terminal_state(self, order: oms.Order):
        return order.order_status in (
            oms.OrderStatus.ORDER_STATUS_REJECTED,
            oms.OrderStatus.ORDER_STATUS_CANCELLED,
            oms.OrderStatus.ORDER_STATUS_FILLED
        )

class StrategyStateProxy:
    def __init__(self, account_ids: list[int],
                 symbol_refs: list[common.InstrumentRefData],
                 init_orders:list[oms.Order] = None,
                 init_balances: dict[str, oms.Position] = None):
        self.account_states: dict[int, AccountStateProxy] = {}
        self.order_id_idx: dict[int, AccountStateProxy] = {}
        self.current_ts = None
        self.errors = []
        try:
            from zk_client import TQClient
            self._tq_client: TQClient = None #  not to be used directly
        except Exception as e:
            logger.error(f"zk_client not found: {e}")
        self.is_in_error_state = False
        self.symbol_refs: dict[str, common.InstrumentRefData] = {s.instrument_id: s for s in symbol_refs} if symbol_refs else {}
        for account_id in account_ids:
            # _init_orders = [o for o in init_orders if o.account_id == account_id] if init_orders else []
            # _init_balances = {k: v for k, v in init_balances.items() if v.account_id == account_id} if init_balances else {}
            account_proxy = AccountStateProxy(account_id)
            # account_proxy.init_orders(_init_orders)
            # account_proxy.init_balances(_init_balances)
            self.account_states[account_id] = account_proxy


    def append_error(self, err_msg:str):
        if len(self.errors) > 10:
            del self.errors[0] # make sure error messages do not cause OOM
        self.errors.append(err_msg)
        self.is_in_error_state = True

    def init_orders_and_balances(self, init_orders: list[oms.Order], init_balances: list[oms.Position]):
        for account_id, account_proxy in self.account_states.items():
            _init_orders = [o for o in init_orders if o.account_id == account_id]
            for o in _init_orders:
                self.order_id_idx[o.order_id] = account_proxy
            _init_balances = [b for b in init_balances if b.account_id == account_id]
            account_proxy.init_orders(_init_orders)
            account_proxy.init_balances(_init_balances)



    def book_order(self, order: StrategyOrder):
        self._check_account_id(order.account_id)
        if order.order_id in self.order_id_idx:
            raise ValueError(f"order id {order.order_id} already exists")

        order_id = order.order_id
        account_state = self.account_states.get(order.account_id)
        account_state.book_order(order)

        self.order_id_idx[order_id] = account_state


    def book_cancel(self, cancel: StrategyCancel):
        self._check_order_id_exists(cancel.order_id)
        account_state = self.order_id_idx.get(cancel.order_id)
        account_state.book_cancel(cancel)

    def process_orderupdate(self, order_update_event: oms.OrderUpdateEvent):
        order_id = int(order_update_event.order_id)
        if not order_id or order_id not in self.order_id_idx:
            # cannot find matech account state;
            # ignore this order update
            logger.warning(f"unknown order update order id {order_id} received")
            return
        #self._check_order_id_exists(order_id)
        account_state = self.order_id_idx.get(order_id)
        account_state.process_orderupdate(order_update_event)

        self._update_ts(order_update_event.timestamp)

    def process_positionupdate(self, position_update_event: oms.PositionUpdateEvent):
        for p in position_update_event.position_snapshots:
            account_state = self.account_states.get(p.account_id)
            account_state.process_positionupdate(p)


    def update_time(self, ts: datetime):
        if self.current_ts is None or ts > self.current_ts:
            self.current_ts = ts

    def _update_ts(self, ts: Union[int, datetime]):
        if isinstance(ts, int):
            dt = tqutils.from_unix_ts_to_datetime(ts)
            self.update_time(dt)
        elif isinstance(ts, datetime):
            self.update_time(ts)

    def get_symbol_info(self, symbol: str) -> common.InstrumentRefData:
        if symbol in self.symbol_refs:
            return self.symbol_refs[symbol]
        return None

    def get_account_balance(self, account_id):
        self._check_account_id(account_id)
        account_state = self.account_states.get(account_id)
        return account_state.get_account_balance()

    def get_open_orders(self, account_id) -> list[oms.Order]:
        self._check_account_id(account_id)
        account_state = self.account_states.get(account_id)
        return account_state.get_open_order()


    def get_pending_orders(self, account_id) -> list[StrategyOrder]:
        self._check_account_id(account_id)
        account_state = self.account_states.get(account_id)
        return account_state.get_pending_order()


    def get_pending_cancels(self, account_id) -> set[int]:
        self._check_account_id(account_id)
        account_state = self.account_states.get(account_id)
        return account_state.get_pending_cancels()

    def get_order(self, order_id)-> Union[StrategyOrder, oms.Order]:
        self._check_order_id_exists(order_id)
        account_state = self.order_id_idx.get(order_id)
        order = account_state.open_orders.get(order_id, None)
        if not order:
            order = account_state.terminal_orders.get(order_id, None)
        if not order:
            order = account_state.pending_orders.get(order_id, None)
        return order

    def get_order_account_id(self, order_id) -> int:
        self._check_order_id_exists(order_id)
        account_state = self.order_id_idx.get(order_id)
        if account_state:
            return account_state.account_id
        else:
            return None

    def _check_account_id(self, account_id):
        if not account_id or account_id not in self.account_states:
            raise ValueError(f"account {account_id} not found/configured")

    def _check_order_id_exists(self, order_id):
        if not order_id or order_id not in self.order_id_idx:
            logger.debug(f"order id idx: {self.order_id_idx}")
            raise ValueError(f"order id {order_id}/{type(order_id)} not found")
