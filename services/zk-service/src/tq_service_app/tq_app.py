import json
import logging

import requests
import uvicorn
import nats
from dataclasses import dataclass
from betterproto import Casing
from fastapi import FastAPI, HTTPException
from pymongo import MongoClient
from typing import List, Dict, Union
from pydantic import BaseModel
from datetime import datetime
import asyncio
import redis
import uuid

from loguru import logger

import zk_datamodel.common as common
import zk_datamodel.tqrpc_oms as oms_rpc
import zk_datamodel.oms as oms
import zk_datamodel.strategy as strat_pb
from zk_datamodel import ods
from tq_service_app.pnl_fetcher import BinancePnlFetcher, ParadexPnlFetcher, VertexPnlFetcher
from tq_service_app.utils import calc_open_order_stats, fetch_balance, query_accounts_pipeline, get_followed_orders_key
from zk_client.tqclient import TQClient, TQClientConfig, create_client

@dataclass
class AppConfig:
    nats_url: str = None
    mongo_url_base: str = None
    api_port: int = None
    account_ids: list[str] = None
    id_gen_instance: int = None
    redis_host: str = None
    redis_port: str = None
    redis_password: str = None
    redis_namespace: str = None
    oms_db_name: str = None
    strat_db_name: str = None
    data_db_name: str = None

from tqrpc_utils.config_utils import try_load_config

app_conf: AppConfig = try_load_config(AppConfig)

app = FastAPI()

client = MongoClient(app_conf.mongo_url_base)
db = client[app_conf.strat_db_name]
strategy_col = db["strategy"]
execution_col = db["execution"]
order_col = db["strategy_order"]
cancel_col = db["strategy_cancel"]
log_col = db["strategy_log"]

oms_db = client[app_conf.oms_db_name]

account_mapping_col = oms_db['refdata_account_mapping']

_all_account_ids = [int(_accnt_mapping['accound_id']) for _accnt_mapping in account_mapping_col.find()]

logging.info(f"all account ids loaded: {_all_account_ids}")

data_db = client[app_conf.data_db_name]
trade_col = data_db['trade']

redis_client = redis.Redis(
    host=app_conf.redis_host,
    port=app_conf.redis_port,
    password=app_conf.redis_password,
    db=0
)
def store_followed_order(oms_id: str, account_id: Union[str, int], order_id: Union[str, int]):
    key = get_followed_orders_key(app_conf.redis_namespace, oms_id, account_id, order_id)
    redis_client.set(key, 1)
    redis_client.expire(name=key, time=60*5)

_nc: nats.NATS = None
async def connect_nats():
    global _nc
    if _nc:
        return _nc
    _nc = await nats.connect(servers=[app_conf.nats_url])
    return _nc

_account_oms_map: dict[int, str] = {}


_tq_client: TQClient = None
async def load_tq_client(force_reload=False):
    global _tq_client
    if _tq_client and not force_reload:
        return _tq_client
    import socket
    if (app_conf.account_ids is None
            or len(app_conf.account_ids) == 0
            or app_conf.account_ids[0] == "*"
            or app_conf.account_ids[0] == ""):
        account_ids = _all_account_ids
    elif isinstance(app_conf.account_ids, list):
        account_ids = [int(x) for x in app_conf.account_ids]
    elif app_conf.account_ids is not None or app_conf .account_ids != "":
        account_ids = [int(app_conf.account_ids)] # just in case a single string is provided
    else:
        account_ids = _all_account_ids

    logging.info(f"creating client for account ids: {account_ids}")
    temp_tq_client = await create_client(
        nats_url=app_conf.nats_url,
        account_ids=account_ids,
        source_id=f"APP@{socket.gethostname()}", timeout=10)

    await temp_tq_client.start(init_timeout=10)
    await temp_tq_client.subscribe_order_update(handle_order_update)

    _tq_client = temp_tq_client


    # TODO: refactor this
    _account_details: dict[int, ods.AccountTradingConfig] = _tq_client.get_account_details()
    for _account_id in _account_details:
        _accnt_detail = _account_details[_account_id]
        _account_oms_map[_account_id] = _accnt_detail.oms_id
    return _tq_client

async def handle_order_update(order_update: oms.OrderUpdateEvent):
    oms_id = _account_oms_map.get(order_update.account_id)
    if oms_id is None:
        return

    redis_key = get_followed_orders_key(app_conf.redis_namespace, oms_id, order_update.account_id, order_update.order_id)
    if redis_client.exists(redis_key):
        subject = f'tq_app.order_update.{oms_id}'
        await _tq_client.nc.publish(subject=subject, payload=order_update.to_json().encode('utf-8'))

        if (order_update.exec_message.exec_type in [
                oms.ExecType.EXEC_TYPE_CANCEL, 
                oms.ExecType.EXEC_TYPE_PLACING_ORDER
            ]
            or order_update.order_snapshot.order_status in [
                oms.OrderStatus.ORDER_STATUS_FILLED, 
                oms.OrderStatus.ORDER_STATUS_CANCELLED, 
                oms.OrderStatus.ORDER_STATUS_REJECTED
            ]
        ):  
            redis_client.delete(redis_key)


