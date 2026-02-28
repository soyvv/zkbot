from dataclasses import dataclass
import traceback
from pymongo import MongoClient
import redis.asyncio as aioredis
from snowflake import SnowflakeGenerator

import zk_datamodel.tqrpc_ods as tqrpc_ods
import zk_datamodel.ods as ods
import zk_datamodel.oms as oms
import zk_datamodel.exch_gw as exch_gw
import zk_datamodel.tqrpc_exch_gw as rpc_gw
import zk_datamodel.common as common
import nats
import asyncio

from zk_client.tqclient import rpc


from loguru import logger

from zk_datamodel import rtmd


@dataclass
class ODSConfig:
    redis_host: str = None
    redis_port: int = None
    redis_password: str = None
    mongo_uri: str = None
    mongo_db_name: str = None
    nats_url: str = None
    #notify_topic: str = None


_inst_type_map = {
    "PERP": "INST_TYPE_PERP",
    "SPOT": "INST_TYPE_SPOT",
    "INST_TYPE_PERP": "INST_TYPE_PERP",
    "INST_TYPE_SPOT": "INST_TYPE_SPOT",
}

_GW_RPC_DEFAULT_TIMEOUT = 3

class ODSServer:

    def __init__(self, config: ODSConfig):
        self.config = config
        self.redis_client: aioredis.Redis = None
        self.nc = None
        self.id_gen = SnowflakeGenerator(instance=1)

        self.mongo_client = MongoClient(self.config.mongo_uri)
        self.db = self.mongo_client[self.config.mongo_db_name]

        #self.oms_config_dict: dict[str, ods.OmsConfigEntry] = {}
        self.account_config_dict: dict[int, ods.AccountTradingConfig] = {}
        self.instrument_refdata: dict[str, common.InstrumentRefData] = {}


    async def start(self):
        self.nc = await nats.connect(self.config.nats_url)
        config = self.config
        redis_url = f"redis://:{config.redis_password}@{config.redis_host}:{config.redis_port}/0"
        pool = aioredis.ConnectionPool.from_url(redis_url)
        self.redis_client = aioredis.Redis.from_pool(pool)

        self.mongo_client = MongoClient(self.config.mongo_uri)

        self._load_accounts_configs()


    async def stop(self):
        pass


    async def periodic_reload_account_from_db(self):
        while True:
            await asyncio.sleep(60)
            self._load_accounts_configs()


    # async def periodic_reload_instrument_refdata_from_db(self):
    #     while True:
    #         await asyncio.sleep(60 * 30)
    #         self._load_instrument_refdata()


    def _load_accounts_configs(self):

        is_error = False
        err_symbols = []
        err_accounts = []

        instruments_jsons = self._query_mongo(collection_name='refdata_instrument_basic')
        logger.info(f"Loading {len(instruments_jsons)} instruments")
        instruments = []
        for instrument_json in instruments_jsons:
            if "instrument_type" in instrument_json:
                if instrument_json["instrument_type"] in _inst_type_map:
                    instrument_json["instrument_type"] = _inst_type_map[instrument_json["instrument_type"]]
                else:
                    logger.warning(f"Unknown instrument type {instrument_json['instrument_type']}")
            if "instrument_id" not in instrument_json:
                instrument_json["instrument_id"] = instrument_json["instrument_code"]
            try:
                _instrument = common.InstrumentRefData().from_dict(instrument_json)
                _try_serialized_bytes = _instrument.__bytes__()
                instruments.append(_instrument)
            except Exception as e:
                logger.error(f"Error loading instrument refdata: {e}")
                is_error = True
                err_symbols.append(instrument_json["instrument_id"])

        if is_error:
            logger.error(f"{len(err_symbols)} errors while loading instruments")
            logger.error(f"Invalid symbol refdata found for : {err_symbols}")

        _account_to_omsid = self._query_account_omsid_dict()
        logger.info(f"Loading oms managed accounts: {len(_account_to_omsid)} entries")

        _account_to_routes = self._query_account_route_dict()
        logger.info(f"Loading account routes: {len(_account_to_routes)} accounts")

        _gw_key_to_config = self._query_gw_config_dict()
        logger.info(f"Loading gw configs: {len(_gw_key_to_config)} gw configs")

        _account_to_config: dict[int, ods.AccountTradingConfig] = self._get_account_config_dict(
            account_to_routes=_account_to_routes,
            account_to_omsid=_account_to_omsid,
            gw_key_to_config=_gw_key_to_config,
            instruments=instruments
        )

        self.account_config_dict = _account_to_config
        self.instrument_refdata = {refdata.instrument_id: refdata for refdata in instruments}

    def query_account_balance(self, request: tqrpc_ods.OmsQueryAccountRequest) \
            -> tqrpc_ods.OmsAccountResponse:
        query_gw = request.query_gw
        account_id = request.account_id

        if query_gw:
            gw_key = self._query_account_route(account_id).gw_key
            gw_balances: list[exch_gw.PositionReport] = self._query_account_balance_gw(gw_key=gw_key, account_id=account_id)
            if gw_balances:
                converted_balances: list[oms.Position] = [
                    self._convert_gw_position_report_to_oms_balance(gw_balance)
                    for gw_balance in gw_balances
                ]
            else:
                converted_balances = []
            return tqrpc_ods.OmsAccountResponse(
                account_id=account_id,
                account_balance_entries=converted_balances
            )
        else:
            # query oms redis
            oms_id = self._get_oms_id(account_id)
            oms_balances: list[oms.Position] = self.query_oms_balances(oms_id=oms_id, account_id=account_id)
            return tqrpc_ods.OmsAccountResponse(
                account_id=account_id,
                account_balance_entries=oms_balances
            )



    def query_order_details(self, request: tqrpc_ods.OmsQueryOrderDetailRequest) \
            -> tqrpc_ods.OmsOrderDetailResponse:
        order_refs = request.order_refs
        query_gw = request.query_gw

        if query_gw: # query gw
            gw_key = self._query_account_route(account_id=0).gw_key
            gw_orders: list[exch_gw.OrderReport] = self._query_orders_gw(gw_key=gw_key, order_refs=order_refs)
            converted_orders: list[oms.Order] = [
                self._convert_gw_order_to_oms_order(gw_order)
                for gw_order in gw_orders
            ]
            return tqrpc_ods.OmsOrderDetailResponse(
                orders=converted_orders
            )
        else: # query oms
            oms_id = self._query_oms_id()


    def query_open_orders(self, request: tqrpc_ods.OmsQueryOpenOrderRequest) \
            -> tqrpc_ods.OmsOrderDetailResponse:

        account_id = request.account_id
        oms_id = self._get_oms_id(account_id)
        open_orders: list[oms.Order] = self._query_open_orders(oms_id=oms_id, account_id=account_id)
        return tqrpc_ods.OmsOrderDetailResponse(
            orders=open_orders
        )

    def query_instrument_refdata(self, request: tqrpc_ods.OmsQueryInstrumentRefdataRequest) \
            -> tqrpc_ods.OmsQueryInstrumentRefdataResponse:
        logger.info(f"Query instrument refdata: {request}")
        account_id = request.account_id
        instruments = request.instruments

        # load cached data
        if account_id:
            if account_id not in self.account_config_dict:
                return tqrpc_ods.OmsQueryInstrumentRefdataResponse()
            _refdata = self.account_config_dict.get(account_id).supported_instruments
            # filter by instrument ids provided
            if instruments:
                instruments_set = set(instruments)
                _refdata = [rd for rd in _refdata if rd.instrument_id in instruments_set]
        else:
            _refdata = [self.instrument_refdata[id] for id in instruments]

        # sort by instrument_id
        #_refdata.sort(key=lambda x: x.instrument_id)

        # TODO: need pagination
        return tqrpc_ods.OmsQueryInstrumentRefdataResponse(
            instrument_refdata=_refdata
        )


    def query_instruments(self, request: tqrpc_ods.OmsQueryInstrumentRequest) \
            -> tqrpc_ods.OmsQueryInstrumentResponse:
        logger.info(f"Query instruments: {request}")
        account_id = request.account_id

        if account_id:
            if account_id not in self.account_config_dict:
                return tqrpc_ods.OmsQueryInstrumentResponse()
            _refdata = self.account_config_dict.get(account_id).supported_instruments
            instrument_ids = [rd.instrument_id for rd in _refdata]
        else:
            instrument_ids = list(self.instrument_refdata.keys())

        return tqrpc_ods.OmsQueryInstrumentResponse(
            instrument_ids=instrument_ids
        )



    def query_account_trading_config(self, request: tqrpc_ods.OmsQueryAccountTradingConfigRequest) \
            -> tqrpc_ods.OmsQueryAccountTradingConfigResponse:
        account_ids = request.account_ids

        _account_omsid_dict = self._query_account_omsid_dict()
        _account_to_routes = self._query_account_route_dict()
        _gw_key_to_config = self._query_gw_config_dict()

        account_configs: list[ods.AccountTradingConfig] = []
        for account_id in account_ids:

            account_config = ods.AccountTradingConfig()
            account_config.account_id = account_id
            if account_id not in _account_omsid_dict:
                logger.info(f"Account {account_id} not found in oms_config")
                continue
            account_config.oms_id = _account_omsid_dict.get(account_id, None)
            account_config.account_route = _account_to_routes.get(account_id, None)
            gw_key = account_config.account_route.gw_key
            account_config.gw_config = _gw_key_to_config.get(gw_key, None)
            account_configs.append(account_config)

        return tqrpc_ods.OmsQueryAccountTradingConfigResponse(
            account_trading_configs=account_configs
        )


    def query_all_account_trading_config(self, req: common.DummyRequest) \
            -> tqrpc_ods.OmsQueryAccountTradingConfigResponse:
        account_ids = list(self.account_config_dict.keys())
        _req = tqrpc_ods.OmsQueryAccountTradingConfigRequest(
            account_ids=account_ids)
        return self.query_account_trading_config(_req)

    def query_oms_config(self, request: tqrpc_ods.OmsQueryOmsConfigRequest) \
            -> tqrpc_ods.OmsQueryOmsConfigResponse:
        logger.info(f"Query oms config: {request}")
        oms_id = request.oms_id
        oms_configs = self._query_mongo(collection_name='oms_config', query={'oms_id': oms_id})
        if len(oms_configs) == 0:
            return tqrpc_ods.OmsQueryOmsConfigResponse()
        else:
            oms_config: ods.OmsConfigEntry = ods.OmsConfigEntry(**oms_configs[0])
            return tqrpc_ods.OmsQueryOmsConfigResponse(
                oms_config=oms_config
            )


    def query_client_config(self, request: tqrpc_ods.OmsQueryClientConfigRequest) \
            -> tqrpc_ods.OmsQueryClientConfigResponse:
        logger.info(f"Query client config: {request}")
        account_ids = request.trading_account_ids
        account_endpoint_mapping = {}
        order_update_endpoints = []
        balance_update_endpoints = []

        _account_omsid_dict = self._query_account_omsid_dict()
        for account_id in account_ids:
            oms_id = _account_omsid_dict.get(account_id, None)
            if oms_id:
                oms_rpc_endpoint, oms_ou_endpoint, oms_bu_endpoint = self._get_oms_endpoints(oms_id)
                account_endpoint_mapping[account_id] = oms_rpc_endpoint
                order_update_endpoints.append(oms_ou_endpoint)
                balance_update_endpoints.append(oms_bu_endpoint)

        for account_id in request.order_subscribed_account_ids:
            oms_id = _account_omsid_dict.get(account_id, None)
            if oms_id:
                _, oms_ou_endpoint, _ = self._get_oms_endpoints(oms_id)
                order_update_endpoints.append(oms_ou_endpoint)

        for account_id in request.position_subscribed_account_ids:
            oms_id = _account_omsid_dict.get(account_id, None)
            if oms_id:
                _, _, oms_bu_endpoint = self._get_oms_endpoints(oms_id)
                balance_update_endpoints.append(oms_bu_endpoint + ".*")

        return tqrpc_ods.OmsQueryClientConfigResponse(
            oms_service_endpoints=account_endpoint_mapping,
            oms_orderupdates_endpoints=order_update_endpoints,
            oms_balanceupdates_endpoints=balance_update_endpoints
        )


    def query_app_client_config(self, req: common.DummyRequest) \
            -> tqrpc_ods.OmsQueryClientConfigResponse:
        account_id_2_oms_id = self._query_account_omsid_dict()
        account_ids = list(account_id_2_oms_id.keys())

        _req = tqrpc_ods.OmsQueryClientConfigRequest(
            trading_account_ids=account_ids)

        return self.query_client_config(_req)


    def query_rtmd_subscription_list(self, request: tqrpc_ods.OdsRtmdSubQueryRequest) \
        -> tqrpc_ods.OdsRtmdSubQueryResponse:
        logger.info(f"Query rtmd subscription list: {request}")
        exch_name = request.exch_name
        instance_id = request.rtmd_instance_id

        exch_sub_table = self._query_mongo(collection_name='rtmd_subscriptions', query={'exch_name': exch_name})
        if not exch_sub_table:
            return tqrpc_ods.OdsRtmdSubQueryResponse()
        else:
            exch_subs = exch_sub_table[0]['subscriptions']
            instrument_ids = [instrument_id for instrument_id, instance in exch_subs.items()
                              if instance == instance_id]

            refdata_items = [self.instrument_refdata[instrument_id] for instrument_id in instrument_ids]

            exch_instrument_ids = [refdata.instrument_id_exchange for refdata in refdata_items]

            return tqrpc_ods.OdsRtmdSubQueryResponse(
                exch_instrument_ids=exch_instrument_ids,
                exch_name=exch_name,
                rtmd_instance_id=instance_id
            )



    _CHANNEL_NOT_PRESENT_MSG = tqrpc_ods.OdsRtmdChannelQueryResponse(
        channel_present=False
    )

    _DATA_NOT_PRESENT_MSG = tqrpc_ods.OdsRtmdDataQueryResponse(
        data_present=False
    )

    async def query_rtmd_channel(self, query_request: tqrpc_ods.OdsRtmdQueryRequest) \
            -> tqrpc_ods.OdsRtmdChannelQueryResponse:
        logger.info(f"Query rtmd channel: {query_request}")
        instrument_id= query_request.instrument_id
        if instrument_id in self.instrument_refdata:
            refdata = self.instrument_refdata[instrument_id]
            instrument_exch_id = refdata.instrument_id_exchange
            exchange_name = refdata.exch_name
            channel_key = f"rtmd:subject:{exchange_name}:{instrument_exch_id}"
            channel_bytes = await self._query_redis(channel_key)
            if channel_bytes:
                channel = channel_bytes.decode('utf-8')
                return tqrpc_ods.OdsRtmdChannelQueryResponse(
                    rtmd_channel=channel,
                    channel_present=True
                )
        return self._CHANNEL_NOT_PRESENT_MSG

    async def query_rtmd_orderbook(self, query_request: tqrpc_ods.OdsRtmdQueryRequest)\
            -> tqrpc_ods.OdsRtmdDataQueryResponse:
        logger.info(f"Query rtmd orderbook: {query_request}")
        instrument_id = query_request.instrument_id
        if instrument_id in self.instrument_refdata:
            refdata = self.instrument_refdata[instrument_id]
            instrument_exch_id = refdata.instrument_id_exchange
            exchange_name = refdata.exch_name
            data_key = f"rtmd:data:{exchange_name}:{instrument_exch_id}"
            data = await self._query_redis(data_key)
            if data:
                return tqrpc_ods.OdsRtmdDataQueryResponse(
                    data_present=True,
                    rtmd_orderbook_bytes=data
                )

        return self._DATA_NOT_PRESENT_MSG


    async def query_account_summary(self, query_request: tqrpc_ods.OdsQueryAccountSummaryRequest) \
            -> tqrpc_ods.OdsAccountSummaryResponse:
        account_id = query_request.account_id
        account_config = self.account_config_dict.get(account_id)
        if not account_config:
            raise ValueError(f"Account {account_id} not found in account config")
        gw_config = account_config.gw_config

        rpc_endpoint = gw_config.extra_rpc_endpoint if gw_config.extra_rpc_endpoint else gw_config.rpc_endpoint

        gw_query_req = rpc_gw. ExchQueryAccountRequest()

        resp_data, is_error = await rpc(nc=self.nc, subject=rpc_endpoint,
                         method="QueryAccountSummary", payload=gw_query_req, timeout_in_secs=_GW_RPC_DEFAULT_TIMEOUT,
                         retry_on_timeout=False)

        if not is_error and resp_data is not None:
            resp = rpc_gw.ExchAccountSummaryResponse().parse(resp_data)
            return tqrpc_ods.OdsAccountSummaryResponse(
                account_summary=resp .account_summary)
        else:
            logger.error(f"Error querying account summary: {resp_data}")
            return tqrpc_ods.OdsAccountSummaryResponse()


    async def query_account_setting(self, request: tqrpc_ods.OdsQueryAccountSettingRequest) \
            -> tqrpc_ods.OdsAccountSettingResponse:
        account_id = request.account_id
        instrument_id = request.instrument_id
        account_config = self.account_config_dict.get(account_id)
        if not account_config:
            raise ValueError(f"Account {account_id} not found in account config")

        if instrument_id not in self.instrument_refdata:
            raise ValueError(f"Instrument {instrument_id} not found in refdata")

        refdata = self.instrument_refdata[instrument_id]
        if  refdata.exch_name != account_config.gw_config.exch_name:
            raise ValueError(f"Instrument {instrument_id} not supported for account {account_id}")

        gw_config = account_config.gw_config

        rpc_endpoint = gw_config.extra_rpc_endpoint if gw_config.extra_rpc_endpoint else gw_config.rpc_endpoint

        gw_query_req = rpc_gw.ExchQueryAccountSettingRequest()
        gw_query_req.exch_account_id = account_config.account_route.exch_account_id
        gw_query_req.instrument = self.instrument_refdata[request.instrument_id].instrument_id_exchange

        resp_data, is_error = await rpc(nc=self.nc, subject=rpc_endpoint,
                            method="QueryAccountSetting", payload=gw_query_req, timeout_in_secs=_GW_RPC_DEFAULT_TIMEOUT,
                                        retry_on_timeout=False)

        if not is_error and resp_data is not None:
            resp = rpc_gw.ExchAccountSettingResponse().parse(resp_data)
            return tqrpc_ods.OdsAccountSettingResponse(
                account_setting_entry=resp.account_setting_entry)
        else:
            logger.error(f"Error querying account setting: {resp_data}")
            return tqrpc_ods.OdsAccountSettingResponse()


    async def update_account_setting(self, request: tqrpc_ods.OdsUpdateAccountSettingRequest) \
            -> tqrpc_ods.OdsUpdateAccountSettingResponse:
        account_id = request.account_id
        account_config = self.account_config_dict.get(account_id)
        if not account_config:
            raise ValueError(f"Account {account_id} not found in account config")

        account_setting_entry = request.account_setting_entry
        instrument_id = account_setting_entry.instrument
        if instrument_id not in self.instrument_refdata:
            raise ValueError(f"Instrument {instrument_id} not found in refdata")
        instrument_id_exch = self.instrument_refdata[instrument_id].instrument_id_exchange
        account_setting_entry.instrument = instrument_id_exch # update to exchange instrument id

        gw_config = account_config.gw_config
        rpc_endpoint = gw_config.extra_rpc_endpoint if gw_config.extra_rpc_endpoint else gw_config.rpc_endpoint

        gw_update_req = rpc_gw.ExchAccountSettingRequest()
        gw_update_req .account_setting_entry = account_setting_entry

        resp_data, is_error = await rpc(nc=self.nc, subject=rpc_endpoint,
                            method="UpdateAccountSetting", payload=gw_update_req, timeout_in_secs=_GW_RPC_DEFAULT_TIMEOUT,
                                        retry_on_timeout=False)

        if not is_error and resp_data is not None:
            resp = rpc_gw.ExchAccountSettingUpdateResponse().parse(resp_data)
            return tqrpc_ods.OdsUpdateAccountSettingResponse(
                margin_mode_changed=resp.margin_mode_changed,
                leverage_changed=resp.leverage_changed,
                err_msg=resp.err_msg
            )
        else:
            logger.error(f"Error updating account setting: {resp_data}")
            return tqrpc_ods.OdsUpdateAccountSettingResponse(
                margin_mode_changed=False,
                leverage_changed=False,
                err_msg=str(resp_data)
            )






    def gen_order_id(self, req: common.DummyRequest) -> str:
        _id = next(self.id_gen)
        return str(_id)

    #def query_online_oms(self, req: common.DummyRequest) ->:

    def _get_oms_id(self, account_id: int) -> str:
        if account_id in self.account_config_dict:
            return self.account_config_dict[account_id].oms_id
        return None


    def _query_account_route_dict(self) -> dict[int, ods.OmsRouteEntry]:
        account_routes_jsons = self._query_mongo(collection_name='refdata_account_mapping')
        account_routes = []
        for account_route_json in account_routes_jsons:
            # compatibility with typo
            if 'accound_id' in account_route_json:
                account_route_json['account_id'] = account_route_json.pop('accound_id')
            account_routes.append(ods.OmsRouteEntry().from_dict(account_route_json))

        _account_to_routes = {}
        for account_route in account_routes:
            account_id = account_route.account_id
            _account_to_routes[account_id] = account_route

        return _account_to_routes


    def _query_gw_config_dict(self) -> dict[str, ods.GwConfigEntry]:
        gw_configs_jsons = self._query_mongo(collection_name='refdata_gw_config')
        gw_configs = [ods.GwConfigEntry().from_dict(gw_config_json)
                        for gw_config_json in gw_configs_jsons]

        _gw_key_to_config = {}
        for gw_config in gw_configs:
            gw_key = gw_config.gw_key
            _gw_key_to_config[gw_key] = gw_config

        return _gw_key_to_config


    def _query_account_omsid_dict(self) -> dict[int, str]:
        oms_configs_jsons = self._query_mongo(collection_name='oms_config')
        oms_configs = [ods.OmsConfigEntry().from_dict(oms_config_json)
                       for oms_config_json in oms_configs_jsons]

        _account_to_omsid = {}
        for oms_config in oms_configs:
            for account_id in oms_config.managed_account_ids:
                # TODO: check duplicate account id in different oms
                _account_to_omsid[account_id] = oms_config.oms_id

        return _account_to_omsid

    def _get_account_config_dict(self, account_to_routes: dict[int, ods.OmsRouteEntry],
                                account_to_omsid: dict[int, str],
                                gw_key_to_config: dict[str, ods.GwConfigEntry],
                                 instruments: list[common.InstrumentRefData]) -> dict[int, ods.AccountTradingConfig]:
        _account_to_config: dict[int, ods.AccountTradingConfig] = {}
        for (account_id, account_route) in account_to_routes.items():
            account_trading_config = ods.AccountTradingConfig()
            account_trading_config.account_id = account_id
            if account_id not in account_to_omsid:
                logger.info(f"Account {account_id} not found in oms_config")
                continue
            account_trading_config.oms_id = account_to_omsid[account_id]
            account_trading_config.account_route = account_route
            gw_key = account_route.gw_key
            gw_config = gw_key_to_config[gw_key]
            account_trading_config.gw_config = gw_config

            supported_instruments = [refdata for refdata in instruments
                                     if refdata.exch_name == gw_config.exch_name]

            account_trading_config.supported_instruments = supported_instruments
            _account_to_config[account_id] = account_trading_config

        return _account_to_config

    def _get_oms_endpoints(self, oms_id: str):
        rpc_endpoint = f"tq.oms.service.{oms_id}.rpc"
        order_update_endpoint = f"tq.oms.service.{oms_id}.order_update"
        balance_update_endpoint = f"tq.oms.service.{oms_id}.balance_update"

        return rpc_endpoint, order_update_endpoint, balance_update_endpoint

    async def _rpc_gw(self, rpc_path: str, method_name:str, request):
        pass

    def _query_mongo(self, collection_name: str, query:dict[str, any]=None) -> list[dict]:
        collection = self.mongo_client[self.config.mongo_db_name][collection_name]
        if query:
            res = list(collection.find(query))
        else:
            res = list(collection.find())

        # remove _id in mongo result
        for item in res:
            item.pop('_id', None)

        return res


    async def _query_redis(self, key: str) -> bytes:
        data = await self.redis_client.get(key)
        return data


