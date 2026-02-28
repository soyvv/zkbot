import os
import traceback
from dataclasses import dataclass, field
from datetime import timedelta
from typing import Union, AsyncIterator, Optional
import asyncio
import time

import betterproto
import nats
from nats.errors import TimeoutError
from httpx import Timeout, ReadTimeout

import zk_datamodel.oms as oms
import zk_datamodel.exch_gw as gw
import zk_datamodel.tqrpc_oms as oms_rpc
import zk_datamodel.tqrpc_exch_gw as gw_rpc
import zk_datamodel.common as common
import zk_datamodel.rtmd as rtmd
import zk_datamodel.ods as ods
import zk_datamodel.tqrpc_ods as ods_rpc

from loguru import logger

from snowflake import SnowflakeGenerator
import snowflake
import httpx

import json

_GLOBAL_ODS_ENDPOINT = "tq.ods.rpc"


@dataclass
class TQClientConfig:
    nats_url: str = None
    refdata_api_url: str = None
    oms_service_endpoint: str = None
    oms_service_endpoints: list[tuple[int, str]] = None
    rtmd_endpoints: list[str] = field(default_factory=list)
    rtmd_kline_endpoints: list[str] = field(default_factory=list)
    rt_signal_endpoints: list[str] = field(default_factory=list)
    oms_orderupdate_endpoint: Union[str, list[str]] = None
    oms_balanceupdate_endpoint: Union[str, list[str]] = None

@dataclass
class TQOrder:
    account_id: int = None
    order_id: int = None
    symbol: str = None
    price: float = None
    qty: float = None
    side: common.BuySellType = common.BuySellType.BS_UNSPECIFIED
    extra_args: dict = None

@dataclass
class TQCancel:
    order_id: int = None
    exch_order_id: str = None
    account_id: int = None


async def create_client(
        nats_url:str,
        account_ids: list[int],
        nats_client: nats.NATS = None,
        api_root_url: str = None,
        instance_id: int=None,
        rtmd_channels: list[str]=None,
        extra_orderupdate_channels: list[str]=None,
        extra_balanceupdate_channels: list[str]=None,
        extra_signal_channels: list[str]=None,
        source_id:str=None,
        enable_logging=True,
        enable_contract_size_adjustment=False,
        skip_refdata_loading=False,
        timeout=10) -> 'TQClient':
    '''
    create a TQClient instance
    :param nats_url: nats server url
    :param account_ids: list of account ids
    :param nats_client: nats client can also be used if created externally
    :param api_root_url: root url of the tq api for refdata loading etc.
    :param instance_id: instance id for order id generation (<1000)
    :param rtmd_channels: list of rtmd subjects
    :param extra_orderupdate_channels: list of extra orderupdate channels
    :param extra_balanceupdate_channels: list of extra balanceupdate channels
    :param extra_signal_channels: list of extra signal channels
    :param source_id: order source id; used for tracing the origin of the order
    :param enable_logging: enable logging
    :param enable_contract_size_adjustment: when enabled, the client will adjust the order qty based on contract size
    :param skip_refdata_loading: skip refdata loading
    :param timeout: default timeout in seconds when doing RPC calls
    :return: TQClient instance
    '''

    if api_root_url is None:
        # load API from Env
        tq_env = os.getenv("TQ_ENV")
        if tq_env is not None:
            if tq_env.lower() == "uat":
                api_root_url = "https://test.tq.api.intra.tokkalabs.com/tq-api"
            elif tq_env.lower() == "prod":
                api_root_url = "https://prod.tq.api.intra.tokkalabs.com/tq-api"

    if api_root_url is None:
        raise ValueError("api_root_url is required! Please provide it as an argument or set TQ_ENV in the environment.")

    config_req = ods_rpc.OmsQueryClientConfigRequest()
    config_req .trading_account_ids = account_ids

    nc = await nats.connect(nats_url) if nats_client is None else nats_client
    _data, is_error = await _rpc(nc, _GLOBAL_ODS_ENDPOINT, "QueryClientConfig",
                                 bytes(config_req), timeout_in_secs=timeout, retry_on_timeout=True)
    if not is_error:
        client_config_data = ods_rpc.OmsQueryClientConfigResponse().parse(_data)
        client_config = TQClientConfig()
        client_config.rtmd_endpoints = rtmd_channels
        client_config.nats_url = nats_url
        client_config.refdata_api_url = api_root_url
        client_config.oms_service_endpoints = client_config_data.oms_service_endpoints

        _ou_endpoints = client_config_data.oms_orderupdates_endpoints
        if extra_orderupdate_channels:
            _ou_endpoints.extend(extra_orderupdate_channels)
        client_config.oms_orderupdate_endpoint = list(set(_ou_endpoints))

        _bu_endpoints = client_config_data.oms_balanceupdates_endpoints
        if extra_balanceupdate_channels:
            _bu_endpoints.extend(extra_balanceupdate_channels)
        client_config.oms_balanceupdate_endpoint = list(set(_bu_endpoints))

        if extra_signal_channels:
            client_config.rt_signal_endpoints = extra_signal_channels

        if client_config.oms_service_endpoints is None or len(client_config.oms_service_endpoints) == 0:
            raise ValueError(f"no oms service endpoint found for account_ids: {account_ids}")

        source_id = _gen_source_id() if source_id is None else source_id

        instance_id = _gen_instance_id() if instance_id is None else instance_id

        tq_client = TQClient(config=client_config,
                             source_id=source_id,
                             nc=nc,
                             id_gen_instance=instance_id,
                             enable_logging=enable_logging,
                             enable_contract_size_adjustment=enable_contract_size_adjustment,
                             account_ids=account_ids,
                             timeout_in_secs=timeout,
                             skip_refdata_loading=skip_refdata_loading)

        return tq_client
    else:
        raise ValueError(f"error querying client config: {_data}")


