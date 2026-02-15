import asyncio
import json
import traceback

from betterproto import Casing
from dataclasses import dataclass
import datetime

from zk_oms.core.models import GWConfigEntry, OMSRouteEntry, InstrumentRefdata, TradeExtraData, RefdataContext
from tqrpc_utils.config_utils import try_load_config

from zk_datamodel.strategy import *
import zk_datamodel.oms as oms
import nats
import tqrpc_utils.rpc_utils as svc_utils

from pymongo import MongoClient

from .config import DBConfigLoader

from loguru import logger

import sqlalchemy as sa
from sqlalchemy import create_engine
from sqlalchemy.ext.declarative import declarative_base
from sqlalchemy.orm import sessionmaker

from .data_model import TQHistTrade


@dataclass
class RecorderConfig:
    nats_url: str = None
    oms_order_channel: str = None
    mongo_url_base: str = None
    mongo_uri: str = None
    mongo_datadb_name: str = None
    mongo_configdb_name: str = None
    pg_db_host: str = None
    pg_db_port: int = None
    pg_db_database: str = None
    pg_db_user: str = None
    pg_db_password: str = None
    pg_db_use_ssl: bool = False




def _convert_ts_to_datetime(ts: int):
    return datetime.datetime.fromtimestamp(float(ts) / 1000)

def enrich_trade(trade: oms.Trade, order_update_event: oms.OrderUpdateEvent,
                 refdata_ctx: RefdataContext) -> TradeExtraData:
    account_route = refdata_ctx.account_route
    gw_config = refdata_ctx.gw_config
    symbol_refdata = refdata_ctx.instrument_refdata

    extra_data = TradeExtraData()
    if account_route:
        extra_data.account_id = account_route.account_id
        extra_data.account_name = account_route.exch_account_id

    if gw_config:
        extra_data.exch = gw_config.exch_name

    if symbol_refdata:
        extra_data.instrument_type = symbol_refdata.instrument_type.name
        extra_data.instrument_mo = symbol_refdata.base_asset + symbol_refdata.quote_asset
        extra_data.instrument_exch = symbol_refdata.instrument_id_exchange

    extra_data.source_id = order_update_event.order_source_id
    extra_data.timestamp = trade.filled_ts
    extra_data.datetime = _convert_ts_to_datetime(trade.filled_ts)
    extra_data.record_datetime = datetime.datetime.now()

    return extra_data

def last_fee_exists_in_event(order_update_event: oms.OrderUpdateEvent) -> bool:
    if betterproto.serialized_on_wire(order_update_event.last_fee):
        fee = order_update_event.last_fee
        return fee.fee_amount is not None and fee.fee_amount != 0
    else:
        return False
    
def last_trade_exists_in_event(order_update_event: oms.OrderUpdateEvent) -> bool:
    return betterproto.serialized_on_wire(order_update_event.last_trade)