def main():
    import os
    from tqrpc_utils.config_utils import try_load_config
    import  tqrpc_utils.rpc_utils as svc_utils

    config: ODSConfig = try_load_config(ODSConfig)
    print(f"config: {config}")

    ods_obj = ODSServer(config=config)

    ods_service: svc_utils.TQRpcService = svc_utils.RpcServiceBuilder(
        service_namespace="tq.ods",
        service_name="tq-ods-prod",
        rpc_endpoint="tq.ods.rpc",
        nc_url=config.nats_url,
        nc_task_queue_name="tq_ods_worker"
     ).register_rpc_method(ods_obj.query_client_config, concurrency=3) \
        .register_rpc_method(ods_obj.query_account_trading_config, concurrency=3) \
        .register_rpc_method(ods_obj.query_all_account_trading_config, concurrency=2) \
        .register_rpc_method(ods_obj.query_instrument_refdata, concurrency=3) \
        .register_rpc_method(ods_obj.query_instruments, concurrency=3) \
        .register_rpc_method(ods_obj.query_rtmd_channel, concurrency=5) \
        .register_rpc_method(ods_obj.query_rtmd_orderbook, concurrency=5) \
        .register_rpc_method(ods_obj.gen_order_id, concurrency=2) \
        .register_rpc_method(ods_obj.query_app_client_config, concurrency=2) \
        .register_rpc_method(ods_obj.query_account_summary, concurrency=3) \
        .register_rpc_method(ods_obj.query_account_setting, concurrency=2) \
        .register_rpc_method(ods_obj.update_account_setting, concurrency=2) \
        .register_rpc_method(ods_obj.query_rtmd_subscription_list, concurrency=3) \
        .register_start_method(ods_obj.start) \
        .register_stop_method(ods_obj.stop) \
        .register_task_method(ods_obj.periodic_reload_account_from_db) \
        .build()

    runner = svc_utils.create_runner(shutdown_cb=ods_service.stop)

    runner.run_until_complete(ods_service.start())
    tasks = ods_service.get_async_tasks()
    for task in tasks:
        runner.create_task(task)

    async def reload(msg):
        logger.info("Reloading refdata / configs due to reload request")
        runner.run_in_executor(None, ods_obj._load_accounts_configs)

    runner.run_until_complete(ods_service.nc.subscribe("tq.reload.ods", cb=reload))

    # wait for any of the tasks to be finished(most likely due to exception)
    runner.run_until_complete(asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED))

if __name__ == "__main__":
    main()