def _gen_source_id() -> str:
    # use "{pid}@{hostname}" as the source id
    import socket
    import os
    return f"{os.getpid()}@{socket.gethostname()}"


def _gen_instance_id() -> int:
    return int(time.time() * 1000_000) % snowflake.snowflake.MAX_INSTANCE

async def _nc_request_with_retry(nc: nats.NATS, subject, headers, payload,
                                 max_retries=3, initial_timeout=2, backoff_factor=2):
    retry = 0
    timeout = initial_timeout
    while retry < max_retries:
        try:
            resp = await nc.request(
                subject=subject, headers=headers, payload=payload, timeout=timeout)
            return resp
        except TimeoutError as e:
            retry += 1
            timeout = min(10, timeout * backoff_factor)
            logger.warning(f"error in request: {e}")
            logger.warning(f"retrying request {retry} with timeout {timeout}")

    raise TimeoutError(f"request to {subject} failed after {max_retries} retries")


async def _http_request_with_retry(client: httpx.AsyncClient, url, init_timeout=2,
                                   max_retries=3, backoff_factor=2):
    retry = 0
    timeout = init_timeout
    while retry < max_retries:
        try:
            resp = await client.get(url, timeout=Timeout(timeout))
            return resp
        except BaseException as e:
            retry += 1
            timeout = min(10, timeout * backoff_factor)
            logger.warning(f"error in request: {e}")
            logger.warning(f"retrying request {retry} with timeout {timeout}")

    raise ReadTimeout(f"request to {url} failed after {max_retries} retries")


async def rpc(nc: nats.NATS, subject: str, method: str, payload: any, timeout_in_secs=2, retry_on_timeout=False) -> tuple[any, bool]:
    '''
    perform a RPC call to tq-rpc wrapped service
    :param nc: nats client
    :param subject: NATS subject to send the request
    :param method: method name (convention: CamelCase)
    :param payload: payload to send; pb or string
    :param timeout_in_secs: timeout in seconds
    :param retry_on_timeout: if True, will retry 3 times with backoff
    :return: tuple of (response, is_error); response is either bytes or err msg
    '''
    return await _rpc(nc=nc, subject=subject, method=method, payload=payload, timeout_in_secs=timeout_in_secs, retry_on_timeout=retry_on_timeout)


async def _rpc(nc: nats.NATS, subject: str, method: str, payload: any,
               timeout_in_secs=2, retry_on_timeout=False) -> tuple[any, bool]:
    headers = {"rpc_method": method}
    if isinstance(payload, str):
        payload_bytes = bytes(payload, encoding='utf-8')
    else:
        payload_bytes = bytes(payload)
    if retry_on_timeout:
        resp = await _nc_request_with_retry(nc, subject, headers, payload_bytes, max_retries=3,
                                            initial_timeout=timeout_in_secs if timeout_in_secs < 10 else 10)
    else:
        resp = await nc.request(subject=subject, headers=headers, payload=payload_bytes, timeout=timeout_in_secs)

    if resp.headers and resp.headers.get("error"):
        is_error = True
        rpc_error_msg = str(resp.data, encoding='utf-8')
        rpc_error = json.dumps(rpc_error_msg)
        return (rpc_error, is_error)
    else:
        return resp.data, False



