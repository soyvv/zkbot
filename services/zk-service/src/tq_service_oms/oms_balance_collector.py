import asyncio
import signal
from contextlib import suppress
import traceback
import logging
from typing import Union
import betterproto
from dataclasses import dataclass
import nats
import redis.asyncio as aioredis
import pymongo
import os
from zk_datamodel import tqrpc_exch_gw as gw_rpc
from zk_datamodel import oms, exch_gw, common
from zk_datamodel.ods import OMSConfigEntry
from zk_oms.core.confdata_mgr import ConfdataManager
from zk_oms.core.models import GWConfigEntry, OMSRouteEntry, InstrumentRefdata, BalanceSnapshot, PositionExtraData
from tq_service_oms.oms_gw_sender import GWSender
from tq_service_oms.oms_server import reload_refdata
from tqrpc_utils.config_utils import try_load_config

from tq_service_oms.config import resolve_oms_config, OMSConfig, get_db_config_loader, DBConfigLoader
from tq_service_oms.redis_handler import RedisHandler
from tq_service_oms.utils.formatter import get_now_ms
from tq_service_oms.utils import oms_position_converter

from loguru import logger

@dataclass
class RecorderConfig:
    nats_url: str = None
    mongo_uri: str = None
    mongo_dbname: str = None
    mongo_collection: str = None
    redis_host: str = None
    redis_port: str = None
    redis_password: str = None

async def shutdown(signal_, loop):
    print(f"Received {signal_.name} signal. Shutting down...")
    tasks = [t for t in asyncio.all_tasks() if t is not asyncio.current_task()]

    for task in tasks:
        task.cancel()

    with suppress(asyncio.CancelledError):
        await asyncio.gather(*tasks, return_exceptions=True)

    loop.stop()

def retrieve_oms_configs(recorder_config):
    config_list = []
    client = pymongo.MongoClient(recorder_config.mongo_uri)

    db = client[recorder_config.mongo_dbname]
    collection = db[recorder_config.mongo_collection]

    for document in collection.find():
        config = OMSConfig(
            enable_logger=True,
            enable_risk_check=True,
            oms_id=document['oms_id'],
            refdata_loading_mode="db",
            refdata_loading_path=recorder_config.mongo_uri,
            refdata_loading_path2="zk_oms",
            nats_url=recorder_config.nats_url,
            redis_host=recorder_config.redis_host,
            redis_port=recorder_config.redis_port,
            redis_password=recorder_config.redis_password
        )
        config_list.append(config)
    return config_list


def enrich_gw_position(
    account_id: int,
    gw_position: exch_gw.PositionReport,
    sync_timestamp_ms: int,
    account_detail: OMSRouteEntry,
    gw_config: GWConfigEntry,
    instrument_refdata: InstrumentRefdata) -> tuple[BalanceSnapshot, PositionExtraData]:

    if gw_position .instrument_type == common.InstrumentType.INST_TYPE_SPOT:
        b = BalanceSnapshot(
            account_id=account_id,
            account_name=account_detail.exch_account_id,
            exch=gw_config.exch_name,
            instrument_mo=gw_position.instrument_code,
            instrument_tq=gw_position.instrument_code,
            instrument_exch=gw_position.instrument_code,
            instrument_type=common.InstrumentType.INST_TYPE_SPOT.name,
            sync_timestamp=sync_timestamp_ms,
            update_timestamp=gw_position.update_timestamp,
            side="short" if gw_position.long_short_type == common.LongShortType.LS_SHORT else "long",
            total_qty=gw_position.qty,
            avail_qty=gw_position.avail_qty,
            frozen_qty=gw_position.qty - gw_position.avail_qty,
            raw_data=gw_position.message_raw
        )
    else:
        # non-spot instrument; need refdata to enrich
        if instrument_refdata is None:
            logger.error(f"Refdata not found for instrument_code: {gw_position.instrument_code}; some fields will be empty")
            instrument_mo = ""
            instrument_type = ""
        else:
            instrument_mo = f"{instrument_refdata.base_symbol}{instrument_refdata.quote_symbol}"
            instrument_type = instrument_refdata.instrument_type.name

        b = BalanceSnapshot(
            account_id=account_id,
            account_name=account_detail.exch_account_id,
            exch=gw_config.exch_name,
            instrument_mo=instrument_mo,
            instrument_tq=instrument_refdata.instrument_id,
            instrument_exch=gw_position.instrument_code,
            instrument_type=instrument_type,
            sync_timestamp=sync_timestamp_ms,
            update_timestamp=gw_position.update_timestamp,
            side="short" if gw_position.long_short_type == common.LongShortType.LS_SHORT else "long",
            total_qty=gw_position.qty,
            avail_qty=gw_position.avail_qty,
            frozen_qty=gw_position.qty - gw_position.avail_qty,
            raw_data=gw_position.message_raw
        )


    if gw_position.instrument_type != common.InstrumentType.INST_TYPE_SPOT:
        if not betterproto.serialized_on_wire(gw_position.margin_pos_info):
            # default value
            margin_info = common.MarginPositionInfo(
                contracts=gw_position.qty,
                contract_size=1,
                entry_price=None,
                index_price=None,
                last_trade_price=None,
                leverage=None,
                liquidation_price=None,
                margin=None,
                unsettled_pnl=None
            )
        else:
            margin_info = gw_position.margin_pos_info
        extra = PositionExtraData(
            contracts=margin_info.contracts,
            contract_size=margin_info.contract_size,
            pos_qty=gw_position.qty,
            unsettled_pnl=margin_info.unsettled_pnl,
            avg_entry_price=margin_info.entry_price,
            index_price=margin_info.index_price,
            last_trade_price=margin_info.last_trade_price,
            leverage=margin_info.leverage,
            liquidation_price=margin_info.liquidation_price,
            margin=margin_info.margin
        )
    else:
        extra = None

    return b, extra



