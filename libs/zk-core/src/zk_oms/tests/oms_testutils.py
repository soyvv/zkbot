import zk_utils.zk_utils
from zk_datamodel.ods import OMSConfigEntry
from zk_oms.core.confdata_mgr import ConfdataManager
from zk_oms.core.models import OMSRouteEntry, GWConfigEntry, InstrumentRefdata, InstrumentTradingConfig, OMSPosition
import zk_datamodel.common as common
from zk_oms.core.oms_core import OMSCore
import zk_datamodel.exch_gw as gw
import zk_datamodel.oms as oms
import datetime
from assertpy import assert_that
import zk_oms.core.models as oms_models

from copy import deepcopy

def get_test_account_routes() -> list[OMSRouteEntry]:
    account_routes = [
        OMSRouteEntry(
            accound_id=100,
            gw_key="GW1",
            exch_account_id="TEST1"
        ),
        OMSRouteEntry(
            accound_id=101,
            gw_key="GW2",
            exch_account_id="TEST2"
        ),
    ]
    return account_routes


def get_test_gw_config() -> list[GWConfigEntry]:
    gw_configs = [
        GWConfigEntry(
            rpc_endpoint="dummy1",
            report_endpoint="dummy2",
            balance_endpoint="dummy3",
            gw_key="GW1",
            exch_name="EX1",
            cancel_required_fields=["order_id"]
        ),
        GWConfigEntry(
            rpc_endpoint="dummy4",
            report_endpoint="dummy5",
            balance_endpoint="dummy6",
            gw_key="GW2",
            exch_name="EX2",
            cancel_required_fields=["order_id"]
        )
    ]
    return gw_configs


def get_test_symbols() -> list[InstrumentRefdata]:
    refdata = [
        InstrumentRefdata(
            instrument_id="ETH-P/USDC@EX1",
            instrument_code="ETH-P/USDC@EX1",
            instrument_id_exchange="ETH",
            exch_symbol="ETH",
            exch_name="EX1",
            instrument_type=common.InstrumentType.INST_TYPE_PERP,
            quote_symbol="USDC",
            base_symbol="ETH",
            price_precision=2,
            qty_precision=2
        ),
        InstrumentRefdata(
            instrument_id="ETH-P/USDC@EX2",
            instrument_code="ETH-P/USDC@EX2",
            instrument_id_exchange="ETH",
            exch_symbol="ETH",
            exch_name="EX2",
            instrument_type=common.InstrumentType.INST_TYPE_PERP,
            quote_symbol="USDC",
            base_symbol="ETH",
            price_precision=2,
            qty_precision=2
        ),
        InstrumentRefdata(
            instrument_id="BTC-P/USDC@EX1",
            instrument_code="BTC-P/USDC@EX1",
            instrument_id_exchange="BTC",
            exch_symbol="BTC",
            exch_name="EX1",
            instrument_type=common.InstrumentType.INST_TYPE_PERP,
            quote_symbol="USDC",
            base_symbol="BTC",
            price_precision=1,
            qty_precision=5
        ),
        InstrumentRefdata(
            instrument_id="BTC-P/USDC@EX2",
            instrument_code="BTC-P/USDC@EX2",
            exch_symbol="BTC",
            instrument_id_exchange="BTC",
            exch_name="EX2",
            instrument_type=common.InstrumentType.INST_TYPE_PERP,
            quote_symbol="USDC",
            base_symbol="BTC",
            price_precision=1,
            qty_precision=5
        ),
        InstrumentRefdata(
            instrument_id="ETH/USD@EX1",
            instrument_code="ETH/USD@EX2",
            instrument_id_exchange="ETH",
            exch_symbol="ETH",
            exch_name="EX2",
            instrument_type=common.InstrumentType.INST_TYPE_SPOT,
            quote_symbol="USD",
            base_symbol="ETH",
            price_precision=1,
            qty_precision=5
        ),
    ]
    return refdata


def get_test_trading_config() -> list[InstrumentTradingConfig]:
    trading_config = []
    return trading_config



def _create_init_positions(init_balances: dict[int, dict[str, float]]) -> list[OMSPosition]:
    positions = []
    for acc_id, balances in init_balances.items():
        for sym, qty in balances.items():
            is_perp = "-P" in sym
            instrument_type = common.InstrumentType.INST_TYPE_PERP if is_perp else common.InstrumentType.INST_TYPE_SPOT
            pos = OMSPosition.create_position(account_id=acc_id, symbol=sym, instrument_type=instrument_type)
            pos.position_state.avail_qty = qty
            pos.position_state.total_qty = qty
            positions.append(pos)
    return positions

def get_default_test_omscore(init_positions: dict[int, dict[str, float]]=None) -> OMSCore:

    account_routes = get_test_account_routes()
    gw_configs = get_test_gw_config()
    refdata = get_test_symbols()
    trading_config_table = [
        InstrumentTradingConfig(
            instrument_code=ref.instrument_code,
            bookkeeping_balance=False,
            balance_check=False
        )
        for ref in refdata
    ]

    confdata_mgr = ConfdataManager(oms_id="test_oms")
    confdata_mgr.reload_config(
        oms_config_entry=OMSConfigEntry(
            oms_id="test_oms",
            managed_account_ids=[100, 101],
            namespace="test"
        ),
        account_routes=account_routes,
        gw_configs=gw_configs,
        refdata=refdata,
        trading_configs=trading_config_table
    )

    oms_core = OMSCore(
        confdata_mgr=confdata_mgr,
        use_time_emulation=True,
        logger_enabled=True,
        risk_check_enabled=True
    )



    init_orders = []
    _positions = _create_init_positions(init_positions) if init_positions else []

    oms_core.init_state(curr_orders=init_orders, curr_balances=_positions)
    return oms_core

