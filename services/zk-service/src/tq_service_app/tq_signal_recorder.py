from betterproto import Casing
import tqrpc_utils.rpc_utils as rpc_utils
from tqrpc_utils.config_utils import try_load_config

from zk_datamodel.strategy import *
import zk_datamodel.oms as oms
import zk_datamodel.rtmd as rtmd
import zk_utils.zk_utils as utils
import nats
from concurrent.futures import ThreadPoolExecutor
import asyncio
import logging
from pymongo import MongoClient
logging.basicConfig(format="%(levelname)s:%(filename)s:%(lineno)d:%(asctime)s:%(message)s", level=logging.DEBUG)
logger = logging.getLogger(__name__)


@dataclass
class AppConfig:
    nats_url: str = None
    # mongo_url_base: str = None,
    # mongo_uri: str = None,
    mongo_url: str = None
    db_name: str = None
    subject: str = None
    max_worker: int = None

class Persister:

    def __init__(self, mongo_url:str, db_name:str, max_workers:int) -> None:
        self.client = MongoClient(mongo_url)
        self.db = self.client[db_name]
        self._executor = ThreadPoolExecutor(max_workers=max_workers)

        self._queues: dict[str, asyncio.Queue] = {} # key: collection name, value: queue

        self.tasks = []

    def store_signal(self, signal: rtmd.RealtimeSignal):
        # order_data = ob.to_dict(casing=Casing.SNAKE)
        # #order_data["timestamp"] = int(datetime.now().timestamp())
        # self.ob_collection.insert_one(order_data)
        #self._executor.submit(self._batch_store_ts, signal)

        logger.debug("signal received:" + str(signal))

        if signal.namespace == None or signal.namespace == "":
            logger.warning("signal namespace or instrument is None; discard signal")
            return
        collection_name = signal.namespace
        if collection_name not in self._queues:
            if len(self._queues) > 20:
                raise Exception("Too many collections")
            self._ensure_collection_exsits(collection_name)
            queue = asyncio.Queue()
            task = asyncio.create_task(self._process_signal_queue(collection_name, queue))
            self.tasks.append(task)
            self._queues[collection_name] = queue

        self._queues[collection_name].put_nowait(signal)

    # def _store_ts(self, signal: rtmd.RealtimeSignal):
    #     collection = signal.namespace
    #     ts_data = [
    #         {"metadata": {"symbol": signal.instrument, "signal_name": k},
    #          k: v,
    #          "timestamp": signal.timestamp}
    #         for (k, v) in signal.data.items()
    #     ]
    #
    #     self.db[collection].insert_many(ts_data)

    def _batch_store_ts(self, collection_name:str, signals: list[rtmd.RealtimeSignal]):
        ts_data = [
            {"metadata": {"symbol": signal.instrument, "signal_name": k},
             k: v,
             "timestamp": utils.from_unix_ts_to_datetime(signal.timestamp)}
            for signal in signals
            for (k, v) in signal.data.items()
        ]

        ts_data.sort(key=lambda x: (x["metadata"]['symbol'], x["metadata"]['signal_name']))

        logger.debug("batch insertion: " + str(len(ts_data)) + " records")

        self.db[collection_name].insert_many(ts_data)

    async def _process_signal_queue(self, collection_name, queue: asyncio.Queue):
        while True:
            signals = []
            while not queue.empty():
                item = await queue.get()
                signals.append(item)
            if signals:
                logger.debug("processing " + str(len(signals)) + " signals")
                #self._executor.submit(self._batch_store_ts, collection_name, signals)
                asyncio.get_event_loop().run_in_executor(None, self._batch_store_ts, collection_name, signals)

            await asyncio.sleep(1)

    def _ensure_collection_exsits(self, collection_name: str):
        if collection_name not in self.db.list_collection_names():
            self.db.create_collection(
                collection_name,
                timeseries={
                    "timeField": "timestamp",
                    "metaField": "metadata",
                    "granularity": "seconds"
                })




class SignalRecorder:
    def __init__(self, nats_url: str,
                 subject: str,
                 mongo_url: str,
                 db_name: str,
                 max_workers: int=20
                 ):
        self.nc = None
        self.nats_url = nats_url
        self.ob_sub = None
        self.subject = subject
        self.persister = Persister(mongo_url, db_name, max_workers=max_workers)

    async def start(self):
        self.nc = await nats.connect(self.nats_url)


    async def run_recorder(self):
        async def ob_recorder(msg):
            subject = msg.subject
            reply = msg.reply
            # data = msg.data.decode()
            data = msg.data
            event = rtmd.RealtimeSignal().parse(data)

            self.persister.store_signal(event)

        self.ob_sub = await self.nc.subscribe(self.subject, cb=ob_recorder)

    async def stop(self):
        await self.ob_sub.unsubscribe()
        await self.nc.close()


def main():
    app_config: AppConfig = try_load_config(AppConfig)

    recorder = SignalRecorder(
        nats_url=app_config.nats_url,
        subject=app_config.subject,
       # mongo_url=app_config.mongo_url_base + "/" + app_config.mongo_uri,
        mongo_url=app_config.mongo_url,
        db_name=app_config.db_name,
        max_workers=app_config.max_worker
    )

    runner = rpc_utils.create_runner(shutdown_cb=recorder.stop)
    runner.run_until_complete(recorder.start())
    runner.run_until_complete(recorder.run_recorder())
    runner.run_forever()


if __name__ == "__main__":
    main()
