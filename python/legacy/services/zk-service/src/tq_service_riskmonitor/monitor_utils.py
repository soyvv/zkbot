import betterproto
import pymongo
from loguru import logger
from datetime import datetime, timedelta
from dataclasses import dataclass

from tqrpc_utils.config_utils import try_load_config

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
    oms_id: str = None
    
@dataclass
class OrderMonitorConfig:
    nats_url: str = None
    mongo_url: str = None
    db_name: str = None    
    reject_key: str = None
    booked_key: str = None
    volume_key: str = None
    
@dataclass
class BalanceMonitorConfig:
    nats_url: str = None
    mongo_url: str = None
    db_name: str = None    
    rule_key: str = None
    
def get_current_ts():
    return int(datetime.now().timestamp() * 1000)

# argparse
def order_monitor_config_load():
    
    app_config: OrderMonitorConfig = try_load_config(OrderMonitorConfig)
    
    MONGO_URL = app_config.mongo_url
    DB_NAME = app_config.db_name
    RULE_KEYS = [
        app_config.reject_key, app_config.booked_key, app_config.volume_key
    ]
    # OMS_ID = app_config.oms_id
    
    COLLECTION_NAME = "riskcheck_rule"
    # oms_id = None
    oms_ids = set()
    # check_oms_acc = []
    
    # connect to mongodb
    try:
        client = pymongo.MongoClient(MONGO_URL)
        db = client[DB_NAME]
        collection = db[COLLECTION_NAME]
        rule_configs = []
        
        # watcher_mapping = {}
        oms_watcher = {}
        
        for RULE_KEY in RULE_KEYS:
            rule = collection.find_one({"rule_key": RULE_KEY})
            
            if rule is None:
                raise Exception(f"Rule with key {RULE_KEY} not found!")
            
            rule_configs.append(rule['rule_config'])
            
            # update oms -> watcher mapping
            for id_, _to_use in rule['rule_config']['oms_id'].items():
                if _to_use:
                    if id_ not in oms_watcher:
                        oms_watcher[id_] = [rule['rule_config']['name']]
                    else:
                        oms_watcher[id_].append(rule['rule_config']['name'])
                    
            oms_ids.update(rule['rule_config']['oms_id'])
            
        oms_mapping = {}
        # load oms config info
        collection_oms = db['oms_config']
        if oms_ids is None:
            raise Exception(f"Error. No oms_ids loaded. Please check config")
        
        for id in oms_ids:
            oms_config = collection_oms.find_one({"oms_id": id})
            managed_accounts = oms_config['managed_account_ids']
            oms_mapping[id] = managed_accounts
        
    except Exception as e:
        logger.exception(f"Failed to connect to MongoDB. Error: {e}", exc_info=True)
        exit(1)

    # return app_config, rule_configs, managed_accounts, oms_id
    return app_config, rule_configs, oms_mapping, oms_watcher
    
def balance_monitor_config_load():
    app_config: BalanceMonitorConfig = try_load_config(BalanceMonitorConfig)
    
    MONGO_URL = app_config.mongo_url
    DB_NAME = app_config.db_name
    RULE_KEY = app_config.rule_key
    # OMS_ID = app_config.oms_id
    
    
    COLLECTION_NAME = "riskcheck_rule"
    oms_ids = []
    
    # connect to mongodb
    try:
        client = pymongo.MongoClient(MONGO_URL)
        db = client[DB_NAME]
        collection = db[COLLECTION_NAME]
        rule = collection.find_one({"rule_key": RULE_KEY})
        # RULE_TYPE = rule['rule_type']
        
        if rule is None:
            raise Exception(f"Rule with key {RULE_KEY} not found!")
        
        # load single oms_id
        # if oms_ids is None:
        #     oms_ids = rule['rule_config']['oms_id']
            
        # load multiple oms_id
        for _id, _to_use in rule['rule_config']['oms_id'].items():
            if _to_use:
                oms_ids.append(_id)
                
        oms_mapping = {}
        # load oms config info
        collection_oms = db['oms_config']
        
        for id in oms_ids:
            oms_config = collection_oms.find_one({"oms_id": id})
            managed_accounts = oms_config['managed_account_ids']
            oms_mapping[id] = managed_accounts
        
    except Exception as e:
        logger.exception(f"Failed to connect to MongoDB. Error: {e}", exc_info=True)
        exit(1)

    # return app_config, rule, managed_accounts, oms_id
    return app_config, rule, oms_mapping
    