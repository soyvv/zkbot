from dataclasses import dataclass, field
from enum import Enum
import zk_datamodel.tqrpc_exch_gw as gw_rpc
import zk_datamodel.tqrpc_oms as oms_rpc
import zk_datamodel.oms as oms
import zk_datamodel.common as common

class SymbolType(Enum):
    FIAT = 1
    VALUE_COIN = 2
    COIN = 3



class OMSFund:
    pass



from zk_datamodel.common import InstrumentRefData as InstrumentRefdata
from zk_datamodel.ods import GWConfigEntry as GWConfigEntry

@dataclass
class InstrumentTradingConfig:
    instrument_code: str
    bookkeeping_balance: bool = False  # balance will be managed by OMS
    balance_check: bool = False
    publish_balance_on_trade: bool = True
    publish_balance_on_book: bool = False
    publish_balance_on_cancel: bool = False
    use_margin: bool = False
    margin_ratio: float = 1.0
    max_order_size: float = None

@dataclass
class OMSRouteEntry:
    accound_id: int  # TODO: fix typo
    exch_account_id: str
    gw_key: str
    account_id: int = None
    startup_sync: bool = True
    desc: str = None

    def __post_init__(self):
        self.account_id = self.accound_id

# @dataclass
# class GWConfigEntry:
#     exch_name: str
#     gw_key: str
#     rpc_endpoint: str
#     report_endpoint: str
#     balance_endpoint: str = None
#     gw_system_endpoint: str = None # for system level events
#     support_order_replace: bool = False
#     support_order_query: bool = True
#     support_position_query: bool = True
#     support_batch_order: bool = True
#     support_batch_cancel: bool = True
#     calc_balance_needed: bool = False
#     cancel_required_fields: list[str] = field(default_factory=list)



@dataclass
class OMSPosition:
    account_id: int
    symbol: str
    symbol_exch: str = None
    is_short: bool = False
    position_state: oms.Position = None

    @staticmethod
    def create_position(account_id: int, symbol: str, instrument_type:common.InstrumentType, is_short=False, is_from_exch=False):
        pos = OMSPosition(account_id=account_id, symbol=symbol)
        pos_state = oms.Position()
        pos_state.account_id = account_id

        pos_state.instrument_code = symbol
        pos_state.instrument_type = instrument_type
        pos_state.long_short_type = common.LongShortType.LS_SHORT if is_short else common.LongShortType.LS_LONG
        pos_state.avail_qty = 0
        pos_state.frozen_qty = 0
        pos_state.total_qty = 0
        pos_state.is_from_exch = is_from_exch

        pos.position_state = pos_state
        pos.is_short = is_short

        return pos

@dataclass
class PositionChange:
    account_id: int
    symbol: str
    position: OMSPosition = None
    is_short: bool = False
    avail_change: float = .0
    frozen_change: float = .0
    total_change: float = .0

@dataclass
class OMSOrder:
    is_from_external: bool = False
    account_id: int = None
    order_id: int = None
    exch_order_ref: str = None
    oms_req: oms.OrderRequest = None  # received order request
    gw_req: gw_rpc.ExchSendOrderRequest = None # generated gw request
    cancel_req: gw_rpc.ExchCancelOrderRequest = None # generated gw cancel request
    oms_order_state: oms.Order = None # states to be published
    trades: list[oms.Trade] = field(default_factory=list)
    acc_trades_filled_qty: float = .0
    acc_trades_value: float = .0
    order_inferred_trades: list[oms.Trade] = field(default_factory=list)
    exec_msgs: list[oms.ExecMessage] = field(default_factory=list)
    fees: list[oms.Fee] = field(default_factory=list)
    cancel_attempts: int = 0


@dataclass
class ExchOrderRef:
    gw_key: str = None
    account_id: int = None
    exch_order_id: str = None
    order_id: int = None
    symbol_exch: str = None # some exchange requires symbol for query

@dataclass
class OrderRecheckRequest:
    order_id: int = None
    check_delay_in_sec: int = 5
    timestamp: int = None

@dataclass
class CancelRecheckRequest:
    orig_request: gw_rpc.ExchCancelOrderRequest = None
    check_delay_in_sec: int = 5
    retry: bool = False
    timestamp: int = None

@dataclass
class OrderContext:
    account_id: int = None
    fund: OMSPosition = None
    position: OMSPosition = None
    route: OMSRouteEntry = None
    trading_config: InstrumentTradingConfig = None
    symbol_ref: InstrumentRefdata = None
    gw_config: GWConfigEntry = None
    order: OMSOrder = None
    errors: list[str] = field(default_factory=list)

    def has_error(self):
        return len(self.errors) > 0

@dataclass
class RefdataContext:
    instrument_refdata: InstrumentRefdata = None
    account_route: OMSRouteEntry = None
    gw_config: GWConfigEntry = None
    trading_config: InstrumentTradingConfig = None


@dataclass
class ValidateResult:
    is_valid: bool = True
    error_msg: str = None


class SchedRequestType(Enum):
    CLEANUP = 1
    RELOAD_CONFIG = 2

@dataclass
class OMSCoreSchedRequest:
    ts_ms: int = None
    request_type: SchedRequestType = None


class OMSActionType(Enum):
    UPDATE_BALANCE = 1
    SEND_ORDER_TO_GW = 2
    SEND_CANCEL_TO_GW = 3
    UPDATE_ORDER = 4
    PERSIST_ORDER = 5

    RECHECK_ORDER = 6 # order status is uncertain; need to recheck after some time
    RECHECK_CANCEL = 7
    ORDER_SYNC = 8 # order status is uncertain; need to sync with GWs

    BATCH_SEND_ORDER_TO_GW = 9
    BATCH_SEND_CANCEL_TO_GW = 10

    PUBLISH_ERROR = 11

@dataclass
class OMSAction:
    action_type: OMSActionType
    action_meta: dict[str, any]
    action_data: any




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

@dataclass
class TradeExtraData:
    account_id: int = None
    account_name: str = None
    exch: str = None
    instrument_mo: str = None
    instrument_exch: str = None
    instrument_type: str = None
    source_id: str = None
    timestamp: int = None
    datetime: str = None
    record_datetime: str = None
