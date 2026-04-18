
import asyncio
import traceback
import json
import time
from typing import Literal
from pymongo import MongoClient
from dataclasses import dataclass

from zk_proto_betterproto import oms
from tqrpc_utils.config_utils import try_load_config
from betterproto import Casing
from loguru import logger
import ast
import os
import redis
import redis.asyncio as aioredis
import nats

@dataclass
class AppConfig:
    nats_url: str = None
    redis_host: str = None
    redis_port: str = None
    redis_password: str = None
    mongo_uri: str = None
    mongo_dbname: str = None
    posttrade_delay: int = 30
    skip_cancelled_order: bool = False


class PostTradeHandler:

    def __init__(self,
                 nats_url=None,
                 redis_url=None,
                 mongo_uri=None, mongo_dbname=None):
        self.nats_url = nats_url
        self.redis_url = redis_url
        self.mongo_uri = mongo_uri
        self.mongo_dbname = mongo_dbname
        self.client = None
        self.collection = None
        self.rc = None
        self.nc = None

    async def start(self):
        await self.connect_to_mongodb()
        await self.connect_to_redis()
        self.nc = nats.NATS()
        await self.nc.connect(self.nats_url)

        await self.nc.subscribe("tq.posttrade", queue="posttrade_task", cb=self.process_event)



    async def connect_to_mongodb(self):
        try:
            self.client = MongoClient(self.mongo_uri)
            self.collection = self.client[self.mongo_dbname]["order"]
        except Exception as e:
            logger.error(traceback.format_exc())
            raise e

    async def connect_to_redis(self):
        try:
            pool = aioredis.ConnectionPool.from_url(self.redis_url)
            self.rc = aioredis.Redis.from_pool(pool)
        except Exception as e:
            logger.error(traceback.format_exc())
            raise e

    async def process_event(self, msg):
        data = json.loads(msg.data.decode('utf-8'))
        logger.info(f"Message received: {data}")
        asyncio.create_task(self._process_posttrade_event(data))

    async def _process_posttrade_event(self, message: dict):
        redis_namespace = message['namespace']
        oms_id = message['oms_id']
        order_id = message['order_id']
        await asyncio.sleep(app_config.posttrade_delay)
        try:
            redisKey = f"{redis_namespace}:order:{order_id}"
            #Upload to MongoDB
            hash_data = await self.rc.hgetall(redisKey)
            hash_data = {key.decode('utf-8'): value.decode('utf-8') for key, value in hash_data.items()}
            hash_data['order_state'] = ast.literal_eval(hash_data['order_state'])
            hash_data['oms_req'] = ast.literal_eval(hash_data['oms_req'])
            hash_data['gw_req'] = ast.literal_eval(hash_data['gw_req'])
            hash_data['trades'] = ast.literal_eval(hash_data['trades'])
            hash_data.update({
                'order_id': hash_data['order_state']['order_id'],
                'order_status': hash_data['order_state']['order_status'],
                'updated_at': int(hash_data['order_state']['updated_at']),
                'recorded_at': int(time.time() * 1000),
                'filled_qty': hash_data['order_state'].get('filled_qty', 0.0),
                'oms_id': oms_id
            })
            logger.debug(f"MESSAGE UPLOADED: {hash_data}")

            # update or insert to MongoDB
            if app_config.skip_cancelled_order:
                if hash_data['order_status'] == "ORDER_STATUS_CANCELLED" and hash_data['filled_qty'] == 0.0:
                    return
            self.collection.update_one({'order_id': hash_data['order_id']}, {'$set': hash_data}, upsert=True)
        except Exception as e:
            logger.error("Error in posttrade event handling: " + traceback.format_exc())


    async def stop(self):
        self.client.close()
        self.rc.close()
        await self.nc.close()


if __name__ == "__main__":
    app_config: AppConfig = try_load_config(AppConfig)

    redis_url = f"redis://:{app_config.redis_password}@{app_config.redis_host}:{app_config.redis_port}/0"

    loop = asyncio.get_event_loop()
    handler = PostTradeHandler(
        app_config.nats_url,
        redis_url,
        app_config.mongo_uri,
        app_config.mongo_dbname)
    loop.run_until_complete(handler.start())
    try:
        loop.run_forever()
    except KeyboardInterrupt:
        logger.info("Shutting down...")
        asyncio.run(handler.stop())
        logger.info("Shutdown complete.")
    except Exception as e:
        logger.error(traceback.format_exc())
        asyncio.run(handler.stop())
        logger.info("Shutdown complete.")
        raise e

