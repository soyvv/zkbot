import datetime
import threading
import time
from typing import Callable, Optional
from dataclasses import dataclass

from tq_service_oms.config import DBConfigLoader
try:
    from .default_match_policy import DefaultMatchPolicy
except ImportError:
    from default_match_policy import DefaultMatchPolicy
from zk_simulator.sim_core import *
import tqrpc_utils.rpc_utils as svc_utils

import pymongo

import datetime
from loguru import logger
from pydantic import BaseModel

@dataclass
class SimGwAppConfig:
    nats_url: str = None
    mongo_uri: str = None
    mongo_dbname: str = None
    gw_key: str = None
    # service_namespace: str = None
    # service_name: str = None
    # exch_account_id: str = None
    # rpc_endpoint: str = None
    # report_endpoint: str = None
    # rtmd_endpoint: str = None
    # init_balances_json: str = None


class SimGwConfig(BaseModel):
    gw_key: str
    exch_account_id: str
    rtmd_channels: list[str]
    init_balances: list[tuple[str, float]]
    fund_asset: str
    target_exchange: str
    # match_policy_cls: str # class name
    # match_policy_config: dict

@dataclass
class EnrichedSimConfig:
    nats_url: str = None
    gw_key: str = None
    exch_account_id: str = None
    rtmd_channels: list[str] = None
    init_balances: list[tuple[str, float]] = None
    fund_asset: str = None
    target_exchange: str = None # which exchange to impersonate
    refdata: list[common.InstrumentRefData] = None # refdata for all symbols
    # match_policy_cls: str # class name
    # match_policy_config: dict


