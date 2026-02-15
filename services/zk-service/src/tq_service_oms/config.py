
import csv, os, configparser
import logging
from typing import Optional

from pymongo import MongoClient
from dataclasses import dataclass
from zk_oms.core.models import GWConfigEntry, OMSRouteEntry, InstrumentRefdata, InstrumentTradingConfig
import zk_datamodel.common as common
import zk_datamodel.ods as ods
'''
OMS configurations:
- infra related config: including NATS/Redis/DB/Logger etc.
    infra config will be loaded from ini files
- refdata related config: including instrument refdata, exchange refdata, account mapping etc.
    refdata config will be loaded from DB or local CSV files
- behavioral config for OMSCore
    behavioral config will be loaded from cmd line
'''


# refdata from common.InstrumentRefData to oms_core.models.InstrumentRefData
# TODO: merge the data model
_REF_DATA_FIELD_MAPPING = {
    'instrument_id': 'instrument_code',
    'instrument_type': 'instrument_type',
    'instrument_id_exchange': 'exch_symbol',
    'exchange_name': 'exch_name',
    'base': 'base_symbol',
    'quote': 'quote_symbol',
    'price_precision': 'price_precision',
    'qty_precision': 'qty_precision',
    'disabled': 'disabled'
}


@dataclass
class OMSConfig:
    enable_logger: bool = True
    enable_risk_check: bool = True
    handle_non_tq_order_reports: bool = False
    relaxing_ordering: bool = False  # no queueing for order/cancel submission
    oms_id: str = None
    #serving_accounts: list[str] = None
    refdata_loading_mode: str = None # DB or CSV
    refdata_loading_path: str = None # DB connection string or CSV file path
    refdata_loading_path2: str = None # DB name for DB type
    nats_url: str = None
    #redis_namespace: str = None

    redis_host: str = None
    redis_port: int = None
    redis_password: str = None


@dataclass
class OMSResolvedConfig:
    # infra related config
    pubsub_url: str = None
    rpc_endpoint: str = None
    report_pub_endpoint: str = None
    balance_pub_endpoint: str = None
    system_pub_endpoint: str = None

    redis_namespace: str = None
    redis_host: str = None
    redis_port: int = None
    redis_password: str = None

    # refdata related config
    # instrument_refdata: list[InstrumentRefdata] = None
    # gw_config_table: list[GWConfigEntry] = None
    # account_routing_table: list[OMSRouteEntry] = None
    # trading_config_table: list[InstrumentTradingConfig] = None
    # serving_accounts: list[int] = None
    # account_details: dict[int, dict] = None
    # instrument_details: dict[int, dict] = None
    # instrument_mapping: dict[str, dict] = None # exch_name -> instrument_exch -> instrument_tq

    # db_config_loader: "DBConfigLoader" = None

    # behavioral config for OMSCore
    enable_logger: bool = True
    enable_balance_bookkeeping: bool = True
    enable_risk_check: bool = True
    enable_non_tq_order_reports: bool = False
    enable_ordering_relaxing: bool = False

# These values will be used if not specified in the DB
_GW_CONFIG_DEFAULTS = {
    "support_order_replace": False,
    "support_order_query": True,
    "support_position_query": True,
    "support_batch_order": True,
    "support_batch_cancel": True,
    "support_trade_history_query": True,
    "support_fee_query": True,
    "calc_balance_needed": False,
    "cancel_required_fields": ["order_id", "instrument_exch"]
}



