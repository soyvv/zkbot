from dataclasses import dataclass
from enum import Enum

from datetime import datetime
from typing import Callable

import zk_proto_betterproto.common as common


# dataclass StrategyOrder (price, qty, symbol, account_id, order_id, side)
@dataclass
class StrategyOrder:
    price: float = None
    qty: float = None
    symbol: str = None
    account_id: int = None
    order_id: int = None
    side: common.BuySellType = None
    extra_params: dict = None

@dataclass
class TimerSubscription:
    timer_name: str = None
    scheduled_clock: datetime = None
    cron_expr: str = None
    cron_start_ts: datetime = None
    cron_end_ts: datetime = None
    cb: callable = None
    cb_extra_kwarg: dict[str, any] = None


@dataclass
class RPCRequest:
    request_id: str = None
    payload: any = None
    subject: str = None
    method: str = None
    timeout_in_secs: int = None
    retry_on_timeout: bool = False
    resp_cb: Callable[[any, bool], None] = None


class SLogLevel(Enum):
    ERROR = 1,
    WARNING = 2,
    INFO = 3,
    DEBUG = 4


@dataclass
class StrategyLog:
    ts_dt: datetime = None
    logline: str = None
    loglevel: SLogLevel = None


@dataclass
class StrategyCancel:
    order_id: int = None
    exch_order_ref: str = None
    account_id: int = None

@dataclass
class TimerEvent:
    timer_key: str = None
    current_dt: datetime = None



class SEventType(Enum):
    RTMD = 1,
    ORDER_UPDATE = 2,
    POSITION_UPDATE = 3,
    TIMER_EVENT = 4,
    RTSIG = 5


class SActionType(Enum):
    ORDER = 1,
    CANCEL = 2,
    LOG = 3,
    TIMER_SUB = 4,
    SIGNAL = 5,
    EXPORT = 6,
    NOTIFY = 7,
    RPC_REQUEST = 8, # other RPC request


class SEvent:
    def __init__(self, event_type: SEventType, event_data):
        pass


class SAction:
    def __init__(self, action_type: SActionType, action_data):
        self.action_type = action_type
        self.action_data = action_data

