from datetime import datetime, timedelta
import argparse
import logging
import time
import betterproto
import requests
import pymongo
import schedule

from dataclasses import dataclass
from tqrpc_utils.config_utils import try_load_config
from zk_proto_betterproto.tqrpc_oms import OMSOrderDetailResponse
from tq_service_helper.slack_notifier import SlackChannelNotifier

"""
Sample riskcheck_rule
{
  "_id": {
    "$oid": "658a4bccc58846886c4518c6"
  },
  "rule_key": "SAMPLE_RULE_KEY",
  "rule_config": {
    "7715": {
      "account_id": "7715",
      "action": "CANCEL",
      "schedule_sec": 60,
      "timeout_sec": 30
    },
    "226": {
      "account_id": "226",
      "action": "NOTIFY",
      "schedule_sec": 300,
      "timeout_sec": 60
    }
  },
  "rule_type": "MONITOR_OPEN_ORDERS"
}

Sample command to run
python -m tq_service_riskmonitor.open_order_monitor --oms_url 'localhost' --slack_token 'xoxb-SLACK-BOT-TOKEN' --slack_channel 'test-monitor-open-order' --mongo_url 'localhost:27017' --db_name 'local' --rule_key "MONITOR_OPEN_ORDERS" --dry_run True
"""

class MonitorAction(betterproto.Enum):
    UNSPECIFIED = 0
    CANCEL = 1
    NOTIFY = 2

@dataclass
class AppConfig:
    oms_url: str = None
    slack_token: str = None
    slack_channel: str = None
    mongo_url: str = None
    db_name: str = None
    rule_key: str = None
    dry_run: bool = None

class MonitorConfig():
    configured_accounts = set()
    
    def __init__(self, account_id:str, action:MonitorAction, schedule_sec:int, timeout_sec:int) -> None:
        # no account should be configured by multiple rule entries
        if account_id and account_id in self.configured_accounts:
            raise ValueError(f'Account [{account_id}] is already configured.')
        
        # minimal check interval: 5 seconds
        if schedule_sec < 5:
            raise ValueError(f'Invalid schedule [{schedule_sec}]sec for account [{account_id}]: Schedule should be at least 5 seconds.')
        
        # minimal timeout: 30 seconds
        if timeout_sec < 30:
            raise ValueError(f'Invalid timeout [{timeout_sec}]sec for account [{account_id}]: Timeout should be at least 30 seconds.')
        
        
        self.configured_accounts.add(account_id)
        
        self.account_id = account_id
        self.action = action
        self.schedule_sec = schedule_sec
        self.timeout_sec = timeout_sec
        

    def __repr__(self) -> str:
        return f"MonitorConfig(account_id={self.account_id}, action={self.action.name}, schedule_sec={self.schedule_sec}, timeout_sec={self.timeout_sec})"


