import argparse
import asyncio
from loguru import logger

from datetime import datetime, timedelta
from nats import NATS

from zk_proto_betterproto import oms
from tq_service_riskmonitor.monitor_utils import *
import traceback
import json


"""
Sample Position Update Event
{'accountId': '9716', 
'positionSnapshots': [
    {'accountId': '9716', 
    'instrumentCode': 'MATIC-P/USDC@HYPERLIQUID', 
    'longShortType': 'LS_LONG', 
    'instrumentType': 'INST_TYPE_PERP', 
    'totalQty': 9.0, 'availQty': 9.0, 
    'syncTimestamp': '1709864867769', 
    'updateTimestamp': '1709864867765', 
    'isFromExch': True, 
    'exchDataRaw': "
    {'coin': 'MATIC', 
    'cumFunding': 
    {'allTime': '0.0', 'sinceChange': '0.0', 'sinceOpen': '0.0'}, 'entryPx': '1.1646', 'leverage': {'type': 'cross', 'value': 20}, 'liquidationPx': None, 'marginUsed': '0.523125', 'maxLeverage': 20, 'positionValue': '10.4625', 'returnOnEquity': '-0.03606388', 'szi': '9.0', 'unrealizedPnl': '-0.0189'}"}], 'timestamp': '1709865257544'}
"""
    
class BalanceUpdateMonitor:
    """
    Balance Updates Monitor
    """
    
    def __init__(
        self,
        nats_url: str,
        # target_subject: str,
        oms_id:int,
        max_secs_no_data: int,
        notify_subject: str,
        action: MonitorAction,
        account_id: int
    ):
        self.nats_url = nats_url
        self.oms_id = oms_id
        self.target_subject = self.__load_target_subject()
        self.max_secs_no_data = max_secs_no_data
        self.notify_subject = notify_subject  # nats topic for slack notification hub
        self.action = action
        self.watchers = {}
        self.account_id = account_id
        
        self.nats = NATS()
        
    def __load_target_subject(self):
        _ss = f"tq.oms.service.{self.oms_id}.balance_update.*"
        return _ss
            
    async def __on_msg(self, msg):
        
        data = msg.data
        postion_update = json.loads(oms.PositionUpdateEvent().parse(data).to_json())
        _instrument = postion_update['positionSnapshots'][0]['instrumentCode']
        _account_id = int(postion_update['accountId'])
        
        if _account_id != self.account_id:
            return
        
        # to log position update
        # logger.info(postion_update)
        
        # first balance update or new instrument found
        if _instrument not in self.watchers:
            logger.info(f"New instrument: {_instrument} on account_id: {self.account_id}")
            self.watchers[_instrument] = datetime.now() + timedelta(seconds=self.max_secs_no_data)
            asyncio.create_task(self.__wait(_instrument))
        else:
            # update timer if balance update received
            self.watchers[_instrument] = datetime.now() + timedelta(seconds=self.max_secs_no_data)
        
    
    async def __notify(self, message:str):
        '''Notify slack via tq_service_helper.notifiction_hub.
        Notificiation hub must be running and configured for notification.'''
        await self.nats.publish(self.notify_subject, message.encode())
        
        
        
    async def __stagger(self, name):
        curr_time = datetime.now()
        
        while curr_time < self.watchers[name]:
            await asyncio.sleep((self.watchers[name] - curr_time).seconds)
            curr_time = datetime.now()
        
    async def __wait(self, name):
        while True:
            # curr_time = datetime.now()
            # if curr_time > self.watchers[name]:
            await self.__stagger(name)
            msg = f"Account id: {self.account_id} \n Timestamp: {datetime.now()} \n Symbol:{name} \n No Balance Updates received in the past {self.max_secs_no_data} seconds"
            logger.info(msg)
            if self.action == MonitorAction.NOTIFY:
                await self.__notify(msg)
            else:
                logger.error(f"Verify Monitor Action in config. Expected {MonitorAction.NOTIFY} but got {self.action}")
                logger.info(f"Notif msg: {msg}")
            self.watchers[name] = datetime.now() + timedelta(seconds=self.max_secs_no_data)
        
    async def __connect(self):
        await self.nats.connect(self.nats_url)
        await self.nats.subscribe(self.target_subject, cb=self.__on_msg)
        logger.info(f"Connected to {self.nats_url} and subscribed to {self.target_subject}.")
        
    async def start(self):
        
        await self.__connect()
        logger.info(f"Monitoring on message from Nats topic-[{self.target_subject}].")
        logger.info(f"Loading BalanceMonitor for account_id: {self.account_id}")
        logger.info(f"Maximum duration allowed with no balance updates: {self.max_secs_no_data}")
    
if __name__ == "__main__":

    # app_config, rule, managed_accounts, oms_id = balance_monitor_config_load()
    app_config, rule, oms_mapping = balance_monitor_config_load()
    
    NATS_URL = app_config.nats_url
    MONGO_URL = app_config.mongo_url
    DB_NAME = app_config.db_name
    RULE_KEY = app_config.rule_key

    logger.info(f"app config: {app_config}")
    logger.info(f"rule: {rule['rule_config']}")
    logger.info(f"Oms -> account mapping: {oms_mapping}")

    config = rule["rule_config"]
    loop = asyncio.new_event_loop()

    for oms_id, managed_accounts in oms_mapping.items():
    
        logger.info(f"oms_id: {oms_id}, Managed accounts: {managed_accounts}")
        
        for acc_id in managed_accounts:
            order_monitor = BalanceUpdateMonitor(
                nats_url=NATS_URL,
                oms_id=oms_id,
                max_secs_no_data=config["max_secs_no_data"],
                notify_subject=config["notify_subject"],
                action=MonitorAction[config["action"]],
                account_id=acc_id
            )
            loop.create_task(order_monitor.start())
            
    loop.run_forever()

    