class SimGwBalanceMgr:
    def __init__(self, init_balances: list[tuple[str, float]],
                 fund_asset: str,
                 exch_account_id: str,
                 refdata: list[common.InstrumentRefData]):

        self.fund_asset = fund_asset
        self.exch_account_id = exch_account_id
        self.refdata_dict: dict[str, common.InstrumentRefData] = \
            {rd.instrument_id_exchange: rd for rd in refdata}
        self.balance_reports: dict[str, gw.PositionReport] = {}
        for symbol, qty in init_balances:
            if symbol == fund_asset:
                p = gw.PositionReport()
                p.qty = qty
                p.avail_qty = qty
                p.exch_account_code = exch_account_id
                p.update_timestamp = int(datetime.datetime.now().timestamp() * 1000)
                p.instrument_code = symbol
                p.instrument_type = common.InstrumentType.INST_TYPE_SPOT
                p.long_short_type = common.LongShortType.LS_LONG
                p.message_raw = ""
                self.balance_reports[symbol] = p
            else:
                refdata = self.refdata_dict.get(symbol, None)
                p = gw.PositionReport()
                p.qty = abs(qty)
                p.avail_qty = abs(qty)
                p.exch_account_code = exch_account_id
                p.update_timestamp = int(datetime.datetime.now().timestamp() * 1000)
                if refdata and refdata.instrument_type != common.InstrumentType.INST_TYPE_SPOT:
                    p.instrument_code = refdata.instrument_id_exchange
                    p.instrument_type = refdata.instrument_type
                    p.long_short_type = common.LongShortType.LS_LONG if qty > 0 else common.LongShortType.LS_SHORT
                else:
                    p.instrument_code = symbol
                    p.instrument_type = common.InstrumentType.INST_TYPE_SPOT
                    p.long_short_type = common.LongShortType.LS_LONG
                p.message_raw = ""
                self.balance_reports[symbol] = p

    def update_balance(self, gw_reports: list[gw.OrderReport]) \
            -> Optional[gw.BalanceUpdate]:
        changed_assets = set()
        for gw_report in gw_reports:

            for rep_entry in gw_report.order_report_entries:
                if rep_entry.report_type == gw.OrderReportType.ORDER_REP_TYPE_TRADE:
                    trade = rep_entry.trade_report
                    symbol = trade.order_info.instrument
                    buy_sell = trade.order_info.buy_sell_type
                    refdata = self.refdata_dict.get(symbol)
                    if not refdata:
                        logger.warning(f"refdata not found for symbol {symbol}")
                        continue
                    price = trade .filled_price
                    qty = trade.filled_qty
                    amount = price * qty
                    if refdata.instrument_type != common.InstrumentType.INST_TYPE_SPOT:
                        asset_report = self.get_or_create_position(symbol, refdata.instrument_type)
                    else:
                        base_asset = refdata.base_asset
                        asset_report = self.get_or_create_position(base_asset, common.InstrumentType.INST_TYPE_SPOT)
                    changed_assets.add(asset_report.instrument_code)
                    changed_assets.add(self.fund_asset)
                    fund_report = self.balance_reports.get(self.fund_asset)
                    if buy_sell == common.BuySellType.BS_BUY:
                        fund_report.qty -= amount
                        fund_report.avail_qty -= amount
                        if asset_report.long_short_type == common.LongShortType.LS_LONG:
                            asset_report.qty += qty
                            asset_report.avail_qty += qty
                        else:
                            asset_report.qty -= qty
                            asset_report.avail_qty -= qty
                    else:
                        fund_report.qty += amount
                        fund_report.avail_qty += amount
                        if asset_report.long_short_type == common.LongShortType.LS_LONG:
                            asset_report.qty -= qty
                            asset_report.avail_qty -= qty
                        else:
                            asset_report.qty += qty
                            asset_report.avail_qty += qty
                    self._normalize_position(asset_report)

        if len(changed_assets) > 0:
            balance_update = gw.BalanceUpdate()
            balance_update.balances = [self.balance_reports[s] for s in changed_assets]
            return balance_update
        return None

    def _normalize_position(self, pos_report: gw.PositionReport):
        if pos_report.qty < 0 and pos_report.long_short_type == common.LongShortType.LS_LONG:
            pos_report.qty = abs(pos_report.qty)
            pos_report.avail_qty = abs(pos_report.avail_qty)
            pos_report.long_short_type = common.LongShortType.LS_SHORT
        elif pos_report.qty < 0 and pos_report.long_short_type == common.LongShortType.LS_SHORT:
            pos_report.qty = abs(pos_report.qty)
            pos_report.avail_qty = abs(pos_report.avail_qty)
            pos_report.long_short_type = common.LongShortType.LS_LONG

    def get_current_balances(self) -> list[gw.PositionReport]:
        return list(self.balance_reports.values())

    def get_or_create_position(self, asset_name:str,
                               asset_type=common.InstrumentType.INST_TYPE_SPOT) -> gw.PositionReport:
        if asset_name not in self.balance_reports:
            pos_report = gw.PositionReport()
            pos_report.instrument_code = asset_name
            pos_report.exch_account_code = self.exch_account_id
            pos_report.instrument_type = asset_type
            pos_report.qty = .0
            pos_report.avail_qty = .0
            pos_report.long_short_type = common.LongShortType.LS_LONG
            pos_report.update_timestamp = int(time.time() * 1000)
            self.balance_reports[asset_name] = pos_report

        return self.balance_reports[asset_name]