class OMSPersister:

    def __init__(self, mongo_url:str, datadb_name:str, configdb_name:str,
                 pg_url:str=None, pg_use_ssl=False) -> None:
        self._config_loader: DBConfigLoader = None
        self.reload_config(mongo_url, configdb_name)

        logger.info(f"connecting to mongo: {mongo_url}")
        self.client = MongoClient(mongo_url)
        self.db = self.client[datadb_name]
        self.trade_collection = self.db["trade"]
        self.fee_collection = self.db['fee']

        if pg_url:
            logger.info(f"connecting to pg: {pg_url}")
            if pg_use_ssl:
                self.pg_engine = create_engine(pg_url, connect_args={'sslmode': 'require'})
            else:
                self.pg_engine = create_engine(pg_url)
            self.make_session = sessionmaker(bind=self.pg_engine)

    
    def reload_config(self, mongo_url: str, configdb_name: str):
        if not self._config_loader:
            _config_loader = DBConfigLoader(mongo_url=mongo_url, db_name=configdb_name)
            self._config_loader = _config_loader
        else:
            _config_loader = self._config_loader
        account_routes = _config_loader.load_account_routing()
        gw_configs: list[GWConfigEntry] = _config_loader.load_gw_config()
        refdata: list[InstrumentRefdata] = _config_loader.load_refdata()
        self.gw_config_dict = {gw.gw_key: gw for gw in gw_configs}
        self.account_routes_dict = {route.accound_id: route for route in account_routes}
        self.refdata_dict = {rd.instrument_id: rd for rd in refdata}


    def store_trade(self, trade: oms.Trade, order_update_event: oms.OrderUpdateEvent):
        logger.info("preparing to store trade...")

        refdata_ctx = self._resolve_refdata_context(order_update_event)

        trade_extra_data = enrich_trade(
            trade, order_update_event, refdata_ctx)

        trade_data = trade.to_dict(casing=Casing.SNAKE)

        if trade_extra_data:
            trade_data.update(trade_extra_data.__dict__)

        # add last fee to trade if exists
        if last_fee_exists_in_event(order_update_event):
            trade_data['fee'] = order_update_event.last_fee.to_dict(casing=Casing.SNAKE)
            
        # TODO: add contract size and position
        # contract_size = 100
        # trade_data['contract_size'] = contract_size
        # trade_data['position'] = trade_data['filled_qty'] * contract_size

        logger.info(f"store trade to mongo: {trade_data}")
        self.trade_collection.insert_one(trade_data)

        logger.info("store trade to pg...")
        if self.pg_engine:
            try:
                self._store_trade_in_pg(trade_data)
            except Exception as e:
                logger.error(f"failed to store trade in pg: {traceback.format_exc()}")


    def _store_trade_in_pg(self, enriched_trade: dict):
        tq_trade_for_insert = {}

        tq_trade_for_insert.update(enriched_trade)

        # some adjustments
        if '_id' in tq_trade_for_insert:
            tq_trade_for_insert.pop("_id")

        if 'record_datetime' in tq_trade_for_insert:
            tq_trade_for_insert['record_ts'] = enriched_trade['record_datetime']
            tq_trade_for_insert.pop("record_datetime")

        if 'datetime' in tq_trade_for_insert:
            tq_trade_for_insert['trade_ts'] = enriched_trade['datetime']
            tq_trade_for_insert.pop("datetime")

        if 'timestamp' in tq_trade_for_insert:
            tq_trade_for_insert.pop("timestamp")

        if 'exch_pnl' in tq_trade_for_insert:
            tq_trade_for_insert.pop("exch_pnl")
            tq_trade_for_insert['realized_pnl'] = enriched_trade['exch_pnl']

        if 'fee' in tq_trade_for_insert:
            tq_trade_for_insert.pop("fee")
            fee = enriched_trade['fee'] # load fee data from enriched trade
            if 'fee_symbol' in fee:
                tq_trade_for_insert['fee_symbol'] = fee['fee_symbol']
            if 'fee_amount' in fee:
                tq_trade_for_insert['fee_amount'] = fee['fee_amount']
            if 'fee_type' in fee:
                tq_trade_for_insert['fee_type'] = fee['fee_type']




        logger.info(f"store trade to pg: {tq_trade_for_insert}")

        with self.make_session() as session:
            with session.begin():
                trade = TQHistTrade(**tq_trade_for_insert)
                session.add(trade)
                session.commit()


    def store_fee(self, fee: oms.Fee, order_update_event: oms.OrderUpdateEvent):
        logger.info("prepare store fee")

        if fee.fee_amount is None or fee.fee_amount == 0:
            logger.warning("fee amount is 0, skip storing fee")
            return
        
        # only proceed to store the fee if last trade is not present
        if last_trade_exists_in_event(order_update_event):
            logger.info("fee already exists in trade, skip storing fee")
            return

        refdata_ctx = self._resolve_refdata_context(order_update_event)

        fee_data = fee.to_dict(casing=Casing.SNAKE)
        fee_data['account_id'] = order_update_event.account_id

        if refdata_ctx.account_route:
            fee_data['account_name'] = refdata_ctx.account_route.exch_account_id

        if refdata_ctx.gw_config:
            fee_data['exch'] = refdata_ctx.gw_config.exch_name

        fee_data['timestamp'] = fee.fee_ts or order_update_event.timestamp
        fee_data['datetime'] = _convert_ts_to_datetime(fee_data['timestamp'])
        fee_data['record_datetime'] = datetime.datetime.now()

        logger.info(f'store fee: {fee_data}')
        self.fee_collection.insert_one(fee_data)


    def _resolve_refdata_context(self, order_update_event: oms.OrderUpdateEvent) -> RefdataContext:
        instrument_id = order_update_event.order_snapshot.instrument
        account_route = self.account_routes_dict.get(order_update_event.account_id, None)
        gw_config = self.gw_config_dict.get(account_route.gw_key, None)
        symbol_refdata = self.refdata_dict.get(instrument_id, None)

        refdata_ctx = RefdataContext(
            instrument_refdata=symbol_refdata,
            account_route=account_route,
            gw_config=gw_config,
            trading_config=None
        )

        return refdata_ctx

    async def stop(self):
        pass