class OpenOrderMonitor():
    timeout_order_count = dict() # {order_id: count}
    
    def __init__(self, config: MonitorConfig, oms_id:str, oms_url:str, notifier:SlackChannelNotifier, is_dry_run:bool):
        self.account_id = config.account_id
        self.oms_id = oms_id
        self.action = config.action
        self.timeout = int(config.timeout_sec)
        self.oms_url = oms_url
        self.notifier = notifier
        self.is_dry_run = is_dry_run
        
    
    def _get_open_orders(self)->OMSOrderDetailResponse:
        # call OMS api to get all open orders for self.account
        
        logging.info(f"Requesting open orders of [{self.account_id}] ...")
        
        req_url = OMS_URL + f"/v1/oms/open_orders?oms_id={self.oms_id}&account_id={self.account_id}"
        resp = requests.request("GET", req_url).content
        return OMSOrderDetailResponse().from_json(resp)


    def _cancel_order(self, order_id: str)->requests.Response:
        if not self.is_dry_run:
            # call OMS api to cancel order, request method DELETE
            
            req_url = OMS_URL + f"/v1/oms/orders/{self.oms_id}/{self.account_id}/{order_id}"
            logging.info(f"Canceling order [{order_id}] ...")
            resp = requests.request("DELETE", req_url).json()
            
            logging.info(f'Cancel order response: {resp}')
            
            return resp
        else:
            logging.info(f"<DRY-RUN> Canceling order [{order_id}] ...")
            return {'message': 'Dry-run mod. Not cancelling orders.'}
    
    def _notify_slack(self, message: str):
        if not self.is_dry_run:
            resp = self.notifier.notify(message)
            
            logging.info(f"Notifying slack with status: {resp['ok']}")
        else:
            logging.info(f"<DRY-RUN> Notifying slack with status: {True}")
    
    def process_open_orders(self):
        # for each order, get creation time and order id
        # if order is older than timeout time, take action accordingly, using OMS api, or slack notification, etc.
        
        open_orders = self._get_open_orders().to_dict().get('orders', [])
        
        if open_orders:
            current_time = datetime.now()
            timeout_orders = list(filter(lambda order: current_time - datetime.fromtimestamp(float(order['createdAt']) / 1000) > timedelta(seconds=self.timeout), open_orders))
            # timeout orders dict by id {order_id: order}
            timeout_orders_dict = {order.get('orderId'): order for order in timeout_orders}
            logging.info(f'{len(timeout_orders_dict)} timed out open orders found for account [{self.account_id}].')
            
            # if order is older than timeout time, take action accordingly
            if self.action == MonitorAction.CANCEL:
                # process repeated timeout orders
                orders_to_remove = []
                for old_timeout_order_id in self.timeout_order_count.keys():
                    if old_timeout_order_id not in timeout_orders_dict.keys():
                        # remove orders that no longer timeout
                        logging.info(f"Order-[{old_timeout_order_id}] of account-[{self.account_id}] oms-[{self.oms_id}] no longer timed out.")
                        orders_to_remove.append(old_timeout_order_id)
                    else:
                        # old timeout order still timeout
                        # if count exceeds threshold, do not cancel again, notif slack
                        if self.timeout_order_count[old_timeout_order_id] >= 3: # configure limit here
                            old_timeout_order = timeout_orders_dict.get(old_timeout_order_id)
                            self.timeout_order_count[old_timeout_order_id] += 1 # increase count to track total timeouts
                            message = f"Order-[{old_timeout_order_id}] of instrument-[{old_timeout_order['instrument']}] account-[{self.account_id}] oms-[{self.oms_id}] cancelling repeated {self.timeout_order_count[old_timeout_order_id]} times, paused cancellation."
                            logging.info(message)
                            self._notify_slack(message)
                            timeout_orders_dict.pop(old_timeout_order_id) # do not cancel again
                
                # remove orders from count that no longer timeout
                for k in orders_to_remove:
                    self.timeout_order_count.pop(k)
                
                if len(timeout_orders_dict) > 0:
                    # build notification
                    timeout_order_info = '\n'.join(
                        [f"Order-[{order.get('orderId')}] of instrument-[{order.get('instrument')}] account-[{self.account_id}] oms-[{self.oms_id}] with open time [{current_time - datetime.fromtimestamp(float(order['createdAt']) / 1000)}]"
                            for order in timeout_orders_dict.values()])
                    notif_msg_cancel = f"Cancelling timeout orders:\n{timeout_order_info}"
                    
                    logging.info(notif_msg_cancel)
                    self._notify_slack(notif_msg_cancel)
                
                    for timeout_order_id, timeout_order in timeout_orders_dict.items():
                        # increase count if repeated timeout
                        self.timeout_order_count[timeout_order_id] = self.timeout_order_count.get(timeout_order_id, 0) + 1
                        logging.info(f"Order-[{timeout_order_id}] of instrument-[{timeout_order['instrument']}] account-[{self.account_id}] oms-[{self.oms_id}] repeated {self.timeout_order_count[timeout_order_id]} times, proceed to cancelling...")
                        self._cancel_order(timeout_order_id)
                
                logging.info(f"Current timeout order count: {self.timeout_order_count}")
            
            elif self.action == MonitorAction.NOTIFY:
                # build notification
                timeout_order_info = '\n'.join(
                    [f"Order-[{order.get('orderId')}] of instrument-[{order.get('instrument')}]] account-[{self.account_id}] oms-[{self.oms_id}] with open time [{current_time - datetime.fromtimestamp(float(order['createdAt']) / 1000)}]"
                        for order in timeout_orders])
                notif_msg_notify = f"Notifying timeout orders:\n{timeout_order_info}"
                
                logging.info(notif_msg_notify)
                self._notify_slack(notif_msg_notify)
                
            # logic for other actions...
            
            elif self.action == MonitorAction.UNSPECIFIED:
                logging.info(f"Action {self.action.name} for account [{self.account_id}] oms-[{self.oms_id}] is undefined. Skipping...")
            
        else:
            # no open order, reset timeout count
            self.timeout_order_count.clear()
            logging.info(f"No open order found for account-[{self.account_id}] oms-[{self.oms_id}].")
        
        return