class SimGwService:
    def __init__(self, config: EnrichedSimConfig):
        self.config = config
        match_policy = DefaultMatchPolicy()
        self.exch_account_id = config.exch_account_id
        #self.init_balances = config.init_balances # sym -> balance
        self.fund_asset = config.fund_asset
        self.sim_core = SimulatorCore(match_policy=match_policy)

        self.balance_mgr = SimGwBalanceMgr(init_balances=config.init_balances,
                                           fund_asset=self.fund_asset,
                                           exch_account_id=self.exch_account_id,
                                           refdata=config.refdata)

        self.report_publisher = None

        self.tick_received = set()

        self._sim_lock = threading.RLock()

    def place_order(self, order_req: gw_rpc.ExchSendOrderRequest) -> gw_rpc.GatewayResponse:
        logger.debug(f"receiving order request: {order_req.to_json()}")
        current_ts_millis = int(datetime.datetime.now().timestamp() * 1000)
        with self._sim_lock:
            sim_results = self.sim_core.on_new_order(order_req, ts=current_ts_millis)

        self._handle_sim_results(sim_results)
        gw_resp = gw_rpc.GatewayResponse()
        gw_resp.status = gw_rpc.GatewayResponseStatus.GW_RESP_STATUS_SUCCESS
        gw_resp.timestamp = current_ts_millis
        return gw_resp


    def batch_place_orders(self, batch_order_req: gw_rpc.ExchBatchSendOrdersRequest) -> gw_rpc.GatewayResponse:
        logger.debug(f"receiving batch order request: {batch_order_req.to_json()}")
        gw_resp = self._gen_default_gw_response()
        for req in batch_order_req.order_requests:
            self.place_order(req)
        return gw_resp

    def cancel_order(self, cancel_req: gw_rpc.ExchCancelOrderRequest) -> gw_rpc.GatewayResponse:
        logger.debug(f"receiving cancel request: {cancel_req.to_json()}")
        current_ts_millis = int(datetime.datetime.now().timestamp() * 1000)
        with self._sim_lock:
            sim_results = self.sim_core.on_cancel_order(cancel_req, ts=current_ts_millis)

        self._handle_sim_results(sim_results)
        gw_resp = gw_rpc.GatewayResponse()
        gw_resp.status = gw_rpc.GatewayResponseStatus.GW_RESP_STATUS_SUCCESS
        gw_resp.timestamp = current_ts_millis
        return gw_resp

    def batch_cancel_orders(self, batch_cancel_req: gw_rpc.ExchBatchCancelOrdersRequest) -> gw_rpc.GatewayResponse:
        logger.debug(f"receiving batch cancel request: {batch_cancel_req.to_json()}")
        gw_resp = self._gen_default_gw_response()
        for req in batch_cancel_req.cancel_requests:
            self.cancel_order(req)
        return gw_resp

    def query_account_balance(self, query_req: gw_rpc.ExchQueryAccountRequest) -> gw_rpc.ExchAccountResponse:
        logger.debug(f"receiving balance query request: {query_req.to_json()}")
        resp = gw_rpc.ExchAccountResponse()
        resp.timestamp = int(datetime.datetime.now().timestamp() * 1000)

        balance_update = gw.BalanceUpdate()
        balance_update.balances = self.balance_mgr.get_current_balances()
        resp.balance_update = balance_update
        resp.exch_account_code = self.exch_account_id
        return resp


    def query_order_details(self, query_req: gw_rpc.ExchQueryOrderDetailRequest) \
        -> gw_rpc.ExchOrderDetailResponse:
        logger.debug(f"receiving order detail query request: {query_req.to_json()}")
        resp = gw_rpc.ExchOrderDetailResponse()
        resp .orders = []

        for order_ref in query_req .order_queries:
            order_id = order_ref.order_id
            sime_order: SimOrder = self.sim_core.order_cache.get(order_id)
            if sime_order:
                sim_res = self.sim_core._update_and_gen_reports(
                    order=sime_order,
                    match_res=None,
                )
                sim_res.order_report.exchange = self.config.gw_key.lower()
                exch_order = gw_rpc.ExchOrder()
                exch_order .order_report = sim_res.order_report
                resp.orders.append(exch_order)
        return resp


    def on_tick(self, tick: rtmd.TickData):
        if tick.instrument_code not in self.tick_received:
            logger.info(f"new tick received: tick={tick.to_json()}")
            self.tick_received.add(tick.instrument_code)
        if tick.tick_type == rtmd.TickUpdateType.OB:
            # logger.debug(f"tick received: s={tick.instrument_code}, b={tick.buy_price_levels[0]}, a={tick.sell_price_levels[0]}")
            with self._sim_lock:
                results: list[SimResult] = self.sim_core.on_tick(tick)
            self._handle_sim_results(results)

    def _handle_sim_results(self, results: list[SimResult]):
        if self.report_publisher:
            for res in results:
                # ignore delay
                logger.debug(f"publishing order report: {res.order_report.to_json()}")
                self.report_publisher(res.order_report)
            balance_update = self.balance_mgr.update_balance([res.order_report for res in results])
            if balance_update:
                logger.debug(f"publishing balance update: {balance_update.to_json()}")
                self.balance_publisher(balance_update)

    def register_report_publisher(self, report_publisher: Callable[[gw.OrderReport], None]):
        self.report_publisher = report_publisher

    def register_balance_publisher(self, balance_publisher: Callable[[gw.BalanceUpdate], None]):
        self.balance_publisher = balance_publisher

    def _gen_default_gw_response(self) -> gw_rpc.GatewayResponse:
        gw_resp = gw_rpc.GatewayResponse()
        gw_resp.status = gw_rpc.GatewayResponseStatus.GW_RESP_STATUS_SUCCESS
        gw_resp.message = "success"
        gw_resp.timestamp = int(datetime.datetime.now().timestamp() * 1000)
        return gw_resp