# Define data models here
#from tqrpc_lib.strategy import *
class Strategy(BaseModel):
    strategy_key: str
    strategy_desc: str
    strategy_script_file_name: str
    current_execution_id: Union[str, None]
    account_ids: list[int]
    symbols: list[str]
    config: dict
    tq_config: dict

class ExecutionCreateRequest(BaseModel):
    strategy_key: str
    execution_id: str

class StrategyExecution(BaseModel):
    start_ts: int = None
    end_ts: int = None
    strategy_key: str = None
    execution_id: str = None
    config: str = None

    strategy_script_file_name: str = None
    account_ids: list[int] = None
    symbols: list[str] = None
    config: dict = None
    tq_config: dict = None


class OMSOrderRequest(BaseModel):
    account_id: int = None
    symbol: str = None
    price: float = None
    qty: float = None
    side: str = None

class OMSPanicRequest(BaseModel):
    oms_id: str = None
    account_id: str = None
    reason: str = None
    source_id: str = None


# class StrategyOrder(BaseModel):
#     timestamp: int
#     strategy_key: str
#     execution_id: str
#     order_request: dict

# class StrategyCancel(BaseModel):
#     timestamp: int
#     strategy_key: str
#     execution_id: str
#     cancel_request: dict

# class StrategyLog(BaseModel):
#     timestamp: int
#     strategy_key: str
#     execution_id: str
#     log: str
#     log_type: str


def _OK(data: Union[dict, list, str]):
    return {
        "success": True,
        "data": data
    }

def _FAILED(reason: str):
    return {
        "success": False,
        "data": reason
    }

def has_account_oms_error(oms_id: str, account_id: Union[int, str]):
    target_oms_id = _account_oms_map.get(int(account_id))
    if target_oms_id is None:
        return _FAILED(f'Account_id {account_id} is not supported')
    if oms_id != target_oms_id:
        return _FAILED(f'Account_id {account_id} and oms_id {oms_id} do not match')

@app.get("/v1/test")
async def test():
    return "hello there"

# Strategy API
@app.post("/v1/tqstrat/strategy")
async def create_strategy(strategy: Strategy):
    # Check if strategy_key already exists
    if strategy_col.count_documents({"strategy_key": strategy.strategy_key}) != 0:
        # return JSONResponse(status_code=400,
        #                     content={"message": f"Strategy with strategy_key {strategy.strategy_key} already exists"})
        raise HTTPException(status_code=400,
                            detail=f"Strategy with strategy_key {strategy.strategy_key} already exists")

    # Insert strategy document in MongoDB
    strategy_col.insert_one(strategy.dict())

    # Return success response
    return _OK("Strategy created successfully")

@app.put("/v1/tqstrat/strategy")
async def update_strategy(strategy: Strategy):
    strategy_key = strategy.strategy_key
    if not strategy_key:
        raise HTTPException(status_code=400, detail="Missing strategy_key")
    result = strategy_col.update_one(
        {"strategy_key": strategy_key},
        {"$set": strategy},
    )
    if result.matched_count == 0:
        raise HTTPException(status_code=404, detail="Strategy not found")
    return {"message": "Strategy updated successfully"}

# Strategy Instance API
@app.post("/v1/tqstrat/strategy-inst")
async def create_strategy_instance(execution_req: ExecutionCreateRequest):
    # Check if the strategy exists
    strategy_filter = {"strategy_key": execution_req.strategy_key}
    strategy_dict  = strategy_col.find_one(strategy_filter)
    #strategy: Strategy = Strategy(**strategy_dict)
    if strategy_dict is None:
        raise HTTPException(status_code=400, detail="Strategy does not exist")

    if strategy_dict['current_execution_id']:
        raise HTTPException(status_code=400,
            detail=f"strategy {execution_req.strategy_key} already started(execution_id={strategy_dict['current_execution_id']})")

    # Copy the config from the strategy to the execution
    execution = StrategyExecution()
    execution.strategy_key = execution_req.strategy_key
    execution.execution_id = execution_req.execution_id
    execution.config = strategy_dict['config']
    execution.strategy_script_file_name = strategy_dict['strategy_script_file_name']
    execution.tq_config = strategy_dict['tq_config']
    execution.account_ids = strategy_dict['account_ids']
    execution.symbols = strategy_dict['symbols']

    # Mark the start_ts to current ts
    execution.start_ts = int(datetime.now().timestamp()*1000)

    # Insert the execution to the database
    result1 = execution_col.insert_one(execution.dict())
    result2 = strategy_col.update_one(filter=strategy_filter,
                            update={'$set': {'current_execution_id': execution.execution_id}})


    # Return the execution_id
    return _OK(execution.dict())

