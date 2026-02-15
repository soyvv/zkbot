import argparse
import asyncio
import betterproto
import logging
import os
import pymongo

from datetime import datetime, timedelta
from nats import NATS

from dataclasses import dataclass
from tqrpc_utils.config_utils import try_load_config

"""
Sample zk_oms.riskcheck_rule:
{
  "rule_key": "NATS_CHANNEL_NO_DATA_MONITOR",
  "rule_config": [
    {
        "target_subjct": "test-1",
        "max_secs_no_data": 10,
        "action": "RESTART",
        "notify_subject": "tq.kubn_restart_monitor",
        "deployment_name": "tq-rtmd-binancedm-main-prod"
    },
    {
        "target_subject": "test-2",
        "max_secs_no_data": 15,
        "action": "NOTIFY",
        "notify_subject": "tq.kubn_restart_monitor",
        "deployment_name": "tq-rtmd-other-main-prod"
    }
    ],
  "rule_type": "NATS_CHANNEL_NO_DATA_MONITOR"

}

Sample zk_oms.notification_hub_config:
{
  "nats_topic": "tq.kubn_restart_monitor",
  "slack_channel": "test-monitor-channel",
  "limit": 5,
  "interval_sec": 10
}

Sample start script:
python -m tq_service_riskmonitor.kubn_restart_monitor --nats_url "nats://localhost:4222" --mongo_url "mongodb://localhost:27017" --db_name "zk_oms" 
"""

class MonitorAction(betterproto.Enum):
    UNSPECIFIED = 0
    RESTART = 1
    NOTIFY = 2

@dataclass
class AppConfig:
    nats_url: str = None
    mongo_url: str = None
    db_name: str = None
    rule_key: str = None

class NatsChannelMonitor:
    '''Monitor if no message is received from nats for a certain period, then restart kubernetes deployment.'''

    def __init__(self, nats_url:str, target_subject:str, max_secs_no_data:int, notify_subject:str, action:MonitorAction, deployment_name:str):
        self.nats_url = nats_url
        self.target_subject = target_subject
        self.max_secs_no_data = max_secs_no_data
        self.notify_subject = notify_subject # nats topic for slack notification hub
        self.action = action
        self.deployment_name = deployment_name

        self.nats = NATS()
        self.restart_time = datetime.now() + timedelta(seconds=max_secs_no_data)
    
    async def __on_msg(self, msg):
        self.restart_time = datetime.now() + timedelta(seconds=self.max_secs_no_data)
        logging.info(f"Received a message: {msg.data}\nSet next restart time to: {self.restart_time}")
    
    async def __connect(self):
        await self.nats.connect(self.nats_url)
        await self.nats.subscribe(self.target_subject, cb=self.__on_msg)
        logging.info(f"Connected to {self.nats_url} and subscribed to {self.target_subject}.")
        
    async def __wait(self):
        curr_time = datetime.now()
        while curr_time < self.restart_time:
            await asyncio.sleep((self.restart_time - curr_time).seconds)
            curr_time = datetime.now()
   
    async def __notify(self, message: str):
        '''Notify slack via tq_service_helper.notifiction_hub.
        Notificiation hub must be running and configured for notification.'''
        await self.nats.publish(self.notify_subject, message.encode())
    
    async def __restart_service(self):
        # msg = f"No message received for {self.max_secs_no_data} seconds from Nats topic-[{self.target_subject}].\nRestarting kubernetes deployment/{self.deployment_name} ..."
        msg = f"No message received for {self.max_secs_no_data} seconds from Nats topic-[{self.target_subject}].\nRestarting kubernetes deployment/{self.deployment_name} is required."

        logging.info(msg)
        await self.__notify(msg)

        # restart kubernetes deployment
        # stop_cmd = f"kubectl scale --replicas=0 deployment/{self.deployment_name}"
        # start_cmd = f"kubectl scale --replicas=1 deployment/{self.deployment_name}"
        # os.system(stop_cmd)
        # await asyncio.sleep(5)
        # os.system(start_cmd)
    
    async def start(self):
        await self.__connect()
        logging.info(f"Monitoring on message from Nats topic-[{self.target_subject}].")
        while True:
            await self.__wait()
            if self.action == MonitorAction.RESTART:
                await self.__restart_service()
            elif self.action == MonitorAction.NOTIFY:
                msg = f"No message received for {self.max_secs_no_data} seconds from Nats topic-[{self.target_subject}]."
                logging.info(msg)
                await self.__notify(msg)
            else:
                logging.info(f"No action specified for {self.action.name} on topic-[{self.target_subject}].")    
            
            self.restart_time = datetime.now() + timedelta(seconds=self.max_secs_no_data)
        

if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO, format="%(asctime)s [%(levelname)s] %(message)s")
    
    # argparse
    app_config: AppConfig = try_load_config(AppConfig)
    
    NATS_URL = app_config.nats_url
    MONGO_URL = app_config.mongo_url
    DB_NAME = app_config.db_name
    
    COLLECTION_NAME = "riskcheck_rule"
    #RULE_KEY = "NATS_CHANNEL_NO_DATA_MONITOR"
    RULE_KEY = app_config.rule_key
    RULE_TYPE = "NATS_CHANNEL_NO_DATA_MONITOR"
    
    # connect to mongodb
    try:
        client = pymongo.MongoClient(MONGO_URL)
        db = client[DB_NAME]
        collection = db[COLLECTION_NAME]
        rule = collection.find_one({"rule_key": RULE_KEY})
    except Exception as e:
        logging.exception(f"Failed to connect to MongoDB. Error: {e}", exc_info=True)
        exit(1)
    
    if rule is None:
        raise Exception(f"Rule with key {RULE_KEY} not found!")
    if rule['rule_type'] != RULE_TYPE:
        raise Exception(f"Rule type mismatch. Expected {RULE_TYPE} but got {rule['rule_type']}")
    
    configs = []
    loop = asyncio.new_event_loop()
    for config in rule['rule_config']:
        monitor = NatsChannelMonitor(
            nats_url=NATS_URL,
            target_subject=config['target_subject'],
            max_secs_no_data=config['max_secs_no_data'],
            notify_subject=config['notify_subject'],
            action=MonitorAction[config['action']],
            deployment_name=config['deployment_name']
        )
        loop.create_task(monitor.start())
    loop.run_forever()
    