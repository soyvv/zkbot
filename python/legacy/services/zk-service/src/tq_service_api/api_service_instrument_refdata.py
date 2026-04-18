from dataclasses import dataclass
import traceback

from betterproto import Casing
from fastapi.responses import ORJSONResponse
from pymongo import MongoClient
import redis
import zk_proto_betterproto.tqrpc_ods as tqrpc_ods
import zk_proto_betterproto.ods as ods
import zk_proto_betterproto.oms as oms
import zk_proto_betterproto.exch_gw as exch_gw
import zk_proto_betterproto.common as common
import nats
import asyncio
from fastapi import FastAPI, APIRouter
from pydantic import BaseModel
from contextlib import asynccontextmanager
from pydantic import BaseModel
from tqrpc_utils.config_utils import try_load_config
import sys 
import uvicorn
from loguru import logger

from fastapi.middleware.gzip import GZipMiddleware
import os
@dataclass
class RefDataConfig:
    mongo_uri: str = None
    mongo_db_name: str = None
    reload_interval_sec: int = None
    api_port: int = None
    worker_size: int = None

class Exch(BaseModel):
    exch_name: list[str]
    
_inst_type_map = {
    "PERP": "INST_TYPE_PERP",
    "SPOT": "INST_TYPE_SPOT",
    "INST_TYPE_PERP": "INST_TYPE_PERP",
    "INST_TYPE_SPOT": "INST_TYPE_SPOT",
}
class RefDataLoader:
    def __init__(self, config: RefDataConfig):
        

        self.config = config
        self.mongo_client = MongoClient(self.config.mongo_uri)
        self.db = self.mongo_client[self.config.mongo_db_name]
        self.instrument_refdata_by_id: dict[str, common.InstrumentRefData] = {}
        self.instrument_refdata: list[dict] = []
        self.all_exchanges: list[str] = []
        self.instrument_refdata_by_exchange: dict[str, list[common.InstrumentRefData]] = {}
        self._load_all_refdata()


    def _load_all_refdata(self):
        is_error = False
        err_symbols = []
        instruments_jsons = self._query_mongo(collection_name='refdata_instrument_basic')
        logger.info(f"Loading {len(instruments_jsons)} instruments")
        instruments = []
        exchanges = set()
        instruments_by_exchange = {}
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
                _instrument.original_info = "" # remove original_info to reduce size
                _instrument_dict = _instrument.to_dict(casing=Casing.SNAKE)
                instruments.append(_instrument_dict)
                exchanges.add(_instrument.exch_name)
                if _instrument.exch_name not in instruments_by_exchange:
                    instruments_by_exchange[_instrument.exch_name] = []
                instruments_by_exchange[_instrument.exch_name].append(_instrument_dict)
            except Exception as e:
                logger.error(f"Error loading instrument refdata: {e}")
                is_error = True
                err_symbols.append(instrument_json["instrument_id"])
        if is_error:
            logger.error(f"{len(err_symbols)} errors while loading instruments")
            logger.error(f"Invalid symbol refdata found for : {err_symbols}")

        self.instrument_refdata_by_id = {refdata["instrument_id"]: refdata for refdata in instruments}
        self.instrument_refdata = instruments
        self.all_exchanges = list(exchanges)
        self.instrument_refdata_by_exchange = instruments_by_exchange

        return True
    
    def get_all_refdata(self) -> list[dict]:
        return self.instrument_refdata
    
    def get_refdata_by_exchange(self, exch: str):
        return self.instrument_refdata_by_exchange.get(exch, [])

    def get_refdata_by_id(self, instrument_id: str) -> common.InstrumentRefData:
        return self.instrument_refdata_by_id.get(instrument_id)

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


    async def reload_task(self):
        while True:
            await asyncio.sleep(self.config.reload_interval_sec)
            try:
                logger.info("Reloading refdata...")
                asyncio.get_event_loop().run_in_executor(executor=None, func=self._load_all_refdata)
            except Exception as e:
                logger.error(f"Error reloading refdata: {e}")


    
    # def _configure_scheduler(self):
    #     scheduler = BackgroundScheduler()
    #     scheduler.add_job(self._load_accounts_configs, 'interval', seconds=self.config.reload_time)
    #     scheduler.start()

async def main():
    config: RefDataConfig = try_load_config(RefDataConfig)

    app = FastAPI()

    app.add_middleware(GZipMiddleware, minimum_size=2000)
    refdata = RefDataLoader(config=config)

    asyncio.create_task(refdata.reload_task())

    @app.get("/v1/refdata/instruments")
    async def get_all_instruments():
        all_instruments = refdata.get_all_refdata()
        resp = ORJSONResponse(content=all_instruments, status_code=200)
        return resp

    @app.get("/v1/refdata/instruments/{exchange}")
    async def get_instruments_by_exch(exchange: str):
        refdata_by_exch = refdata.get_refdata_by_exchange(exchange)
        resp = ORJSONResponse(content=refdata_by_exch, status_code=200)
        return resp

    @app.get("/v1/refdata/exchanges")
    async def get_all_exchanges():
        return refdata.all_exchanges

    univorn_config = uvicorn.Config(app, workers=config.worker_size,
                                    port=config.api_port, host="0.0.0.0", proxy_headers=True,
                                    forwarded_allow_ips="*")
    server = uvicorn.Server(univorn_config)
    await server.serve()


if __name__ == "__main__":
    asyncio.run(main())