def _query_gw_config(app_config: SimGwAppConfig) -> EnrichedSimConfig:
    _db_loader = DBConfigLoader(
        mongo_url=app_config.mongo_uri,
        db_name=app_config.mongo_dbname
    )
    _gw_config_dict = _db_loader.get_simulator_config(app_config.gw_key)
    if _gw_config_dict is None:
        raise ValueError(f"simulator config not found for key {app_config.gw_key}")

    _gw_config = SimGwConfig(**_gw_config_dict)

    _enriched_config = EnrichedSimConfig(
        nats_url=app_config.nats_url,
        gw_key=app_config.gw_key,
        exch_account_id=_gw_config.exch_account_id,
        rtmd_channels=_gw_config.rtmd_channels,
        init_balances=_gw_config.init_balances,
        fund_asset=_gw_config.fund_asset,
        target_exchange=_gw_config.target_exchange,
    )

    all_refdata = _db_loader.load_refdata()
    _enriched_config.refdata = [rd for rd in all_refdata if rd.exch_name == _gw_config.target_exchange]

    return _enriched_config


def main():
    from tqrpc_utils.config_utils import try_load_config

    app_config: SimGwAppConfig = try_load_config(SimGwAppConfig)

    rpc_endpoint = f"tq.gw.service.{app_config.gw_key.lower()}"
    order_report_endpoint = f"tq.gw.report.{app_config.gw_key.lower()}"
    balance_update_endpoint = f"tq.gw.balance.{app_config.gw_key.lower()}"

    gw_config = _query_gw_config(app_config)


    sim_gw = SimGwService(gw_config)

    def decode_tick(raw_bytes: bytes) -> rtmd.TickData:
        return (rtmd.TickData()).parse(raw_bytes)

    sim_service_builder = svc_utils.RpcServiceBuilder(
        service_namespace="tq.gw",
        service_name="sim",
        rpc_endpoint=rpc_endpoint,
        nc_url=app_config.nats_url,
    ) \
        .register_rpc_method(sim_gw.place_order) \
        .register_rpc_method(sim_gw.cancel_order) \
        .register_rpc_method(sim_gw.batch_place_orders) \
        .register_rpc_method(sim_gw.batch_cancel_orders) \
        .register_rpc_method(sim_gw.query_account_balance) \
        .register_rpc_method(sim_gw.query_order_details) \
        .register_as_publisher(order_report_endpoint, sim_gw.register_report_publisher) \
        .register_as_publisher(balance_update_endpoint, sim_gw.register_balance_publisher) \

    for channel in gw_config.rtmd_channels:
        sim_service_builder.register_as_subscriber(channel, sim_gw.on_tick, decode_tick)

    sim_service = sim_service_builder.build()

    runner = svc_utils.create_runner(shutdown_cb=sim_service.stop)

    runner.run_until_complete(sim_service.start())
    tasks = sim_service.get_async_tasks()
    for task in tasks:
        runner.create_task(task)

    runner.run_forever()


if __name__ == "__main__":
    main()