@app.get("/v1/tqstrat/strategy-inst")
async def query_strategy_instance(strategy_key: str, latest: bool = False):
    filter_query = {"strategy_key": strategy_key}
    sort_query = [("start_ts", -1)] if latest else None

    strategy_instances = []
    cursor = execution_col.find(filter_query, sort=sort_query)

    for doc in cursor:
        strategy_instance = StrategyExecution(
            **doc
        )
        if latest:
            return strategy_instance
        else:
            strategy_instances.append(strategy_instance)

    return strategy_instances

@app.delete("/v1/tqstrat/strategy-inst")
async def forcestop_strategy_instance(strategy_key: str):
    if not strategy_key:
        raise HTTPException(status_code=400, detail="Missing strategy_key")
    result = execution_col.update_many(
        {"strategy_key": strategy_key, "end_ts": None},
        {"$set": {"end_ts": int(datetime.now().timestamp() * 1000)}},
    )
    # current_executions = execution_col.find_one(
    #     {"strategy_key": strategy_key, "end_ts": None},
    #     sort=[("start_ts", -1)]
    # )
    # if current_executions:
    strategy_col.update_one(
        {"strategy_key": strategy_key},
        {"$set": {"current_execution_id": None}})

    return _OK(f"{result.modified_count} instances stopped")

# Strategy Logs API
@app.get("/v1/tqstrat/logs")
async def query_strategy_logs(strategy_key: str, execution_id: Union[None, str]=None, page_no: int = 1, limit: int = 100):

    # Construct MongoDB query based on query parameters
    mongo_query = {"strategy_id": strategy_key}
    if execution_id:
        mongo_query["execution_id"] = execution_id

    # Query logs from MongoDB
    logs = log_col.find(mongo_query).sort("timestamp", -1).skip((page_no - 1) * limit).limit(limit)

    # Format logs as list of dictionaries and return
    result = []
    for log in logs:
        strat_log = strat_pb.StrategyLogEvent().from_dict(log)
        result.append(strat_log)
    return result

# Strategy Orders API
@app.get("/v1/tqstrat/orders")
async def query_strategy_orders(strategy_key: str, execution_id: Union[None, str]=None, page_no: int = 1, limit: int = 100):
    if not strategy_key:
        raise HTTPException(status_code=400, detail="Missing strategy_key")

    # Construct MongoDB query based on query parameters
    mongo_query = {"strategy_key": strategy_key}
    if execution_id:
        mongo_query["execution_id"] = execution_id

    skip = (page_no - 1) * limit
    orders = order_col.find(mongo_query).sort("timestamp", -1).skip(skip).limit(limit)
    result = []
    for order in orders:
        strat_order = strat_pb.StrategyOrderEvent().from_dict(order)
        result.append(strat_order)
    return orders

# Accounts
@app.get('/v1/oms/accounts')
async def query_accounts():
    accounts = list(account_mapping_col.aggregate(query_accounts_pipeline))
    result = [
        {**account, "oms_id": _account_oms_map[account['account_id']]} 
        for account in accounts 
        if account['account_id'] in _account_oms_map.keys()
    ]
    return result

# Define route for querying open orders
@app.get('/v1/oms/open_orders')
async def query_open_orders(oms_id:str, account_id: str):
    nc = await connect_nats()
    subject = f"tq.oms.service.{oms_id}.rpc"
    headers = {"rpc_method": "QueryOpenOrders"}
    query_request = oms_rpc.OmsQueryOpenOrderRequest()
    query_request.account_id=int(account_id)
    query_request.query_gw = False
    resp = await nc.request(subject, bytes(query_request), headers=headers, timeout=2)
    query_resp = oms_rpc.OmsOrderDetailResponse().parse(resp.data)

    resp = query_resp.to_dict(casing=Casing.SNAKE)
    resp['stats'] = calc_open_order_stats(query_resp.orders)

    return resp

# Define route for querying balances
@app.get('/v1/oms/balances')
async def query_balances(oms_id:str, account_id: str):
    nc = await connect_nats()
    balances = await fetch_balance(nc, oms_id, int(account_id))

    return balances.to_dict(casing=Casing.SNAKE)

