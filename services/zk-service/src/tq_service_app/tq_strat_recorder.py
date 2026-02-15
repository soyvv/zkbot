from betterproto import Casing
from dataclasses import dataclass

#from infra_config import tq_infra_config_loader
from tqrpc_utils.config_utils import try_load_config

from zk_datamodel.strategy import *
import zk_datamodel.oms as oms
import nats
import tqrpc_utils.rpc_utils as svc_utils
import asyncio

from pymongo import MongoClient

@dataclass
class RecorderConfig:
    nats_url: str = None
    oms_channel: str = None
    mongo_url_base: str = None
    mongo_uri: str = None
    db_name: str = None
    record_trade: bool = False


class Persister:

    def __init__(self, mongo_url:str, db_name:str) -> None:
        self.client = MongoClient(mongo_url)
        self.db = self.client[db_name]
        self.strategy_order_collection = self.db["strategy_order"]
        self.strategy_log_collection = self.db["strategy_log"]
        self.strategy_cancel_collection = self.db["strategy_cancel"]
        self.strategy_lifecycle_collection = self.db["strategy_lifecycle"]
        self.strategy_trade_collection = self.db["strategy_trade"]

    def store_strategy_order(self, strat_order: StrategyOrderEvent):
        order_data = strat_order.to_dict(casing=Casing.SNAKE)
        #order_data["timestamp"] = int(datetime.now().timestamp())
        self.strategy_order_collection.insert_one(order_data)

    def store_strategy_log(self, strat_log: StrategyLogEvent):
        log_data = strat_log.to_dict(casing=Casing.SNAKE)
        #log_data["timestamp"] = int(datetime.now().timestamp())
        self.strategy_log_collection.insert_one(log_data)

    def store_strategy_cancel(self, strat_cancel: StrategyCancelEvent):
        cancel_data = strat_cancel.to_dict(casing=Casing.SNAKE)
        #cancel_data["timestamp"] = int(datetime.now().timestamp())
        self.strategy_cancel_collection.insert_one(cancel_data)

    def store_strategy_lifecycle(self, strat_lifecycle: StrategyLifecycleNotifyEvent):
        lifecycle_data = strat_lifecycle.to_dict(casing=Casing.SNAKE)
        #lifecycle_data["timestamp"] = int(datetime.now().timestamp())
        self.strategy_lifecycle_collection.insert_one(lifecycle_data)

    def store_trade(self, trade: oms.Trade):
        trade_data = trade.to_dict(casing=Casing.SNAKE)
        #trade_data["timestamp"] = int(datetime.now().timestamp())
        self.strategy_trade_collection.insert_one(trade_data)



class Recorder:
    def __init__(self, nats_url: str, mongo_url: str, db_name: str, 
                 oms_channel: str, record_trade: bool=False):
        self.nats_url = nats_url
        self.mongo_url = mongo_url
        self.oms_channel = oms_channel
        self.db_name = db_name
        self.nc = None
        self.persister = None
        self.log_sub = None
        self.lc_sub = None
        self.order_sub = None
        self.cancel_sub = None
        self.order_update_sub = None
        self.record_trade = record_trade

    async def connect(self):
        self.nc = await nats.connect(self.nats_url)
        self.persister = Persister(mongo_url=self.mongo_url, db_name=self.db_name)

    async def run_recorder(self):
        await self.connect()

        async def log_event_handler(msg):
            subject = msg.subject
            reply = msg.reply
            data = msg.data
            log_event = StrategyLogEvent().parse(data)
            print("Received a message on '{subject} {reply}': {data}".format(
                subject=subject, reply=reply, data=log_event))

            self.persister.store_strategy_log(log_event)

        async def strategy_lifecycle_notification_handler(msg):
            subject = msg.subject
            reply = msg.reply
            data = msg.data
            lc_event = StrategyLifecycleNotifyEvent().parse(data)
            print("Received a message on '{subject} {reply}': {data}".format(
                subject=subject, reply=reply, data=lc_event))

            self.persister.store_strategy_lifecycle(lc_event)

        async def order_event_handler(msg):
            subject = msg.subject
            reply = msg.reply
            data = msg.data
            order_event = StrategyOrderEvent().parse(data)
            print("Received a message on '{subject} {reply}': {data}".format(
                subject=subject, reply=reply, data=order_event))

            self.persister.store_strategy_order(order_event)

        async def cancel_event_handler(msg):
            subject = msg.subject
            reply = msg.reply
            data = msg.data
            cancel_event = StrategyCancelEvent().parse(data)
            print("Received a message on '{subject} {reply}': {data}".format(
                subject=subject, reply=reply, data=cancel_event))

            self.persister.store_strategy_cancel(cancel_event)

        async def order_update_handler(msg):
            data = msg.data
            order_update = oms.OrderUpdateEvent().parse(data)
            print("Received a message on '{subject} {reply}': {data}".format(
                subject=msg.subject, reply=msg.reply, data=order_update))

            if betterproto.serialized_on_wire(order_update.last_trade):
                trade = order_update.last_trade
                self.persister.store_trade(trade)

        self.log_sub = await self.nc.subscribe("tq.strategy.log.*", cb=log_event_handler)
        self.lc_sub = await self.nc.subscribe("tq.strategy.lifecycle.*", cb=strategy_lifecycle_notification_handler)
        self.order_sub = await self.nc.subscribe("tq.strategy.order.*", cb=order_event_handler)
        self.cancel_sub = await self.nc.subscribe("tq.strategy.cancel.*", cb=cancel_event_handler)
        #self.order_update_sub = await self.nc.subscribe("tq.oms.service.oms1.order_update", cb=order_update_handler)

        if self.record_trade:
            self.order_update_sub = await self.nc.subscribe(self.oms_channel, cb=order_update_handler)

    async def disconnect(self):
        if self.log_sub:
            await self.log_sub.unsubscribe()
        if self.lc_sub:
            await self.lc_sub.unsubscribe()
        if self.order_sub:
            await self.order_sub.unsubscribe()
        if self.cancel_sub:
            await self.cancel_sub.unsubscribe()
        if self.order_update_sub:
            await self.order_update_sub.unsubscribe()



def main():

    recorder_conf: RecorderConfig = try_load_config(RecorderConfig)
    nats_url = recorder_conf.nats_url
    oms_channel = recorder_conf.oms_channel
    mongo_url = recorder_conf.mongo_url_base + "/" + recorder_conf.mongo_uri
    db_name = recorder_conf.db_name

    recorder = Recorder(nats_url=nats_url, mongo_url=mongo_url, db_name=db_name,
                        oms_channel=oms_channel, record_trade=recorder_conf.record_trade)

    async def stop():
        await recorder.disconnect()

    runner = svc_utils.create_runner(shutdown_cb=stop)
    runner.run_until_complete(recorder.run_recorder())
    runner.run_forever()


if __name__ == "__main__":
    main()
