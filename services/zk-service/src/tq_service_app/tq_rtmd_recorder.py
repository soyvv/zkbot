import traceback

from betterproto import Casing
import tqrpc_utils.rpc_utils as rpc_utils
from tqrpc_utils.config_utils import try_load_config

from zk_datamodel.strategy import *
import zk_datamodel.oms as oms
import zk_datamodel.rtmd as rtmd
import nats

import asyncio

from pymongo import MongoClient


@dataclass
class AppConfig:
    nats_url: str = None
    mongo_url_base: str = None
    mongo_uri: str = None
    db_name: str = None
    collection_name: str = None
    subjects: list[str] = None

class Persister:

    def __init__(self, mongo_url:str, db_name:str, collection_name:str) -> None:
        self.client = MongoClient(mongo_url)
        self.db = self.client[db_name]
        self.ob_collection = self.db[collection_name]

    def store_tick(self, ob: rtmd.TickData):
        order_data = ob.to_dict(casing=Casing.SNAKE)
        #order_data["timestamp"] = int(datetime.now().timestamp())
        self.ob_collection.insert_one(order_data)



class TickRecorder:
    def __init__(self, nats_url: str,
                 subjects: list[str],
                 mongo_url: str,
                 db_name: str,
                 collection_name: str
                 ):
        self.nc = None
        self.nats_url = nats_url
        self.ob_subs = []
        self.subjects = subjects
        self.persister = Persister(mongo_url, db_name, collection_name)

        self.recv_count = 0

    async def start(self):
        self.nc = await nats.connect(self.nats_url)


    async def run_recorder(self):
        async def ob_recorder(msg):
            subject = msg.subject
            reply = msg.reply
            # data = msg.data.decode()
            data = msg.data
            event = rtmd.TickData().parse(data)

            self.recv_count += 1
            if self.recv_count % 200 == 0:
                print("received tick data: " + str(self.recv_count))
            try:
                self.persister.store_tick(event)
            except:
                traceback.print_exc()
        for subject in self.subjects:
            _ob_sub = await self.nc.subscribe(subject, cb=ob_recorder)
            self.ob_subs.append(_ob_sub)

    async def stop(self):
        for ob_sub in self.ob_subs:
            await ob_sub.unsubscribe()
        await self.nc.close()


def main():
    app_config: AppConfig = try_load_config(AppConfig)

    recorder = TickRecorder(
        nats_url=app_config.nats_url,
        subjects=app_config.subjects,
        mongo_url=app_config.mongo_url_base + "/" + app_config.mongo_uri,
        db_name=app_config.db_name,
        collection_name=app_config.collection_name
    )

    runner = rpc_utils.create_runner(shutdown_cb=recorder.stop)
    runner.run_until_complete(recorder.start())
    runner.run_until_complete(recorder.run_recorder())
    runner.run_forever()


if __name__ == "__main__":
    main()