def get_oms_id_by_account_id(oms_url:str, account_id:str):
    
    response = requests.request("GET", oms_url + f"/v1/oms/accounts").json()

    for each in response:
        if str(each['account_id']) == account_id:
            logging.info(f"Account [{account_id}]'s oms id: {each['oms_id']}.")
            return each['oms_id']
    
    return None


if __name__ == '__main__':
    # init logging
    logging.basicConfig(
        level=logging.INFO,
        format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
    )
    
    # parse arguments
    app_config: AppConfig = try_load_config(AppConfig)
    
    OMS_URL = app_config.oms_url
    SLACK_TOKEN = app_config.slack_token
    SLACK_CHANNEL = app_config.slack_channel
    MONGO_URL = app_config.mongo_url
    DB_NAME = app_config.db_name
    RULE_KEY = app_config.rule_key
    IS_DRY_RUN = app_config.dry_run
    
    TABLE_NAME = "riskcheck_rule"
    RULE_TYPE = "MONITOR_OPEN_ORDERS"
    
    # connect to mongodb
    try:
        client = pymongo.MongoClient(MONGO_URL)
        db = client[DB_NAME]
        collection = db[TABLE_NAME]
        rules = list(collection.find({'rule_key': RULE_KEY}))
    except Exception as e:
        logging.info(f"Failed to connect to mongodb with error: {e}")
        exit(0)    
    
    if len(rules) != 1:
        raise Exception(f"No unique rule found for key [{RULE_KEY}]: {len(list(rules))}.")
    
    # load account configs from mongodb
    acc_configs = []
    for rule in rules:
        if rule['rule_type'] == RULE_TYPE:
            for acc_config in rule['rule_config'].values():
                monitor_config = MonitorConfig(
                    account_id=acc_config['account_id'],
                    action=MonitorAction[acc_config['action']],
                    schedule_sec=acc_config['schedule_sec'],
                    timeout_sec=acc_config['timeout_sec'])
                acc_configs.append(monitor_config)
                
    logging.info(f"Loaded monitor rule configs: {acc_configs}")    
    
    # slack notifier
    notifier = SlackChannelNotifier(SLACK_TOKEN, SLACK_CHANNEL)

    # accounts
    if not acc_configs:
        logging.info("No accounts configured.")
        exit(0)
    
    for mconfig in acc_configs:
        # oms_id
        oms_id = get_oms_id_by_account_id(OMS_URL, mconfig.account_id)
        if not oms_id:
            logging.info(f"Could not find oms_id for account [{mconfig.account_id}].")
            exit(0)

        # monitor
        monitor = OpenOrderMonitor(mconfig, oms_id, OMS_URL, notifier, IS_DRY_RUN)

        # schedule job
        schedule.every(mconfig.schedule_sec).seconds.do(monitor.process_open_orders)
        logging.info(f"Scheduled monitor for account [{mconfig.account_id}] every [{mconfig.schedule_sec}] seconds to [{mconfig.action.name}] with timeout [{mconfig.timeout_sec}] seconds.")
    
    # start scheduling
    while True:
        schedule.run_pending()
        time.sleep(1)