async def resync_balance(
        account_id: int,
        account_route: OMSRouteEntry,
        gw_config: GWConfigEntry,
        refdata_lookup: dict[str, InstrumentRefdata],
        gw_sender: GWSender,
        redis_handler: RedisHandler):
    try:
        logger.info(f"Collecting balances for account: {account_id}")
        query_timestamp = get_now_ms()
        resp = await gw_sender.query_balances(gw_key=account_route.gw_key,
                                              timeout=3, retry_on_timeout=True)
        logger.info(f"Processing balances: {resp.balance_update.to_json()}")
    except:
        err_msg = f"Error querying balances for account: {account_id}, error={traceback.format_exc()}"
        logger.error(err_msg)
        return

    for gw_position in resp.balance_update.balances:
        try:
            refdata = refdata_lookup.get(gw_position.instrument_code, None) if refdata_lookup else None
            balance_snap, position_extra = enrich_gw_position(
                account_id=account_id,
                gw_position=gw_position,
                sync_timestamp_ms=query_timestamp,
                account_detail=account_route,
                gw_config=gw_config,
                instrument_refdata=refdata)

            logger.info(f"Storing balance report: balance_data={balance_snap}")
            await redis_handler.store_balance_report(balance_snap)
            if position_extra:
                logger.info(f"Storing position report: position_data={position_extra}")
                await redis_handler.store_position_report(balance_snap, position_extra)
        except:
            logger.error(f"Error storing balance report: account_id={account_id}, error={traceback.format_exc()}")
            logger.error(f"Error in position: {gw_position.to_json()}")
            continue



async def main(recorder_config: RecorderConfig):
    db_config_loader = DBConfigLoader(
            mongo_url=recorder_config.mongo_uri,
            db_name=recorder_config.mongo_dbname)

    nats_client = await nats.connect(recorder_config.nats_url)

    redis_client = aioredis.Redis(
        host=recorder_config.redis_host,
        port=int(recorder_config.redis_port),
        password=recorder_config.redis_password,
        db=0
    )

    refdata: list[InstrumentRefdata] = db_config_loader.load_refdata()
    refdata_lookup_by_gw_key: dict[str, dict[str, InstrumentRefdata]] = ConfdataManager.build_lookuptable_by_gw_key(
                gw_configs=db_config_loader.load_gw_config(),
                instrument_refdata=refdata
            )
    cnt = 0
    while True:
        if cnt == 10:
            cnt = 0
            try:
                _refdata = db_config_loader.load_refdata()
                _refdata_lookup_by_gw_key = ConfdataManager.build_lookuptable_by_gw_key(
                    gw_configs=db_config_loader.load_gw_config(),
                    instrument_refdata=refdata
                )
            except:
                logger.error(f"Error reloading refdata: {traceback.format_exc()}")
            else:
                refdata = _refdata
                refdata_lookup_by_gw_key = _refdata_lookup_by_gw_key

        try:
            all_oms_config_entries: list[OMSConfigEntry] = db_config_loader.load_oms_configs()
            gw_configs = db_config_loader.load_gw_config()
            gw_config_dict = {gw.gw_key: gw for gw in gw_configs}
            account_routes = db_config_loader.load_account_routing()
            gw_sender = GWSender(nats_client, gw_configs)
            account_routes_dict = {route.account_id: route for route in account_routes}
        except:
            logger.error(f"Error loading oms configs: {traceback.format_exc()}")
            await asyncio.sleep(60)
            continue

        tasks = []
        for oms_config_entry in all_oms_config_entries:
            logger.info(f"Starting collecting balances for oms: {oms_config_entry.oms_id}")
            try:
                account_ids = oms_config_entry.managed_account_ids
                redis_handler = RedisHandler(
                    redis_client=redis_client,
                    account_ids=list(account_ids),
                    name_space=oms_config_entry.namespace)

                for account_id in account_ids:
                    logger.info(f"Starting collecting balances for account: {account_id}")
                    account_route = account_routes_dict.get(account_id)
                    if not account_route:
                        logger.error(f"Account route not found for account_id: {account_id}")
                        continue
                    gw_config = gw_config_dict.get(account_route.gw_key)
                    if not gw_config:
                        logger.error(f"GW config not found for gw_key: {account_route.gw_key}")
                        continue
                    refdata_lookup = refdata_lookup_by_gw_key.get(account_route.gw_key)
                    try:
                        task = asyncio.create_task(resync_balance(
                            account_id=account_id,
                            account_route=account_route,
                            gw_config=gw_config,
                            refdata_lookup=refdata_lookup,
                            gw_sender=gw_sender,
                            redis_handler=redis_handler))
                        tasks.append(task)
                    except:
                        logger.error(f"Error recording balances for account: {account_id}, error={traceback.format_exc()}")
            except:
                logger.error(f"Error recording balances for oms: {oms_config_entry.oms_id}, error={traceback.format_exc()}")

        cnt += 1

        # wait for all tasks to be finished
        await asyncio.gather(*tasks)
        await asyncio.sleep(60)

if __name__ == "__main__":

    recorder_config: RecorderConfig = try_load_config(RecorderConfig)

    loop = asyncio.get_event_loop()

    # Register signal handlers for graceful shutdown
    signals = (signal.SIGTERM, signal.SIGINT)
    for sig in signals:
        loop.add_signal_handler(sig, lambda s=sig: asyncio.create_task(shutdown(s, loop)))

    try:
        loop.run_until_complete(main(recorder_config))
        loop.run_forever()
    except KeyboardInterrupt:
        print("Interrupted by user.")