class TQClient:
    def __init__(self,
                 config: TQClientConfig,
                 source_id:str,
                 nc: nats.NATS = None,
                 timeout_in_secs=5,
                 account_ids: list[int]=None,
                 id_gen_instance: int=None,
                 enable_logging=True,
                 enable_contract_size_adjustment=False,
                 skip_refdata_loading=False):
        self.config = config
        self.enable_logging = enable_logging
        self.enable_contract_size_adjustment = enable_contract_size_adjustment
        self.skip_refdata_loading = skip_refdata_loading
        self.nc: nats.NATS = nc if nc is not None else None
        self.rtmd_sub = []
        self.signal_sub = []
        self.order_update_sub = None
        self.balance_update_sub = None
        id_gen_instance = id_gen_instance if id_gen_instance is not None else _gen_instance_id()
        self.id_gen = SnowflakeGenerator(instance=id_gen_instance) # todo: use a better instance id
        self.source_id = source_id
        self.timeout = timeout_in_secs
        if config.oms_service_endpoints:
            if isinstance(config.oms_service_endpoints, list):
                self.oms_dispatch_table: dict[int, str] = {
                    account_id: endpoint
                    for account_id, endpoint in config.oms_service_endpoints}
            elif isinstance(config.oms_service_endpoints, dict):
                self.oms_dispatch_table = config.oms_service_endpoints

        else:
            self.oms_dispatch_table = None

        self.account_ids = set(self.oms_dispatch_table.keys()) \
            if self.oms_dispatch_table \
            else set(account_ids) if account_ids else None

        self._symbol_details: dict[str, common.InstrumentRefData] = {}
        self._account_details: dict[int, ods.AccountTradingConfig] = {}

        self._exch_symbol_to_inst_id: dict[str, dict[str, str]] = {}



    async def start(self, init_timeout=10):
        logger.info("connecting to nats server")
        if self.nc is None or self.nc.is_closed:
            self.nc = await nats.connect(self.config.nats_url)

        logger.info("loading account details")
        await self.load_account_details(rpc_timeout=init_timeout)

        if not self.skip_refdata_loading:
            logger.info("loading refdata")
            await self.load_refdata(rpc_timeout=init_timeout)
        else:
            logger.info("skipping refdata loading")



    async def stop(self):
        await self.nc.close()


    async def load_refdata(self, rpc_timeout=10):
        '''
        load all related (for the given accounts) refdata from the refdata api
        '''

        exchanges = set([acc_detail.gw_config.exch_name for acc_detail in self._account_details.values()])
        _refdata: list[common.InstrumentRefData] = []

        async def _query_for_exch(exch):
            logger.info(f"querying refdata for exchange {exch}")
            refdata_url = f"{self.config.refdata_api_url}/v1/refdata/instruments/{exch}"
            # resp = await client.get(refdata_url, timeout=Timeout(rpc_timeout))
            resp = await _http_request_with_retry(client, refdata_url,
                                                  init_timeout=rpc_timeout if rpc_timeout < 10 else 10,
                                                  max_retries=3)
            if resp.status_code == 200:
                refdata_list = resp.json()
                for refdata in refdata_list:
                    try:
                        _refdata_entry = common.InstrumentRefData().from_dict(refdata)
                        if self .enable_contract_size_adjustment:
                            self._adjust_refdata_for_contract_size(_refdata_entry)
                        _refdata.append(_refdata_entry)
                    except:
                        logger.error(f"error decoding refdata: {refdata}")
                        logger.error(f"error msg refdata: {traceback.format_exc()}")
                logger.info(f"loaded {len(refdata_list)} refdata for exchange {exch}")
            else:
                logger.error(f"error querying refdata for exchange {exch}: {resp.text}")

        async with httpx.AsyncClient() as client:
            _tasks = []
            for exch in exchanges:
                # query via refdata api: {REFDATA_API}/refdata/instruments?exchange={exch}
                task = asyncio.create_task(_query_for_exch(exch))
                _tasks.append(task)
            await asyncio.gather(*_tasks)

        self._symbol_details = {refdata.instrument_id: refdata for refdata in _refdata}

        _exch_symbol_to_inst_id: dict[str, dict[str, str]] = {}  # exch -> exch_symbol -> inst_id
        for inst_id, refdata_entry in self._symbol_details.items():
            exch = refdata_entry.exch_name
            exch_symbol = refdata_entry.exch_symbol
            if exch not in _exch_symbol_to_inst_id:
                _exch_symbol_to_inst_id[exch] = {}
            _exch_symbol_to_inst_id[exch][exch_symbol] = inst_id

        self._exch_symbol_to_inst_id = _exch_symbol_to_inst_id


    def get_cached_refdata(self):
        if self._symbol_details:
            return self._symbol_details
        else:
            raise ValueError("refdata not loaded yet")


    async def load_account_details(self, rpc_timeout=10):
        if not self.account_ids:
            raise ValueError("no account ids provided or inferred")

        accounts = list(self.account_ids)
        # load account details
        _account_details: dict[int, ods.AccountTradingConfig] = {}
        account_req = ods_rpc.OmsQueryAccountTradingConfigRequest()
        account_req.account_ids = accounts
        _data, is_error = await _rpc(self.nc, _GLOBAL_ODS_ENDPOINT,
                                     "QueryAccountTradingConfig", bytes(account_req), timeout_in_secs=rpc_timeout,
                                     retry_on_timeout=True)
        if not is_error:
            resp = ods_rpc.OmsQueryAccountTradingConfigResponse().parse(_data)
            for account_config in resp.account_trading_configs:
                _account_details[account_config.account_id] = account_config

        self._account_details = _account_details


    def get_account_details(self):
        if self._account_details:
            return self._account_details
        else:
            raise ValueError("account details not loaded yet")


    def get_account_detail(self, account_id: int) -> ods.AccountTradingConfig:
        if account_id in self._account_details:
            return self._account_details[account_id]
        else:
            raise ValueError(f"account {account_id} not found locally in the client")


    async def subscribe_order_update(self, order_update_handler):
        async def _sync_handler(msg):
            data = msg.data  # bytes
            report = oms.OrderUpdateEvent().parse(data)
            if self.enable_contract_size_adjustment:
                self._adjust_orderupdate_for_contract_size(report)
            if self.account_ids and report.account_id not in self.account_ids:
                return

            order_update_handler(report)

        async def _async_handler(msg):
            data = msg.data  # bytes
            report = oms.OrderUpdateEvent().parse(data)
            if self.enable_contract_size_adjustment:
                self._adjust_orderupdate_for_contract_size(report)
            if self.account_ids and report.account_id not in self.account_ids:
                return
            await order_update_handler(report)

        _handler = _async_handler if asyncio.iscoroutinefunction(order_update_handler) else _sync_handler

        if isinstance(self.config.oms_orderupdate_endpoint, str):
            logger.info(f"subscribing to orderupdate endpoint: {self.config.oms_orderupdate_endpoint}")
            sub = await self.nc.subscribe(self.config.oms_orderupdate_endpoint, cb=_handler)
            self.order_update_sub = sub
        elif isinstance(self.config.oms_orderupdate_endpoint, list):
            subs = []
            for endpoint in self.config.oms_orderupdate_endpoint:
                logger.info(f"subscribing to orderupdate endpoint: {endpoint}")
                sub = await self.nc.subscribe(endpoint, cb=_handler)
                subs.append(sub)
            self.order_update_sub = subs
        else:
            raise ValueError("oms_orderupdate_endpoint must be either a string or a list of strings")


    async def subscribe_rtmd(self, rtmd_handler, rtmd_endpoints: list[str]=None):
        if not rtmd_endpoints and not self.config.rtmd_endpoints:
            raise ValueError("no rtmd endpoints provided")
        async def _handler(msg):
            data = msg.data  # bytes
            report = rtmd.TickData().parse(data)
            if self.enable_contract_size_adjustment:
                self._adjust_tickdata_for_contract_size(report)
            rtmd_handler(report)
        _endpoints = (set(self.config.rtmd_endpoints) | set(rtmd_endpoints)) if rtmd_endpoints else self.config.rtmd_endpoints
        for endpoint in _endpoints:
            logger.info(f"subscribing to rtmd endpoint: {endpoint}")
            sub = await self.nc.subscribe(endpoint, cb=_handler)
            self.rtmd_sub.append(sub)
        #sub = await self.nc.subscribe(subject="tq.rtmd.ddex", cb=_handler)
        #self.rtmd_sub = sub

    async def subscribe_rtmd_kline(self, kline_handler, rtmd_kline_endpoints: list[str]=None):
        async def _handler(msg):
            data = msg.data  # bytes
            report = rtmd.Kline().parse(data)
            kline_handler(report)
        _endpoints = set(self.config.rtmd_kline_endpoints).update(rtmd_kline_endpoints) \
            if rtmd_kline_endpoints else self.config.rtmd_kline_endpoints
        for endpoint in _endpoints:
            logger.info(f"subscribing to kline endpoint: {endpoint}")
            sub = await self.nc.subscribe(endpoint, cb=_handler)
            self.rtmd_sub.append(sub)

    async def subscribe_signal(self, signal_handler, signal_endpoints: list[str]=None):
        async def _handler(msg):
            data = msg.data  # bytes
            signal = rtmd.RealtimeSignal().parse(data)
            return signal_handler(signal)

        subs = []
        _endpoints = set(self.config.rt_signal_endpoints).update(signal_endpoints) \
            if signal_endpoints else self.config.rt_signal_endpoints
        for endpoint in _endpoints:
            logger.info(f"subscribing to signal endpoint: {endpoint}")
            sub = await self.nc.subscribe(endpoint, cb=_handler)
            subs.append(sub)
        self.signal_sub = subs

    async def subscribe_balance_update(self, balance_update_handler):
        async def _handler(msg):
            data = msg.data  # bytes
            report = oms.PositionUpdateEvent().parse(data)
            if self.enable_contract_size_adjustment:
                for pos in report.position_snapshots:
                    self._adjust_position_for_contract_size(pos)
            return balance_update_handler(report)

        if isinstance(self.config.oms_balanceupdate_endpoint, str):
            logger.info(f"subscribing to balanceupdate endpoint: {self.config.oms_balanceupdate_endpoint}")
            sub = await self.nc.subscribe(self.config.oms_balanceupdate_endpoint, cb=_handler)
            self.balance_update_sub = sub
        elif isinstance(self.config.oms_balanceupdate_endpoint, list):
            subs = []
            for endpoint in self.config.oms_balanceupdate_endpoint:
                logger.info(f"subscribing to balanceupdate endpoint: {endpoint}")
                sub = await self.nc.subscribe(endpoint, cb=_handler)
                subs.append(sub)
            self.balance_update_sub = subs
        else:
            raise ValueError("oms_balanceupdate_endpoint must be either a string or a list of strings")

    async def buy(self, symbol: str,
                  price: float,
                  qty: float,
                  account_id: int, **kwargs) -> int:
        return await self.send_order(symbol, price, qty, account_id,
                                      common.BuySellType.BS_BUY, **kwargs)


    async def sell(self, symbol: str,
                   price: float,
                   qty: float,
                   account_id: int, **kwargs) -> int:
        return await self.send_order(symbol, price, qty, account_id,
                                      common.BuySellType.BS_SELL, **kwargs)


    async def send_order(self, symbol: str, price: float,
                         qty: float, account_id: int, side: common.BuySellType,
                         order_type=common.BasicOrderType.ORDERTYPE_LIMIT,
                         tif_type=common.TimeInForceType.TIMEINFORCE_GTC,
                         trader_name:str=None,
                         client_order_id=None, extra_args:dict=None):
        if client_order_id is None:
            client_order_id = self._gen_order_id_locally()

        qty = qty if not self.enable_contract_size_adjustment else \
            self._adjust_qty_to_contract_size(symbol, qty)

        req = oms.OrderRequest(
            order_id=client_order_id,
            account_id=account_id,
            instrument_code=symbol,
            price=price,
            qty=qty,
            open_close_type=common.OpenCloseType.OC_OPEN,
            buy_sell_type=side,
            order_type=order_type,
            time_inforce_type=tif_type
        )

        if extra_args:
            # override the default values
            if "order_type" in extra_args and isinstance(extra_args["order_type"],
                                                         common.BasicOrderType):
                req.order_type = extra_args["order_type"]

            if "time_inforce_type" in extra_args and isinstance(extra_args["time_inforce_type"],
                                                                common.TimeInForceType):
                req.time_inforce_type = extra_args["time_inforce_type"]

            extra_data = common.ExtraData()
            extra_data.data_map = {k: str(v) for k, v in extra_args.items()}
            req.extra_properties = extra_data

        if self.source_id:
            if trader_name:
                req.source_id = trader_name + "#" + self.source_id
            else:
                req.source_id = self.source_id
        else:
            if trader_name:
                req.source_id = trader_name

        rpc_req = oms_rpc.OmsPlaceOrderRequest(
            order_request=req
        )

        # await self.nc.publish(subject="tq.oms.service.oms1.place_order", payload=bytes(req))

        oms_rpc_endpoint = self._resolve_oms_rpc_endpoint(account_id)
        if oms_rpc_endpoint is None:
            raise ValueError(f"oms rpc endpoint not found for account_id {account_id}")
        response_msg = await self.nc.request(subject=oms_rpc_endpoint,
                                             headers={"rpc_method": "PlaceOrder"},
                                             payload=bytes(rpc_req), timeout=self.timeout)
        response = oms_rpc.OmsResponse().parse(response_msg.data)
        logger.info(f"place order response: {response}")
        return client_order_id

    async def batch_send_orders(self, batched_orders: list[TQOrder]):

        all_orders: list[oms.OrderRequest] = []
        for b_order in batched_orders:
            _qty = b_order.qty if not self.enable_contract_size_adjustment else \
                self._adjust_qty_to_contract_size(b_order.symbol, b_order.qty)
            bo_req = oms.OrderRequest(
                order_id=b_order.order_id,
                account_id=b_order.account_id,
                instrument_code=b_order.symbol,
                price=b_order.price,
                qty=_qty,
                open_close_type=common.OpenCloseType.OC_OPEN,
                buy_sell_type=b_order.side,
                order_type=common.BasicOrderType.ORDERTYPE_LIMIT, # only limit order is supported for batch orders
                time_inforce_type=common.TimeInForceType.TIMEINFORCE_GTC,
                source_id=self.source_id
            )
            if b_order.extra_args:
                # override the default values
                extra_args = b_order.extra_args
                if "order_type" in extra_args and isinstance(extra_args["order_type"],
                                                             common.BasicOrderType):
                    bo_req.order_type = extra_args["order_type"]

                if "time_inforce_type" in extra_args and isinstance(extra_args["time_inforce_type"],
                                                                    common.TimeInForceType):
                    bo_req.time_inforce_type = extra_args["time_inforce_type"]

                extra_data = common.ExtraData()
                extra_data.data_map = {k: str(v) for k, v in b_order.extra_args.items()}
                bo_req.extra_properties = extra_data
            all_orders.append(bo_req)


        if self.oms_dispatch_table is None:
            # use the default oms service endpoint
            req = oms_rpc.OmsBatchPlaceOrdersRequest()
            req.order_requests = all_orders
            response_msg = await self.nc.request(subject=self.config.oms_service_endpoint,
                                                 headers={"rpc_method": "BatchPlaceOrders"},
                                                 payload=bytes(req), timeout=self.timeout)
            response = oms_rpc.OmsResponse().parse(response_msg.data)
            logger.info(f"batch place order response: {response}")
        else:
            # group the orders by account_id if the dispatch table is provided
            account_id_to_orders = {}
            for _order in all_orders:
                if _order.account_id not in account_id_to_orders:
                    account_id_to_orders[_order.account_id] = []
                account_id_to_orders[_order.account_id].append(_order)

            for account_id, orders in account_id_to_orders.items():
                oms_rpc_endpoint = self._resolve_oms_rpc_endpoint(account_id)
                if oms_rpc_endpoint is None:
                    raise ValueError(f"oms rpc endpoint not found for account_id {account_id}")

                req = oms_rpc.OmsBatchPlaceOrdersRequest()
                req.order_requests = orders
                response_msg = await self.nc.request(subject=oms_rpc_endpoint,
                                                     headers={"rpc_method": "BatchPlaceOrders"},
                                                     payload=bytes(req), timeout=self.timeout)
                response = oms_rpc.OmsResponse().parse(response_msg.data)
                logger.info(f"batch place order response: {response}")

    async def cancel(self, order_id: int, **kwargs):
        cancel_req = oms.OrderCancelRequest()
        cancel_req.order_id = order_id

        rpc_req = oms_rpc.OmsCancelOrderRequest(
            order_cancel_request=cancel_req
        )

        if self.oms_dispatch_table is None:
            response_msg = await self.nc.request(subject=self.config.oms_service_endpoint,
                                                 headers = {"rpc_method": "CancelOrder"},
                                                 payload=bytes(rpc_req), timeout=self.timeout)

            response = oms_rpc.OmsResponse().parse(response_msg.data)
            logger.info(f"cancel response: {response}")
        else:
            account_id = kwargs.get("account_id")
            if account_id is None:
                raise ValueError("account_id is required when multiple oms endpoint is provided")

            oms_rpc_endpoint = self._resolve_oms_rpc_endpoint(account_id)
            if oms_rpc_endpoint is None:
                raise ValueError(f"oms rpc endpoint not found for account_id {account_id}")
            response_msg = await self.nc.request(subject=oms_rpc_endpoint,
                                                 headers={"rpc_method": "CancelOrder"},
                                                 payload=bytes(rpc_req), timeout=self.timeout)

            response = oms_rpc.OmsResponse().parse(response_msg.data)
            logger.info(f"cancel response: {response}")

    async def batch_cancel(self, cancels: list[TQCancel]):
        if self.oms_dispatch_table is None:
            req = oms_rpc.OmsBatchCancelOrdersRequest()
            cancel_reqs = []
            for _c in cancels:
                cancel_req = oms.OrderCancelRequest()
                cancel_req.order_id = _c.order_id
                cancel_reqs.append(cancel_req)

            req.order_cancel_requests = cancel_reqs
            response_msg = await self.nc.request(subject=self.config.oms_service_endpoint,
                                                 headers={"rpc_method": "BatchCancelOrders"},
                                                 payload=bytes(req), timeout=self.timeout)

            response = oms_rpc.OmsResponse().parse(response_msg.data)
            logger.info(f"batch cancel response: {response}")
        else:
            # group cancels by account_id
            account_id_to_cancels: dict[int, list[TQCancel]] = {}
            for cancel in cancels:
                if cancel.account_id not in account_id_to_cancels:
                    account_id_to_cancels[cancel.account_id] = []
                account_id_to_cancels[cancel.account_id].append(cancel)

            for account_id, cancels in account_id_to_cancels.items():
                oms_rpc_endpoint = self._resolve_oms_rpc_endpoint(account_id)
                if oms_rpc_endpoint is None:
                    raise ValueError(f"oms rpc endpoint not found for account_id {account_id}")
                req = oms_rpc.OmsBatchCancelOrdersRequest()
                cancel_reqs = []
                for _c in cancels:
                    cancel_req = oms.OrderCancelRequest()
                    cancel_req.order_id = _c.order_id
                    cancel_reqs.append(cancel_req)

                req.order_cancel_requests = cancel_reqs
                response_msg = await self.nc.request(subject=oms_rpc_endpoint,
                                                     headers={"rpc_method": "BatchCancelOrders"},
                                                     payload=bytes(req), timeout=self.timeout)

                response = oms_rpc.OmsResponse().parse(response_msg.data)
                logger.info(f"batch cancel response: {response}")



    async def get_account_balance(self, account_id: int, query_gw=False, **kwargs) -> list[oms.Position]:
        query_request = oms_rpc.OmsQueryAccountRequest()
        headers = {"rpc_method": "QueryAccountBalance"}
        query_request.account_id = account_id
        query_request.query_gw = query_gw
        rpc_endpoint = self._resolve_oms_rpc_endpoint(account_id)
        if rpc_endpoint is None:
            raise ValueError(f"oms rpc endpoint not found for account_id {account_id}")

        timeout_secs = kwargs.get('timeout', self.timeout)
        resp_data, is_error = await _rpc(nc=self.nc, subject=rpc_endpoint, method="QueryAccountBalance",
                          payload=query_request, retry_on_timeout=True, timeout_in_secs=timeout_secs)
        if not is_error and resp_data is not None:
            query_resp = oms_rpc.OmsAccountResponse().parse(resp_data)
            if self.enable_contract_size_adjustment:
                for pos in query_resp.account_balance_entries:
                    self._adjust_position_for_contract_size(pos)
            return query_resp.account_balance_entries
        else:
            logger.error(f"error querying account balance: {resp_data}")
            return []


    async def get_open_orders(self, account_id:int, query_gw=False, **kwargs)-> list[oms.Order]:
        rpc_req = oms_rpc.OmsQueryOpenOrderRequest(
            account_id=account_id,
            query_gw=query_gw
        )
        rpc_endpoint = self._resolve_oms_rpc_endpoint(account_id)
        if rpc_endpoint is None:
            raise ValueError(f"oms rpc endpoint not found for account_id {account_id}")

        timeout_secs = kwargs.get('timeout', self.timeout)
        resp_data, is_error = await _rpc(nc=self.nc, subject=rpc_endpoint, method="QueryOpenOrders",
                                    payload=rpc_req, retry_on_timeout=True, timeout_in_secs=timeout_secs)
        if not is_error and resp_data is not None:
            response = oms_rpc.OmsOrderDetailResponse().parse(resp_data)
            return response.orders
        else:
            logger.error(f"error querying open orders: {resp_data}")
            return []


    async def get_account_summary(self, account_id:int) -> Optional[gw.AccountSummaryEntry]:
        rpc_req = ods_rpc.ODSQueryAccountSummaryRequest()
        rpc_req.account_id = account_id

        resp, is_error = await _rpc(nc=self.nc, subject=_GLOBAL_ODS_ENDPOINT, method="QueryAccountSummary",
                                    payload=rpc_req, retry_on_timeout=True, timeout_in_secs=self.timeout)
        if not is_error and resp is not None:
            response = ods_rpc.ODSAccountSummaryResponse().parse(resp)
            return response.account_summary
        else:
            logger.error(f"error querying account summary: {resp}")
            return None


    async def get_instrument_margin_setting(self, account_id:int, instrument_id:str) -> Optional[gw.AccountSettingEntry]:
        rpc_req = ods_rpc.ODSQueryAccountSettingRequest()
        rpc_req.account_id = account_id
        rpc_req.instrument_id = instrument_id

        resp, is_error = await _rpc(nc=self.nc, subject=_GLOBAL_ODS_ENDPOINT, method="QueryAccountSetting",
                                    payload=rpc_req, retry_on_timeout=True, timeout_in_secs=self.timeout)
        if not is_error and resp is not None:
            response = ods_rpc.ODSAccountSettingResponse().parse(resp)
            return response.account_setting_entry
        else:
            logger.error(f"error querying account setting: {resp}")
            return None

    async def update_leverage(self, account_id:int, instrument_id: str,
                              leverage: float, long_leverage: float=None, short_leverage: float=None):
        rpc_req = ods_rpc.ODSUpdateAccountSettingRequest()
        rpc_req .account_id = account_id
        long_leverage = leverage if long_leverage is None else long_leverage
        short_leverage = leverage if short_leverage is None else short_leverage

        setting = gw.AccountSettingEntry()
        setting.instrument = instrument_id
        setting.long_leverage = long_leverage
        setting.short_leverage = short_leverage
        rpc_req.account_setting_entry = setting

        resp, is_error = await _rpc(nc=self.nc, subject=_GLOBAL_ODS_ENDPOINT, method="UpdateAccountSetting",
                                    payload=rpc_req, retry_on_timeout=True, timeout_in_secs=self.timeout)
        if not is_error and resp is not None:
            response = ods_rpc.ODSUpdateAccountSettingResponse().parse(resp)
            return response
        else:
            logger.error(f"error updating account setting: {resp}")
            return None



    async def update_margin_mode(self, account_id:int, instrument_id: str, margin_mode: str):
        rpc_req = ods_rpc.ODSUpdateAccountSettingRequest()
        rpc_req.account_id = account_id
        setting = gw.AccountSettingEntry()
        setting.instrument = instrument_id
        setting.margin_mode = margin_mode
        rpc_req.setting = setting

        resp, is_error = await _rpc(nc=self.nc, subject=_GLOBAL_ODS_ENDPOINT, method="UpdateAccountSetting",
                                    payload=rpc_req, retry_on_timeout=True, timeout_in_secs=self.timeout)
        if not is_error and resp is not None:
            response = ods_rpc.ODSUpdateAccountSettingResponse().parse(resp)
            return response
        else:
            logger.error(f"error updating account setting: {resp}")
            return None


    async def get_rtmd_channel(self, instrument_id: str) -> Optional[str]:
        req = ods_rpc.ODSRtmdQueryRequest(instrument_id=instrument_id)
        resp_data, is_error = await _rpc(nc=self.nc,
                          subject=_GLOBAL_ODS_ENDPOINT,
                         method="QueryRtmdChannel",
                         payload=bytes(req), retry_on_timeout=True)

        if not is_error and resp_data is not None:
            resp_data = ods_rpc.ODSRtmdChannelQueryResponse().parse(resp_data)
            if resp_data.channel_present:
                return resp_data.rtmd_channel
        return None

    async def get_orderbook(self, instrument_id: str) -> Optional[rtmd.TickData]:
        req = ods_rpc.ODSRtmdQueryRequest(instrument_id=instrument_id)
        resp_data, is_error = await _rpc(nc=self.nc,
                          subject=_GLOBAL_ODS_ENDPOINT,
                         method="QueryRtmdOrderbook",
                         payload=bytes(req), retry_on_timeout=True)

        if not is_error and resp_data is not None:
            resp_data = ods_rpc.ODSRtmdDataQueryResponse().parse(resp_data)
            if resp_data.data_present:
                return rtmd.TickData().parse(resp_data.rtmd_orderbook_bytes)
        return None

    async def panic(self, account_id: int, reason: str=None, source_id: str=None):
        rpc_req = oms_rpc.PanicRequest(
            panic_account_id=account_id,
            panic_source=source_id if source_id is not None else self.source_id,
            panic_message=reason
        )
        rpc_endpoint = self._resolve_oms_rpc_endpoint(account_id)
        if rpc_endpoint is None:
            raise ValueError(f"oms rpc endpoint not found for account_id {account_id}")
        response_msg = await self.nc.request(subject=rpc_endpoint,
                                             headers={"rpc_method": "Panic"},
                                             payload=bytes(rpc_req))

        response = oms_rpc.OmsResponse().parse(response_msg.data)
        logger.info(f"panic response: {response}")

    async def dont_panic(self, account_id: int):
        rpc_req = oms_rpc.DontPanicRequest(
            panic_account_id=account_id
        )
        rpc_endpoint = self._resolve_oms_rpc_endpoint(account_id)
        if rpc_endpoint is None:
            raise ValueError(f"oms rpc endpoint not found for account_id {account_id}")
        response_msg = await self.nc.request(subject=rpc_endpoint,
                                             headers={"rpc_method": "DontPanic"},
                                             payload=bytes(rpc_req))

        response = oms_rpc.OmsResponse().parse(response_msg.data)
        logger.info(f"dont panic response: {response}")


    def get_instrument_refdata(self, symbol: str) -> common.InstrumentRefData:
        if symbol in self._symbol_details:
            return self._symbol_details[symbol]
        return None  


    def get_instrument_id(self, account_id: int, 
            quote_asset: str, base_asset: str, 
            inst_type: common.InstrumentType=common.InstrumentType.INST_TYPE_PERP) -> str:
        exch_name = self._account_details[account_id].gw_config.exch_name
        type_suffix = "-P" if inst_type == common.InstrumentType.INST_TYPE_PERP else ""
        inst_id = f"{base_asset}{type_suffix}/{quote_asset}@{exch_name}"
        if inst_id in self._symbol_details:
            return inst_id
        else:
            # try to find the symbol in the refdata
            for symbol, refdata in self._symbol_details.items():
                if refdata.base_symbol == base_asset and \
                refdata.quote_symbol == quote_asset and  \
                refdata.instrument_type == inst_type and \
                    refdata.exch_name == exch_name:
                    return symbol
        return None


    def get_instrument_id_from_exch_symbol(self, exch: str, exch_symbol: str):
        if exch in self._exch_symbol_to_inst_id:
            return self._exch_symbol_to_inst_id[exch].get(exch_symbol)
        return None


    async def get_trade_history_from_exch(self, account_id:int,
                                start_ts: int, end_ts: int,
                                deduplicate:bool=True,
                                instrument_id_exch=None,
                                page_direction="backward", # forward or backward
                                batch_size=100, timeout=2, retry_on_timeout=True) -> AsyncIterator[list[gw_rpc.HistoryTradeRecord]]:
        account_detail = self._account_details.get(account_id)
        if account_detail is None:
            raise ValueError(f"account {account_id} not found")

        rpc_endpoint = account_detail.gw_config.rpc_endpoint
        rpc_method = "QueryTradeHistory"
        _start_ts = start_ts
        _end_ts = end_ts
        _idx_set = set()
        while True:
            query_req = gw_rpc.ExchQueryTradeDetailRequest(
                start_ts=_start_ts,
                limit=batch_size,
                instrument_code=instrument_id_exch,
                end_ts=_end_ts,
                exch_account_id=account_detail.account_route.exch_account_id
            )
            _resp, is_error = await _rpc(self.nc, rpc_endpoint, rpc_method, query_req,
                                         timeout_in_secs=timeout,
                                         retry_on_timeout=retry_on_timeout)
            if is_error:
                logger.error(f"error querying trade history: {_resp}")
                break
            resp = gw_rpc.ExchTradeDetailResponse().parse(_resp)
            _batch_trades = resp.history_trades
            if len(_batch_trades) > 0:
                if deduplicate:
                    yield [t for t in _batch_trades if t.idx not in _idx_set
                           and start_ts <= t.trade.filled_ts <= end_ts]
                else:
                    yield [t for t in _batch_trades if
                           start_ts <= t.trade.filled_ts <= end_ts]

            if deduplicate:
                _idx_set.update([t.idx for t in _batch_trades])

            if len(_batch_trades) < batch_size:
                break

            if page_direction == "backward":
                # most recent trades are delivered first; update end_ts only
                _end_ts = min([t.timestamp for t in _batch_trades]) - 1
            elif page_direction == "forward":
                # oldest trades are delivered first; update start_ts only
                _start_ts = max([t.timestamp for t in _batch_trades]) + 1
            else:
                raise ValueError(f"invalid page_direction: {page_direction}")

            # if _batch_trades[0].timestamp < _batch_trades[-1].timestamp:
            #     # delivered in timestamp order
            #     _start_ts = _batch_trades[-1].timestamp +1
            # elif _batch_trades[0].timestamp > _batch_trades[-1].timestamp:
            #     # delivered in reverse timestamp order
            #     _end_ts = _batch_trades[-1].timestamp-1

            if _start_ts > _end_ts:
                break



    async def publish(self, subject: str, payload: any):
        await self.nc.publish(subject=subject, payload=bytes(payload))


    async def request(self, subject: str, method: str, payload: any, timeout=2):
        return await _rpc(self.nc, subject, method, payload, timeout_in_secs=timeout)


    def _gen_order_id_locally(self) -> int:
        _id = next(self.id_gen)
        return _id

    def _resolve_oms_rpc_endpoint(self, account_id: int) -> str:
        if self.oms_dispatch_table:
            return self.oms_dispatch_table.get(account_id)
        else:
            return self.config.oms_service_endpoint

    def _adjust_tickdata_for_contract_size(self, tickdata: rtmd.TickData):
        exch_name = tickdata.exchange
        instrument_id = self.get_instrument_id_from_exch_symbol(exch_name, tickdata.instrument_code)
        if instrument_id is None:
            raise ValueError(f"instrument id not found for exchange symbol {tickdata.instrument_code}")
        for b in tickdata .buy_price_levels:
            b.qty = self._adjust_contract_size_to_qty(instrument_id, b.qty)
        for s in tickdata.sell_price_levels:
            s.qty = self._adjust_contract_size_to_qty(instrument_id, s.qty)

        tickdata.contract_size_adjusted = True



    def _adjust_position_for_contract_size(self, pos: oms.Position):
        if pos.account_id not in self.account_ids:
            return
        if pos.instrument_type != common.InstrumentType.INST_TYPE_SPOT:
            pos.avail_qty = self._adjust_contract_size_to_qty(pos.instrument_code, pos.avail_qty)
            pos.frozen_qty = self._adjust_contract_size_to_qty(pos.instrument_code, pos.frozen_qty)
            pos.total_qty = self._adjust_contract_size_to_qty(pos.instrument_code, pos.total_qty)
            pos.contract_size_adjusted = True

    def _adjust_orderupdate_for_contract_size(self, order_update: oms.OrderUpdateEvent):
        if order_update.account_id not in self.account_ids:
            return
        refdata_entry = self.get_instrument_refdata(order_update.order_snapshot.instrument)
        if refdata_entry is None:
            raise ValueError(f"refdata not found for instrument {order_update.order_snapshot.instrument}")
        if refdata_entry.contract_size is None or refdata_entry.contract_size == 0:
            raise ValueError(f"contract size not found for instrument {order_update.order_snapshot.instrument}")

        order_update.order_snapshot.qty = order_update.order_snapshot.qty * refdata_entry.contract_size
        order_update.order_snapshot.filled_qty = order_update.order_snapshot.filled_qty * refdata_entry.contract_size

        if betterproto .serialized_on_wire(order_update.last_trade):
            order_update.last_trade.filled_qty = order_update.last_trade.filled_qty * refdata_entry.contract_size

        if betterproto.serialized_on_wire(order_update.order_inferred_trade):
            order_update.order_inferred_trade.filled_qty = order_update.order_inferred_trade.filled_qty * refdata_entry.contract_size

    def _adjust_qty_to_contract_size(self, instrument_id: str, qty: float) -> float:
        refdata_entry = self.get_instrument_refdata(instrument_id)
        if refdata_entry is None:
            raise ValueError(f"refdata not found for instrument {instrument_id}")
        if refdata_entry.contract_size is None or refdata_entry.contract_size == 0:
            raise ValueError(f"contract size not found for instrument {instrument_id}")
        return qty / refdata_entry .contract_size  # TODO: assuming OMS will handle the rounding

    def _adjust_contract_size_to_qty(self, instrument_id: str, contracts: float) -> float:
        refdata_entry = self.get_instrument_refdata(instrument_id)
        if refdata_entry is None:
            raise ValueError(f"refdata not found for instrument {instrument_id}")
        if refdata_entry.contract_size is None or refdata_entry.contract_size == 0:
            raise ValueError(f"contract size not found for instrument {instrument_id}")
        return contracts * refdata_entry.contract_size


    def _adjust_refdata_for_contract_size(self, refdata: common.InstrumentRefData):
        if refdata.contract_size is not None and refdata.contract_size != 0:
            if refdata.min_order_qty:
                refdata.min_order_qty = refdata.min_order_qty * refdata.contract_size
            if refdata.max_order_qty:
                refdata.max_order_qty = refdata.max_order_qty * refdata.contract_size
            if refdata .qty_lot_size:
                refdata.qty_lot_size = refdata.qty_lot_size * refdata.contract_size
            if refdata.max_mkt_order_qty:
                refdata.max_mkt_order_qty = refdata.max_mkt_order_qty * refdata.contract_size

        else:
            raise ValueError(f"contract size not found for instrument {refdata.instrument_id}")