def get_nontq_test_omscore(init_positions: dict[int, dict[str, float]]=None) -> OMSCore:
    account_routes = get_test_account_routes()
    gw_configs = get_test_gw_config()
    refdata = get_test_symbols()
    trading_config_table = [
        InstrumentTradingConfig(
            instrument_code=ref.instrument_code,
            bookkeeping_balance=False,
            balance_check=False
        )
        for ref in refdata
    ]

    confdata_mgr = ConfdataManager(oms_id="test_oms")
    confdata_mgr.reload_config(
        oms_config_entry=OMSConfigEntry(
            oms_id="test_oms",
            managed_account_ids=[100, 101],
            namespace="test"
        ),
        account_routes=account_routes,
        gw_configs=gw_configs,
        refdata=refdata,
        trading_configs=trading_config_table
    )

    oms_core = OMSCore(
        confdata_mgr=confdata_mgr,
        use_time_emulation=True,
        handle_non_tq_order_reports=True,
        logger_enabled=True,
        risk_check_enabled=True
    )

    init_orders = []
    _positions = _create_init_positions(init_positions) if init_positions else []

    oms_core.init_state(curr_orders=init_orders, curr_balances=_positions)
    return oms_core

def get_spot_trading_omscore(init_positions: dict[int, dict[str, float]]=None) -> OMSCore:
    account_routes = get_test_account_routes()
    gw_configs = get_test_gw_config()
    refdata = get_test_symbols()
    trading_config_table = [
        InstrumentTradingConfig(
            instrument_code=ref.instrument_code,
            bookkeeping_balance=True,
            balance_check=True,
            publish_balance_on_book=True
        )
        for ref in refdata if ref.instrument_type == common.InstrumentType.INST_TYPE_SPOT
    ]

    confdata_mgr = ConfdataManager(oms_id="test_oms")
    confdata_mgr.reload_config(
        oms_config_entry=OMSConfigEntry(
            oms_id="test_oms",
            managed_account_ids=[100, 101],
            namespace="test"
        ),
        account_routes=account_routes,
        gw_configs=gw_configs,
        refdata=refdata,
        trading_configs=trading_config_table
    )

    oms_core = OMSCore(
        confdata_mgr=confdata_mgr,
        use_time_emulation=True,
        logger_enabled=True,
        risk_check_enabled=True
    )

    init_orders = []
    _positions = _create_init_positions(init_positions) if init_positions else []

    oms_core.init_state(curr_orders=init_orders, curr_balances=_positions)
    return oms_core

_id_gen = zk_utils.zk_utils.create_id_gen()

def generate_oms_order_request(
        account:int, symbol: str, side: str, qty: float, price: float,
        **kwargs):
    order_id = next(_id_gen)
    ts = _current_ts()
    extra_properties = common.ExtraData(
        data_map=kwargs if kwargs else {}
    )
    oms_order_request = oms.OrderRequest(
        order_id=order_id,
        account_id=account,
        instrument_code=symbol,
        buy_sell_type=common.BuySellType.BS_SELL if side == "sell" else common.BuySellType.BS_BUY,
        open_close_type=common.OpenCloseType.OC_OPEN,
        order_type=common.BasicOrderType.ORDERTYPE_LIMIT,
        price=price,
        qty=qty,
        timestamp=ts,
        source_id="TESTCASE",
        extra_properties=extra_properties
    )

    return oms_order_request


def assert_actions_has_type(actions: list[oms_models.OMSAction], action_type: oms_models.OMSActionType):
    assert_that(actions).extracting("action_type").contains(action_type)


def retrieve_action_by_type(actions: list[oms_models.OMSAction], action_type: oms_models.OMSActionType) -> oms_models.OMSAction:
    for a in actions:
        if a.action_type == action_type:
            return a
    return None

def retrieve_orderupdate_event(actions: list[oms_models.OMSAction]) -> oms.OrderUpdateEvent:
    for a in actions:
        if a.action_type == oms_models.OMSActionType.UPDATE_ORDER:
            oue: oms.OrderUpdateEvent = deepcopy(a.action_data)
            return oue

    assert_that(True).is_false(f"OrderUpdateEvent not found in actions: {actions}")


def retrieve_positionupdate_event(actions: list[oms_models.OMSAction]) -> oms.PositionUpdateEvent:
    for a in actions:
        if a.action_type == oms_models.OMSActionType.UPDATE_BALANCE:
            pue: oms.PositionUpdateEvent = deepcopy(a.action_data)
            return pue

    assert_that(True).is_false(f"PositionUpdateEvent not found in actions: {actions}")


def generate_oms_cancel_request(order_id: int, **kwargs):
    oms_cancel_request = oms.OrderCancelRequest(
        order_id=order_id)
    
    return oms_cancel_request


def _current_ts():
    return int(datetime.datetime.now().timestamp() * 1000)