class OMSRecorder:
    def __init__(self, nats_url: str, mongo_url: str,
                 mongo_datadbname: str, mongo_configdbname: str,
                 pg_url: str,
                 pg_use_ssl: bool,
                 order_channel: str):
        self.nats_url = nats_url
        self.mongo_url = mongo_url
        self.mongo_datadbname = mongo_datadbname
        self.mongo_configdbname = mongo_configdbname
        self.pg_url = pg_url
        self.pg_use_ssl = pg_use_ssl
        self.order_channel = order_channel
        self.nc = None
        self.persister = None
        self.order_update_sub = None

    async def connect(self):
        logger.info("connecting to nats...")
        self.nc = await nats.connect(self.nats_url)
        self.persister = OMSPersister(
            mongo_url=self.mongo_url, 
            datadb_name=self.mongo_datadbname,
            configdb_name=self.mongo_configdbname,
            pg_url=self.pg_url,
            pg_use_ssl=self.pg_use_ssl)

    async def run_recorder(self):
        await self.connect()


        def record_data(order_update: oms.OrderUpdateEvent):
            if betterproto.serialized_on_wire(order_update.last_trade):
                trade = order_update.last_trade
                self.persister.store_trade(trade, order_update)

            if betterproto.serialized_on_wire(order_update.last_fee):
                fee = order_update.last_fee
                self.persister.store_fee(fee, order_update)

        async def order_update_handler(msg):
            data = msg.data
            order_update = oms.OrderUpdateEvent().parse(data)
            logger.info("Received a message on '{subject} {reply}': {data}".format(
                subject=msg.subject, reply=msg.reply, data=order_update))

            asyncio.get_running_loop().run_in_executor(None, record_data, order_update)

        self.order_update_sub = await self.nc.subscribe(self.order_channel,
                                                        queue="tq_trade_record_workers",
                                                        cb=order_update_handler)


        async def reload(msg):
            logger.info("reloading config...")
            asyncio.get_running_loop().run_in_executor(
                None,
                self.persister.reload_config,
                self.mongo_url, self.mongo_configdbname)

        await self.nc.subscribe("tq.reload.ods", cb=reload)

    async def disconnect(self):
        if self.order_update_sub:
            await self.order_update_sub.unsubscribe()

        await self.persister.stop()

def main():
    recorder_conf: RecorderConfig = try_load_config(RecorderConfig)

    logger.info(f"recorder config: {recorder_conf}")
    nats_url = recorder_conf.nats_url
    mongo_url = recorder_conf.mongo_url_base + "/" + recorder_conf.mongo_uri

    pg_db_url = f"postgresql://{recorder_conf.pg_db_user}:{recorder_conf.pg_db_password}@{recorder_conf.pg_db_host}:{recorder_conf.pg_db_port}/{recorder_conf.pg_db_database}"

    recorder = OMSRecorder(
        nats_url=nats_url, 
        mongo_url=mongo_url,
        pg_url=pg_db_url,
        pg_use_ssl=recorder_conf.pg_db_use_ssl,
        mongo_datadbname=recorder_conf.mongo_datadb_name,
        mongo_configdbname=recorder_conf.mongo_configdb_name,
        order_channel=recorder_conf.oms_order_channel)

    async def stop():
        await recorder.disconnect()

    runner = svc_utils.create_runner(shutdown_cb=stop)
    runner.run_until_complete(recorder.run_recorder())
    runner.run_forever()


if __name__ == "__main__":
    main()
