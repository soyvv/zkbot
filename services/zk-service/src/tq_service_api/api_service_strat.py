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

from loguru import logger
from fastapi.middleware.gzip import GZipMiddleware

import zk_datamodel.strategy as strat_pb


@dataclass
class AppConfig:
    mongo_url_base: str = None
    api_port: int = None
    strat_db_name: str = None

from tqrpc_utils.config_utils import try_load_config

app_conf: AppConfig = try_load_config(AppConfig)

app = FastAPI()

app.add_middleware(GZipMiddleware, minimum_size=2000)

client = MongoClient(app_conf.mongo_url_base)
db = client[app_conf.strat_db_name]
strategy_col = db["strategy"]
execution_col = db["execution"]
order_col = db["strategy_order"]
cancel_col = db["strategy_cancel"]
log_col = db["strategy_log"]



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


@app.get("/v1/tqstrat/test")
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





async def main():


    univorn_config = uvicorn.Config(app, port=app_conf.api_port,
                                    host="0.0.0.0", proxy_headers=True, forwarded_allow_ips="*")
    server = uvicorn.Server(univorn_config)
    await server.serve()

if __name__ == "__main__":
    asyncio.run(main())