class DBConfigLoader:
    def __init__(self, mongo_url: str = "mongodb://localhost:4717/", db_name: str = "zk_oms",
                 mongo_client: MongoClient = None):
        self.mongo_client = MongoClient(mongo_url) if mongo_client is None else mongo_client
        self.db = self.mongo_client[db_name]
        self.instrument_refdata_collection = self.db['refdata_instrument_basic']
        self.gw_config_collection = self.db['refdata_gw_config']
        self.account_routing_collection = self.db['refdata_account_mapping']
        self.instrument_trading_config_collection = self.db['refdata_instrument_trading_config']
        self.oms_config_collection = self.db['oms_config']
        self.simulator_config_collection = self.db['sim_gw_config']

        self._null_check_fields = ["price_precision", "qty_precision"]
        # make them None if not set (to avoid 0 set automatically)


    def get_oms_config(self, oms_id: str) -> ods.OMSConfigEntry:
        _oms_config = self.oms_config_collection.find_one({"oms_id": oms_id})
        if _oms_config is None:
            raise ValueError(f"OMS config not found for oms_id {oms_id}")
        self._remove_id(_oms_config)
        oms_config_entry = ods.OMSConfigEntry().from_dict(_oms_config)

        return oms_config_entry


    def load_oms_configs(self) -> list[ods.OMSConfigEntry]:
        oms_configs = []
        for record in self.oms_config_collection.find({}):
            record_dict = self._remove_id(record)
            oms_configs.append(ods.OMSConfigEntry().from_dict(record_dict))
        return oms_configs

    def get_simulator_config(self, gw_key: str) -> Optional[dict]:
        _sim_config = self.simulator_config_collection.find_one({"gw_key": gw_key})
        if _sim_config is None:
            return None
        self._remove_id(_sim_config)
        return _sim_config



    def _remove_id(self, record:dict):
        if '_id' in record:
            del record['_id']
        return record

    def load_refdata(self) -> list[InstrumentRefdata]:
        refdata_records = self.instrument_refdata_collection.find({})
        refdata = []
        for record in refdata_records:
            record_dict = self._remove_id(record)
            _instrument_type = record_dict['instrument_type']
            instrument_type_str = _instrument_type if _instrument_type.startswith("INST_TYPE_") else "INST_TYPE_" + _instrument_type
            if instrument_type_str not in common.InstrumentType.__members__:
                raise ValueError(f"Invalid instrument type {record_dict['instrument_type']}")
            record_dict['instrument_type'] = common.InstrumentType.from_string(instrument_type_str)
            for field in self._null_check_fields:
                if field not in record_dict or record_dict[field] is None:
                    record_dict[field] = None

            refdata.append(InstrumentRefdata(**record_dict))
        return refdata

    def load_gw_config(self) -> list[GWConfigEntry]:
        gw_config_records = self.gw_config_collection.find({})
        gw_configs = []
        for record in gw_config_records:
            record_dict = _GW_CONFIG_DEFAULTS.copy()
            record_dict.update(self._remove_id(record))
            gw_configs.append(GWConfigEntry(**record_dict))
        return gw_configs

    def load_account_routing(self) -> list[OMSRouteEntry]:
        records = self.account_routing_collection.find({})
        accounts = []
        for record in records:
            record_dict = self._remove_id(record)
            record_dict['account_id'] = record_dict['accound_id']
            accounts.append(OMSRouteEntry(**record_dict))
        return accounts

    def load_symbol_trading_config(self) -> list[InstrumentTradingConfig]:
        records = self.instrument_trading_config_collection.find({})
        configs = []
        for record in records:
            record_dict = self._remove_id(record)
            configs.append(InstrumentTradingConfig(**record_dict))
        return configs

    # def load_instrument_details(self) -> dict[int, dict]: # instrument_code -> refdata_instrument_basic
    #     instrument_details = {}
    #     for instrument_config in self.instrument_refdata_collection.find({}):
    #         instrument_details[instrument_config['instrument_code']] = instrument_config
    #     return instrument_details

    # def load_account_details(self) -> dict[int, dict]: # account_id -> refdata_account_mapping + exch
    #     _gw2exch = {}
    #     _gw_details = {}
    #     for gw_config in self.gw_config_collection.find({}):
    #         gw_key = gw_config['gw_key']
    #         _gw2exch[gw_key] = gw_config['exch_name']
    #         _gw_details[gw_key] = gw_config
    #
    #     account_details = {}
    #     for account_config in self.account_routing_collection.find({}):
    #         account_detail = account_config.copy()
    #
    #         gw_key = account_config['gw_key']
    #         account_detail['exch'] = _gw2exch[gw_key]
    #         account_detail['gw_detail'] = _gw_details[gw_key]
    #
    #         account_details[account_config['accound_id']] = account_detail
    #     return account_details

    # def load_instrument_mapping(self) -> dict: # exch_name -> instrument_exch -> instrument_tq
    #     instrument_mapping = {}
    #     for instrument_config in self.instrument_refdata_collection.find({}):
    #         exch = instrument_config['exch_name']
    #         ins_exch = instrument_config['exch_symbol']
    #         ins_tq = instrument_config['instrument_code']
    #         if exch not in instrument_mapping:
    #             instrument_mapping[exch] = {}
    #         instrument_mapping[exch][ins_exch] = ins_tq
    #     return instrument_mapping

def resolve_oms_config(cmdline_config: OMSConfig, db_config_loader: DBConfigLoader=None) -> OMSResolvedConfig:

    logging.info(f"cmdline config: f{cmdline_config}")
    if db_config_loader:
        _config_loader = db_config_loader
    else:
        _config_loader = get_db_config_loader(cmdline_config)

    oms_config_entry = _config_loader.get_oms_config(cmdline_config.oms_id)
    logging.info(f"oms confing loaded from db: {oms_config_entry}")
    serving_accounts = oms_config_entry .managed_account_ids
    redis_namespace = oms_config_entry.namespace


    _config = OMSResolvedConfig()

    _config.pubsub_url = cmdline_config.nats_url
    _config.rpc_endpoint = f"tq.oms.service.{cmdline_config.oms_id}.rpc"
    _config.report_pub_endpoint = f"tq.oms.service.{cmdline_config.oms_id}.order_update"
    _config.balance_pub_endpoint = f"tq.oms.service.{cmdline_config.oms_id}.balance_update"
    _config.system_pub_endpoint = f"tq.oms.service.{cmdline_config.oms_id}.system_update"
    _config.redis_host = cmdline_config.redis_host
    _config.redis_port = cmdline_config.redis_port
    _config.redis_password = cmdline_config.redis_password


    # _config.instrument_refdata = _config_loader.load_refdata()
    # _config.gw_config_table = _config_loader.load_gw_config()
    # _config.account_routing_table = _config_loader.load_account_routing()
    # _config.trading_config_table = _config_loader.load_symbol_trading_config()
    # _config.account_details = _config_loader.load_account_details()
    # _config.instrument_details = _config_loader.load_instrument_details()
    # _config.instrument_mapping = _config_loader.load_instrument_mapping()

    _config.serving_accounts = []
    _config.serving_accounts = serving_accounts
    _config.enable_logger = cmdline_config.enable_logger
    _config.enable_balance_bookkeeping = True
    _config.enable_risk_check = cmdline_config.enable_risk_check
    _config.enable_non_tq_order_reports = cmdline_config.handle_non_tq_order_reports
    _config.enable_ordering_relaxing = cmdline_config.relaxing_ordering

    _config.redis_namespace=redis_namespace

    return _config


def get_db_config_loader(cmdline_config: OMSConfig=None) -> DBConfigLoader:
    if cmdline_config.refdata_loading_mode == "db":
        return DBConfigLoader(
            mongo_url=cmdline_config.refdata_loading_path,
            db_name=cmdline_config.refdata_loading_path2)
    else:
        raise NotImplementedError("Only DB mode is supported for now")