@app.get('/v1/oms/pnls')
async def query_pnls(oms_id: str, account_id: str):
    # account_oms_error = has_account_oms_error(oms_id, account_id)
    # if account_oms_error is not None:
    #     return account_oms_error
    #
    # if int(account_id) in [7702]:
    #     pnl_fetcher = VertexPnlFetcher()
    #     pnls = await pnl_fetcher.fetch_pnl(int(account_id))
    #     return _OK(pnls)
    # if int(account_id) in [7632, 901]:
    #     nc = await connect_nats()
    #     pnl_fetcher = BinancePnlFetcher(nc)
    #     pnls = await pnl_fetcher.fetch_pnl(oms_id, int(account_id))
    #     return _OK(pnls)
    # if int(account_id) in [226]:
    #     nc = await connect_nats()
    #     pnl_fetcher = ParadexPnlFetcher(nc)
    #     pnls = await pnl_fetcher.fetch_pnl(oms_id, int(account_id))
    #     return _OK(pnls)

    return _FAILED(f'PnL data is not supported for account {account_id}')

@app.get('/v1/oms/trades')
def query_trades(order_id: str):
    trades = list(trade_col.find({'order_id': order_id}, {"_id":0}))
    return {"trades": trades}

@app.post('/v1/oms/orders/{oms_id}')
async def send_order(oms_id: str, oms_order: OMSOrderRequest):
    account_oms_error = has_account_oms_error(oms_id, oms_order.account_id)
    if account_oms_error is not None:
        return account_oms_error

    tq_client = await load_tq_client()
    order_id = await tq_client.send_order(
        account_id=int(oms_order.account_id),
        symbol=oms_order.symbol,
        price=oms_order.price,
        qty=oms_order.qty,
        side=common.BuySellType.BS_SELL
            if oms_order.side == 'sell' else common.BuySellType.BS_BUY,
    )
    store_followed_order(oms_id, oms_order.account_id, order_id)

    return _OK(f"Order sent; order_id: {order_id}")


@app.delete('/v1/oms/orders/{oms_id}/{account_id}/{order_id}')
async def cancel_order(oms_id:str, account_id:str, order_id: str):
    account_oms_error = has_account_oms_error(oms_id, account_id)
    if account_oms_error is not None:
        return account_oms_error

    tq_client = await load_tq_client()
    await tq_client.cancel(
        order_id=int(order_id),
        account_id=int(account_id))
    store_followed_order(oms_id, account_id, order_id)

    return _OK(f"Order {order_id} cancel request sent")


@app.delete('/v1/oms/orders/{oms_id}/{account_id}')
async def cancel_all_orders(oms_id:str, account_id:str):
    account_oms_error = has_account_oms_error(oms_id, account_id)
    if account_oms_error is not None:
        return account_oms_error

    tq_client = await load_tq_client()
    open_orders: list[oms.Order] = await tq_client.get_open_orders(account_id=int(account_id))
    for o in open_orders:
        await tq_client.cancel(order_id=o.order_id, account_id=int(account_id))
        store_followed_order(oms_id, account_id, o.order_id)

    return _OK(f"All({len(open_orders)}) orders cancel request sent")


@app.post('/v1/oms/panic/{oms_id}/{account_id}')
async def panic(oms_id: str, account_id: str,  panic_request: OMSPanicRequest):
    account_oms_error = has_account_oms_error(oms_id, account_id)
    if account_oms_error is not None:
        return account_oms_error

    tq_client = await load_tq_client()

    await tq_client.panic(
        account_id=int(account_id),
        reason=panic_request.reason,
        source_id=panic_request.source_id)
    return _OK(f"panic request sent")


@app.post('/v1/oms/dontpanic/{oms_id}/{account_id}')
async def panic(oms_id: str, account_id: str):
    account_oms_error = has_account_oms_error(oms_id, account_id)
    if account_oms_error is not None:
        return account_oms_error

    tq_client = await load_tq_client()

    await tq_client.dont_panic(int(account_id))
    return _OK(f"dontpanic request sent")



@app.get('/v1/oms-online')
async def query_online_oms():
    return ['oms1']


async def main():
    await load_tq_client()

    async def periodic_reload_tq_client():
        while True:
            await asyncio.sleep(60*30)
            logger.info("reloading tq client")
            try:
                await load_tq_client(force_reload=True)
            except Exception as e:
                logger.error(f"failed to reload tq client: {e}")

    #asyncio.create_task(periodic_reload_tq_client())


    async def reload(msg):
        logger.info("Reloading refdata / configs due to reload request")
        try:
            await load_tq_client(force_reload=True)
        except Exception as e:
            logger.error(f"failed to reload tq client: {e}")

    nc = await connect_nats()
    await nc.subscribe("tq.reload.app", cb=reload)

    univorn_config = uvicorn.Config(app, port=app_conf.api_port, host="0.0.0.0", proxy_headers=True, forwarded_allow_ips="*")
    server = uvicorn.Server(univorn_config)
    await server.serve()

if __name__ == "__main__":
    asyncio.run(main())
