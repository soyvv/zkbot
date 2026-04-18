import asyncio
import nats
import time
import datetime
import traceback
from typing import Literal
from pymongo import MongoClient
from dataclasses import dataclass
from tqrpc_utils.config_utils import try_load_config
from zk_proto_betterproto.oms import OrderUpdateEvent
from zk_proto_betterproto.oms import OrderStatus
from betterproto import Casing
from loguru import logger
import os

@dataclass
class AppConfig:
    nats_url: str = None
    mongo_uri: str = None
    mongo_dbname: str = None
    nats_topics: str = None
    retention_days: int = 30

class OrderEventListener:
    def __init__(self, nats_url=None,
                 nats_topics=None, mongo_uri=None, mongo_dbname=None,
                 retention_days=30,
                 logger=logger):
        self.nats_url = nats_url
        self.nats_topics = nats_topics
        self.mongo_uri = mongo_uri
        self.mongo_dbname = mongo_dbname
        self.client = None
        self.collection = None
        self.nc = None
        self.logger = logger
        self._retention_days = retention_days
        

    async def connect_to_mongodb(self):
        try:
            self.client = MongoClient(self.mongo_uri)
            self.collection = self._create_orderevents_collection()
        except Exception as e:
            self.logger.error(traceback.format_exc())
            raise e

    def _create_orderevents_collection(self):
        col = self.client[self.mongo_dbname]["order_events"]
        col.create_index("order_id")
        col.create_index("recorded_at", expireAfterSeconds=self._retention_days * (3600 * 24))
        return col


    async def connect_to_nats(self):
        try:
            self.nc = await nats.connect(self.nats_url)
        except Exception as e:
            self.logger.error(traceback.format_exc())
            raise e

    async def process_received_messages(self, message_queue):
        while True:
            message = await message_queue.get()
            try:
                deserializedMessage = OrderUpdateEvent().parse(message)

                filledQty = deserializedMessage.order_snapshot.filled_qty
                updatedAt = deserializedMessage.order_snapshot.updated_at
                orderStatus = OrderStatus(deserializedMessage.order_snapshot.order_status)
                deserializedMessage = deserializedMessage.to_dict(Casing.SNAKE)
                deserializedMessage['filled_qty'] = filledQty
                deserializedMessage['recorded_at'] = int(time.time() * 1000)
                deserializedMessage['recorded_at_dt'] = self._convert_ts_to_datetime(deserializedMessage['recorded_at'])
                deserializedMessage['updated_at'] = updatedAt
                deserializedMessage['updated_at_dt'] = self._convert_ts_to_datetime(deserializedMessage['updated_at'])
                deserializedMessage['order_status'] = orderStatus.name
                self.logger.info(deserializedMessage)
                # Upload to MongoDB
                self.collection.insert_one(deserializedMessage)
            except Exception as e:
                self.logger.error(traceback.format_exc())
            finally:
                message_queue.task_done()

    def _convert_ts_to_datetime(self, ts: int):
        return datetime.datetime.fromtimestamp(float(ts) / 1000)

    async def main(self):
        try:
            await self.connect_to_mongodb()
            await self.connect_to_nats()

            message_queue = asyncio.Queue(maxsize=1000)

            async def handle_message(msg):
                await message_queue.put(msg.data)  

            task = asyncio.create_task(self.process_received_messages(message_queue))

            for topic in self.nats_topics:
                await self.nc.subscribe(topic, queue="order_event_reorder_task", cb=handle_message)

            await asyncio.wait([task])

        except:
            self.logger.error(traceback.format_exc())
        finally:
            if self.nc:
                await self.nc.close()
            if self.client:
                self.client.close()


if __name__ == '__main__':

    config: AppConfig = try_load_config(AppConfig)

    listener = OrderEventListener(
        nats_url= config.nats_url,
        mongo_uri=config.mongo_uri,
        mongo_dbname=config.mongo_dbname,
        nats_topics=config.nats_topics.split(','),
        retention_days=config.retention_days
    )
    asyncio.run(listener.main())