import asyncio
import datetime
import itertools
import json
import signal
import sys
import time
from contextlib import suppress
import traceback
import nats
import redis.asyncio as aioredis

from zk_oms.core.confdata_mgr import ConfdataManager
from tq_service_oms.oms_gw_sender import GWSender

try:
    from .redis_handler import RedisHandler
except:
    from tq_service_oms.redis_handler import RedisHandler

try:
    from .config import resolve_oms_config, OMSConfig, DBConfigLoader, get_db_config_loader
except:
    from tq_service_oms.config import resolve_oms_config, OMSConfig


from zk_oms.core.oms_core import OMSCore

from zk_oms.core.models import OMSActionType, OMSAction, GWConfigEntry, OMSOrder, OMSRouteEntry, OMSPosition, \
    OrderRecheckRequest, CancelRecheckRequest, ExchOrderRef, OMSCoreSchedRequest, SchedRequestType
from zk_datamodel import oms, common
from zk_datamodel import tqrpc_oms as oms_rpc
from zk_datamodel import exch_gw as gw
from zk_datamodel import tqrpc_exch_gw as gw_rpc
from zk_datamodel.tqrpc_oms import  OMSResponse, OMSResponseStatus

from loguru import logger


OK = OMSResponse(status=OMSResponseStatus.OMS_RESP_STATUS_SUCCESS)

POST_TRADE_EVENT_TOPIC = "tq.posttrade"


class OMSSubscriber:

    def __init__(self,
                 nats_client: nats.NATS,
                 managed_accounts: list[int],
                 gw_config_table: list[GWConfigEntry],
                 account_routes: list[OMSRouteEntry],
                 queue: asyncio.Queue, oms_core: OMSCore,
                 gw_sender: GWSender,
                 rpc_endpoint: str):
        self.nc = nats_client
        self.rpc_endpoint = rpc_endpoint
        self.managed_accounts = set(managed_accounts)
        self.gw_subs = {}
        self.req_sub = None
        self.reqc_sub = None
        self.queue = queue

        self.oms_core = oms_core
        self.gw_sender = gw_sender

        self.reload_config(gw_config_table, account_routes)


    def reload_config(self, gw_config_table: list[GWConfigEntry],
                      account_routes: list[OMSRouteEntry]):
        logger.info("reloading config for oms subscriber")
        self._gws_to_connect = set([r.gw_key for r in account_routes])
        self.gw_config_table = [c for c in gw_config_table if c.gw_key in self._gws_to_connect]
        self.account_routes = [r for r in account_routes if r.accound_id in self.managed_accounts]
        _msg_source_gw: dict[str, str] = {}  # key: subject, value: gw_key
        for gw_conf in self.gw_config_table:
            if gw_conf.report_endpoint and gw_conf.report_endpoint != "":
                _msg_source_gw[gw_conf.report_endpoint] = gw_conf.gw_key
            if gw_conf.balance_endpoint and gw_conf.balance_endpoint != "":
                _msg_source_gw[gw_conf.balance_endpoint] = gw_conf.gw_key

        self._msg_source_gw = _msg_source_gw

    async def reload_and_restart(self, gw_config_table: list[GWConfigEntry],
                      account_routes: list[OMSRouteEntry]):
        logger.info("reloading and restarting oms subscriber")
        self.reload_config(gw_config_table, account_routes)
        await self.start_gw_subs()


    async def _subscribe_to_gw_subject(self, subject: str, cb):
        if subject not in self.gw_subs:
            gw_sub = await self.nc.subscribe(subject,
                                            cb=cb)
            logger.info(f"subscribed to channel: {subject}")
            self.gw_subs[subject] = gw_sub
        else:
            logger.info(f"already subscribed to order report channel: {subject}; skipping...")

    async def _start_sub(self, gw_entry:GWConfigEntry):
        if gw_entry.gw_key not in self._gws_to_connect:
            logger.info(f"omitting gw config entry for exchange: {gw_entry.gw_key}")
            return
        subject = gw_entry.report_endpoint
        await self._subscribe_to_gw_subject(subject, self.handle_gw_message)

        if gw_entry.balance_endpoint and gw_entry.balance_endpoint != "":
            await self._subscribe_to_gw_subject(gw_entry.balance_endpoint, self.handle_gw_balanceupdate_message)

        if gw_entry.gw_system_endpoint and gw_entry.gw_system_endpoint != "":
            await self._subscribe_to_gw_subject(gw_entry.gw_system_endpoint, self.handle_gw_system_message)


    async def start_gw_subs(self):
        for gw_entry in self.gw_config_table:
            await self._start_sub(gw_entry)

    async def start(self):
        await self.start_gw_subs()
        self.req_sub = await self.nc.subscribe(self.rpc_endpoint,
                                               cb=self.handle_request)


    async def handle_request(self, msg):
        headers = msg.headers
        if headers and "rpc_method" in headers:
            rpc = headers["rpc_method"]
            if rpc == "BatchPlaceOrders":
                await self.handle_batch_order_request(msg)
            elif rpc == "BatchCancelOrders":
                await self.handle_batch_order_cancel(msg)
            elif rpc == "PlaceOrder":
                await self.handle_order_request(msg)
            elif rpc == "CancelOrder":
                await self.handle_order_cancel(msg)
            elif rpc == "QueryOpenOrders":
                await self.handle_open_order_query(msg)
            elif rpc == "QueryAccountBalance":
                await self.handle_account_balance_query(msg)
            elif rpc == "Panic":
                await self.handle_panic(msg)
            elif rpc == "DontPanic":
                await self.handle_dontpanic(msg)
            else:
                logger.warning("received unknown rpc request: " + rpc)
        else:
            logger.error(f"unknown request: {str(msg)}")

    async def handle_gw_message(self, msg):
        raw_data = msg.data # bytes
        subject = msg.subject

        gw_key = self._msg_source_gw.get(subject)

        if gw_key is None:
            raise Exception(f"unknown gw key for subject: {subject}")

        report = gw.OrderReport().parse(raw_data)

        if report.exchange != gw_key:
            logger.warning(f"mismatched exchange gw key in report: should be {gw_key}, but got {report.exchange}")
            report.exchange = gw_key
        # report = raw_data.decode()
        await self.queue.put(report)

    async def handle_gw_balanceupdate_message(self, msg):
        raw_data = msg.data
        balance_update = gw.BalanceUpdate().parse(raw_data)
        await self.queue.put(balance_update)


    async def handle_gw_system_message(self, msg):

        raw_data = msg.data
        gw_system_event = gw.GatewaySystemEvent().parse(raw_data)
        logger.info(f"receiving gw system event: {gw_system_event.to_json(indent=2)}")
        if gw_system_event.event_type == gw.GatewayEventType.GW_EVENT_STARTED:
            # todo: rewrite with better style -_-||
            matched_gws = [entry for entry in self.gw_config_table if entry.gw_system_endpoint == msg.subject]
            if not matched_gws:
                return
            gw_key = matched_gws[0].gw_key
            account_route = [route for route in self.account_routes if route.gw_key == gw_key]
            if not account_route:
                return
            account_id = account_route[0].accound_id

            # TODO: replay the items in the queue
            await sync_with_gw(account_id=account_id, gw_key=gw_key,
                               gw_sender=self.gw_sender, oms_core=self.oms_core)

    async def handle_order_request(self, msg):
        reply = msg.reply
        rpc_req = oms_rpc.OMSPlaceOrderRequest().parse(msg.data)
        req = rpc_req.order_request
        await self.queue.put(req)
        await self.nc.publish(reply, bytes(OK))

    async def handle_order_cancel(self, msg):
        reply = msg.reply
        rpc_req = oms_rpc.OMSCancelOrderRequest().parse(msg.data)
        req = rpc_req.order_cancel_request
        await self.queue.put(req)
        await self.nc.publish(reply, bytes(OK))

    async def handle_batch_order_request(self, msg):
        reply = msg.reply
        rpc_req = oms_rpc.OMSBatchPlaceOrdersRequest().parse(msg.data)
        reqs = rpc_req.order_requests
        await self.queue.put(reqs)
        await self.nc.publish(reply, bytes(OK))

    async def handle_batch_order_cancel(self, msg):
        reply = msg.reply
        rpc_req = oms_rpc.OMSBatchCancelOrdersRequest().parse(msg.data)
        reqs = rpc_req.order_cancel_requests
        await self.queue.put(reqs)
        await self.nc.publish(reply, bytes(OK))

    async def handle_open_order_query(self, msg):
        reply = msg.reply
        rpc_req = oms_rpc.OMSQueryOpenOrderRequest().parse(msg.data)
        account_id = int(rpc_req.account_id)

        open_orders = self.oms_core.get_open_orders(account_id)
        resp = oms_rpc.OMSOrderDetailResponse()
        resp.orders = [o.oms_order_state for o in open_orders]

        await self.nc.publish(reply, bytes(resp))

    async def handle_account_balance_query(self, msg):
        reply = msg.reply
        rpc_req = oms_rpc.OMSQueryAccountRequest().parse(msg.data)
        oms_positions: list[OMSPosition] = None
        if not rpc_req.query_gw:
            oms_positions = self.oms_core.get_account_balance(rpc_req.account_id)
        else:
            pass # todo: query gw or use exch cache from balance mgr?
        resp = oms_rpc.OMSAccountResponse()
        resp.account_id = rpc_req.account_id
        resp.account_balance_entries = [p.position_state for p in oms_positions]

        await self.nc.publish(reply, bytes(resp))


    async def handle_panic(self, msg):
        reply = msg.reply
        rpc_req = oms_rpc.PanicRequest().parse(msg.data)
        await self.queue.put(rpc_req)
        await self.nc.publish(reply, bytes(OK))


    async def handle_dontpanic(self, msg):
        reply = msg.reply
        rpc_req = oms_rpc.DontPanicRequest().parse(msg.data)
        await self.queue.put(rpc_req)
        await self.nc.publish(reply, bytes(OK))

    async def stop(self):
        for gw_sub in self.gw_subs:
            await gw_sub.unsubscribe()
        await self.req_sub.unsubscribe()
        await self.reqc_sub.unsubscribe()
        await self.nc.drain()

class OMSActionHandler:
    def __init__(self, nats_client: nats.NATS,
                 redis_handler: RedisHandler,
                 report_pub_endpoint: str,
                 balance_pub_endpoint: str,
                 system_publish_topic: str,
                 oms_id: str):
        self.nc = nats_client
        self.redis_handler = redis_handler
        self.report_pub_endpoint = report_pub_endpoint
        self.balance_pub_endpoint = balance_pub_endpoint
        self.system_publish_topic = system_publish_topic
        self.oms_id = oms_id

    async def send_order_update(self, action: OMSAction):
        order_update: oms.OrderUpdateEvent = action.action_data

        report_endpoint = self.report_pub_endpoint

        await self.nc.publish(subject=report_endpoint,
                              payload=bytes(order_update))

    async def send_balance_update(self, action: OMSAction):
        balance_update: oms.PositionUpdateEvent = action.action_data
        for pos_snapshot in balance_update.position_snapshots:
            pos_update: oms.PositionUpdateEvent = oms.PositionUpdateEvent()
            pos_update.account_id = balance_update.account_id
            pos_update.position_snapshots = [pos_snapshot]
            pos_update.timestamp = balance_update.timestamp
            symbol = pos_snapshot.instrument_code
            await self.nc.publish(subject=self.balance_pub_endpoint + "." + symbol,
                                  payload=bytes(pos_update))

        for pos in balance_update.position_snapshots:
            asyncio.create_task(self.redis_handler.store_position(pos))

    def save_order(self, action: OMSAction):
        # todo: use asyncio.run_in_executor to run this in a thread pool
        order: OMSOrder = action.action_data
        set_expire = action.action_meta.get("set_expire", False)
        set_open = action.action_meta.get("set_open", False)
        set_closed = action.action_meta.get("set_closed", False)
        _order_data = self.redis_handler.convert_order_to_dict(order)
        asyncio.create_task(self.redis_handler.store_order(
            order_data=_order_data,
            order_id=order.order_id,
            account_id=order.account_id,
            set_expire=set_expire,
            set_open=set_open,
            set_closed=set_closed))

        if set_expire:
            asyncio.create_task(self._send_posttrade_event(order.order_id))


    async def _send_posttrade_event(self, order_id: int):
        posttrade_event = {
            "order_id": order_id,
            "timestamp": int(time.time()*1000),
            "oms_id": self.oms_id,
            "namespace": self.redis_handler.namespace
        }
        await self.nc.publish(subject=POST_TRADE_EVENT_TOPIC,
                              payload=bytes(json.dumps(posttrade_event), encoding="utf-8"))


    async def publish_error(self, action: OMSAction):
        err_msg: str = action.action_data
        err_system_event = oms.OMSSystemEvent(
            event_type="ERROR",
            event_message=err_msg,
            timestamp=int(datetime.datetime.now().timestamp()*1000),
            oms_id=self.oms_id
        )
        await self.nc.publish(subject=self.system_publish_topic,
                              payload=bytes(err_system_event.to_json(), encoding="utf-8"))




async def sync_with_gw(account_id: int, gw_key: str,  oms_core: OMSCore, gw_sender: GWSender,
                       sync_orders=True, sync_balance=True) -> list:
    synced_items = []

    logger.info(f"syncing with GW {gw_key}")

    # note: need to first update orders, then positions
    if sync_orders:
        items = await sync_orders_with_gw(gw_key=gw_key, account_id=account_id, oms_core=oms_core, gw_sender=gw_sender)
        synced_items.extend(items)
    if sync_balance:
        items = await sync_balance_with_gw(gw_key=gw_key, account_id=account_id, oms_core=oms_core, gw_sender=gw_sender)
        synced_items.extend(items)

    return synced_items

async def sync_orders_with_gw(gw_key: str, account_id: int, oms_core: OMSCore, gw_sender: GWSender) -> list:
    '''
    retreive most up-to-date orders from gw services
    convert them into gw events and send to oms core (via internal queue)
    '''

    synced_items = []

    logger.info(f"syncing orders with GW {gw_key}")

    # note: need to first update orders, then positions
    open_orders: list[OMSOrder] = oms_core.get_open_orders(account_id=int(account_id))

    open_order_refs = [
        ExchOrderRef(
            order_id=order.oms_order_state.order_id,
            exch_order_id=order.oms_order_state.exch_order_ref,
            gw_key=order.oms_order_state.gw_key,
            symbol_exch=order.oms_order_state.instrument_exch,
            account_id=order.oms_order_state.account_id
        )
        for order in open_orders if order.exch_order_ref]
    logger.info(f"open orders status syncing for orders: {open_order_refs}")

    for order_ref in open_order_refs:
        try:
            gw_order_reports: list[gw.OrderReport] = await gw_sender.query_orders(
                order_queries=[order_ref],
                gw_key=gw_key)
            for gw_order_report in gw_order_reports:
                synced_items.append(gw_order_report)
        except Exception as e:
            traceback.print_exc()
            logger.error(f"error syncing order {order_ref} with gw {gw_key}: {e}")

    return synced_items


async def sync_balance_with_gw(gw_key: str, account_id: int,  oms_core: OMSCore, gw_sender: GWSender,
                               symbols_to_resync:list[str]=None) -> list:
    synced_items = []

    logger.info(f"syncing position/balance with GW {gw_key}")

    # note: need to first update orders, then positions

    oms_positions = oms_core.get_account_balance(account_id=account_id, use_exch_data=True)
    orig_pos_symbols: set[str] = {(pos.symbol_exch, pos.position_state.instrument_type) for pos in oms_positions}
    # possible_outofsync_positions = [
    #     pos.position_state for pos in oms_positions
    #     if pos.position_state.sync_timestamp is None or pos.position_state.sync_timestamp < ts - 60 * 5 * 1000 ]

    balance_response: gw_rpc.ExchAccountResponse = await gw_sender.query_balances(gw_key=gw_key,
                                                                                  explicit_symbols=symbols_to_resync)
    balance_updates = balance_response.balance_update

    synced_items.append(balance_updates)

    return synced_items




async def load_state_from_datastore(redis_client: RedisHandler) -> tuple[list[OMSOrder], list[OMSPosition]]:
    # load orders from redis
    orders = await redis_client.load_all_orders()

    # TODO: load positions from redis (instead of from GW)
    # load positions from gw services if that gw support balance query
    om_positions: list[OMSPosition] = await redis_client.load_all_positions()

    logger.info(f"{len(orders)} orders loaded for init")
    logger.info(f"{len(om_positions)} positions loaded for init")
    return (orders, om_positions)



async def resync_with_gw(confdata_mgr: ConfdataManager, oms_core: OMSCore, gw_sender: GWSender,
                        sync_order:bool=True, sync_balance:bool=True) -> list:
    items_to_replay = []
    for account_route in confdata_mgr.get_account_routes():
        if int(account_route.account_id) not in confdata_mgr.get_managed_accounts():
            logger.info("omitting account route: " + str(account_route.accound_id))
            continue

        account_id = account_route.accound_id
        gw_key = account_route.gw_key
        if sync_order:
            orders = await sync_orders_with_gw(gw_key=gw_key, account_id=account_id, oms_core=oms_core, gw_sender=gw_sender)
            items_to_replay.extend(orders)
        if sync_balance:
            balances = await sync_balance_with_gw(gw_key=gw_key, account_id=account_id, oms_core=oms_core, gw_sender=gw_sender)
            items_to_replay.extend(balances)

    return items_to_replay

async def reload_refdata(oms_cmd_config: OMSConfig,
                         confdata_mgr: ConfdataManager,
                         db_config_loader: DBConfigLoader,
                         is_init: bool = False):

    logger.info("reloading config...")

    #TODO:  put the refdata loading in a separate awaitable task(so the main process won't be blocked)
    oms_config_entry = db_config_loader.get_oms_config(oms_cmd_config.oms_id)
    all_gw_configs = db_config_loader.load_gw_config()
    all_account_routes = db_config_loader.load_account_routing()
    all_trading_configs = db_config_loader.load_symbol_trading_config()
    all_refdata = db_config_loader.load_refdata()

    logger.info("instrument refdata loaded: " + str(len(all_refdata)) + " items")
    logger.info("gw configs loaded: " + str(len(all_gw_configs)) + " items")
    logger.info("account mapping loaded: " + str(len(all_account_routes)) + " items")
    logger.info("trading configs loaded: " + str(len(all_trading_configs)) + " items")


    logger.info("validating config...")
    try:
        confdata_mgr.validate_config(oms_config_entry, all_account_routes, all_gw_configs, all_refdata, all_trading_configs)
    except Exception as ex:
        logger.error(f"error when validating config: {ex}")
        # TODO: send to notification channel if reloading fails
        if is_init:
            raise ex
        return

    confdata_mgr.reload_config(oms_config_entry, all_account_routes, all_gw_configs, all_refdata, all_trading_configs)




async def main(oms_cmd_config: OMSConfig):

    # resolve config
    db_config_loader = get_db_config_loader(oms_cmd_config)
    oms_config = resolve_oms_config(oms_cmd_config, db_config_loader)

    # setup the infra and core runtime components
    nats_client = await nats.connect(oms_config.pubsub_url)
    redis_client = aioredis.Redis(
        host=oms_config.redis_host,
        port=oms_config.redis_port,
        password=oms_config.redis_password,
        db=0
    )

    # main work queue for OMS
    inbound_event_queue = asyncio.Queue(16)

    # for order place/cancel
    order_action_queue = asyncio.Queue(16)

    # for writing to db/redis etc.
    action_queue = asyncio.Queue(128)

    # setup refdata manager
    confdata_mgr = ConfdataManager(oms_id=oms_cmd_config.oms_id)
    await reload_refdata(oms_cmd_config, confdata_mgr, db_config_loader, is_init=True)
    # refdata = oms_config.instrument_refdata
    # gw_configs = oms_config.gw_config_table
    # account_routes = oms_config.account_routing_table

    oms_core = OMSCore(
        confdata_mgr=confdata_mgr,
        use_time_emulation=False,
        logger_enabled=oms_config.enable_logger,
        risk_check_enabled=oms_config.enable_risk_check,
        handle_non_tq_order_reports=oms_config.enable_non_tq_order_reports)

    gw_sender = GWSender(
        nats_client=nats_client,
        gw_config_table=confdata_mgr.get_gw_configs(),
        report_queue=inbound_event_queue)

    subscriber = OMSSubscriber(
        nats_client=nats_client,
        managed_accounts=list(confdata_mgr.get_managed_accounts()),
        gw_config_table=confdata_mgr.get_gw_configs(),
        account_routes=confdata_mgr.get_account_routes(),
        queue=inbound_event_queue,
        oms_core=oms_core,
        gw_sender=gw_sender,
        rpc_endpoint=oms_config.rpc_endpoint)

    redis_handler = RedisHandler(
        redis_client=redis_client,
        account_ids=list(confdata_mgr.get_managed_accounts()),
        name_space=oms_config.redis_namespace)

    oms_action_handler = OMSActionHandler(
        nats_client=nats_client, redis_handler=redis_handler,
        report_pub_endpoint=oms_config.report_pub_endpoint,
        balance_pub_endpoint=oms_config.balance_pub_endpoint,
        system_publish_topic=oms_config.system_pub_endpoint,
        oms_id=oms_cmd_config.oms_id)

    async def main_process():
        while True:
            msg = await inbound_event_queue.get()
            logger.info("main_process: start to process message")
            try:
                actions = oms_core.process_message(msg)
            except Exception as ex:
                err_msg = f"error when processing message: {traceback.format_exc()}"
                logger.error(err_msg)
                actions = [OMSAction(
                    action_type=OMSActionType.PUBLISH_ERROR,
                    action_meta={},
                    action_data=str(err_msg))]

            if actions:
                logger.info(f"generated {len(actions)} actions")
                for action in actions:
                    if action.action_type in {
                        OMSActionType.SEND_ORDER_TO_GW,
                        OMSActionType.SEND_CANCEL_TO_GW,
                        OMSActionType.BATCH_SEND_ORDER_TO_GW,
                        OMSActionType.BATCH_SEND_CANCEL_TO_GW}:
                        await order_action_queue.put(action)
                    elif action.action_type == OMSActionType.UPDATE_ORDER or \
                        action.action_type == OMSActionType.UPDATE_BALANCE:
                        await action_queue.put(action)
                    elif action.action_type == OMSActionType.PERSIST_ORDER:
                        await action_queue.put(action)

            logger.info(f"msg process completed")
            inbound_event_queue.task_done()

    async def order_process():
        while True:
            action = await order_action_queue.get()
            logger.info(f"action received: {action.action_type}")
            try:
                if oms_config.enable_ordering_relaxing:
                    asyncio.create_task(gw_sender.send_to_gw(action))
                else:
                    await gw_sender.send_to_gw(action)
            except Exception as ex:
                print(ex)
                traceback.print_exc()
            order_action_queue.task_done()

    async def other_action_handler():
        while True:
            action = await action_queue.get()
            logger.info(f"action received: {action.action_type}")
            try:
                if action.action_type == OMSActionType.UPDATE_ORDER:
                    await oms_action_handler.send_order_update(action)
                elif action.action_type == OMSActionType.UPDATE_BALANCE:
                    await oms_action_handler.send_balance_update(action)
                elif action.action_type == OMSActionType.PERSIST_ORDER:
                    oms_action_handler.save_order(action)
                elif action.action_type == OMSActionType.PUBLISH_ERROR:
                    await oms_action_handler.publish_error(action)

            except Exception as ex:
                print(ex)
                traceback.print_exc()
            action_queue.task_done()

    async def periodic_order_resync():
        #account_ids = [ac_route.accound_id for ac_route in account_routes]
        while True:
            await asyncio.sleep(60)
            logger.debug("resync orders...")
            now_ts = int(datetime.datetime.now().timestamp() * 1000)

            # retrieve open orders with pending cancels / pending linkage for some time
            order_refs: list[ExchOrderRef] = oms_core.get_orders_outofsync_maybe()

            if order_refs:
                logger.info("found out-of-sync orders: " + str(order_refs))
                logger.info("start to query GWs for order status")

                order_refs_need_recheck = []
                for order_ref in order_refs:
                    gw_order_reports: list[gw.OrderReport] = await gw_sender.query_orders(
                        order_queries=[order_ref],
                        gw_key=order_ref.gw_key)
                    if gw_order_reports:
                        logger.info(f"{len(gw_order_reports)} report(s) generated from GWs")

                        for gw_order_report in gw_order_reports:
                            await inbound_event_queue.put(gw_order_report)
                    else:

                        # no report received, could be multiple reasons
                        if not order_ref.exch_order_id:
                            order_refs_need_recheck.append(order_ref)
                            logger.info(f"no exch_order_id found for outofsync order {order_ref}; rechecking")

                if len(order_refs_need_recheck) > 0:
                    rechecks = []
                    for ref in order_refs_need_recheck:
                        order_recheck = OrderRecheckRequest()
                        order_recheck.order_id = int(ref.order_id)
                        order_recheck.timestamp = now_ts
                        order_recheck.check_delay_in_sec = 10
                        rechecks.append(order_recheck)
                    async def recheck_order():
                        await asyncio.sleep(10)
                        for recheck in rechecks:
                            await inbound_event_queue.put(recheck)

                    asyncio.create_task(recheck_order())

    async def periodic_resync_balance():

        while True:
            gw_configs_dict: dict[str, GWConfigEntry] = {gw.gw_key: gw for gw in confdata_mgr.get_gw_configs()}
            refdata_dict: dict[str, common.InstrumentRefData] = confdata_mgr.get_instrument_refdata_dict()

            await asyncio.sleep(60)
            logger.debug("resync balance...")
            now_ts = int(datetime.datetime.now().timestamp() * 1000)

            for r in confdata_mgr.get_account_routes():
                acc_id = r.accound_id
                gw_key = r.gw_key

                support_query = gw_configs_dict[gw_key].support_position_query

                if not support_query:
                    continue

                oms_balances = oms_core.get_account_balance(account_id=acc_id, use_exch_data=True)
                need_resync = False
                symbols_to_resync = set()
                for b in oms_balances:
                    if b.position_state.sync_timestamp is None or b.position_state.sync_timestamp < now_ts - 60 * 5 * 1000:
                        need_resync = True
                        if b.position_state.instrument_code in refdata_dict:
                            inst_id_exch = refdata_dict[b.position_state.instrument_code].instrument_id_exchange
                            symbols_to_resync.add(inst_id_exch)

                if need_resync:
                    logger.info(f"resyncing balance for account {acc_id} on exchange {gw_key}")
                    try:
                        balances = await sync_balance_with_gw(gw_key=gw_key, account_id=acc_id,
                                               oms_core=oms_core, gw_sender=gw_sender, symbols_to_resync=list(symbols_to_resync))
                    except Exception as ex:
                        err_msg = traceback.format_exc()
                        logger.error(f"error when syncing balance for account {acc_id} on exchange {gw_key}: {err_msg}")
                        continue

                    for b in balances:
                        await inbound_event_queue.put(b)

    async def period_cleanup():
        while True:
            await asyncio.sleep(60 * 10) # 10 minutes
            logger.debug("periodic cleanup...")
            ts = int(time.time() * 1000)
            cleanup_req = OMSCoreSchedRequest(
                ts_ms=ts,
                request_type=SchedRequestType.CLEANUP
            )
            try:
                inbound_event_queue.put_nowait(cleanup_req)
            except asyncio.QueueFull:
                err_msg = traceback.format_exc()
                logger.error(err_msg)
                logger.info("inbound_event_queue is full, skipping cleanup")



    # OMS init sequence
    # 1. retrieve orders/balances from persistence store;
    all_orders, all_positions = await load_state_from_datastore( redis_client=redis_handler)
    # 2. init OMS core
    oms_core.init_state(curr_orders=all_orders, curr_balances=all_positions)

    items_to_replay = await resync_with_gw(confdata_mgr=confdata_mgr, oms_core=oms_core, gw_sender=gw_sender,
                                           sync_order=True, sync_balance=True)

    await subscriber.start()

    oms_task = asyncio.create_task(main_process())
    gw_sending_task = asyncio.create_task(order_process())
    action_task = asyncio.create_task(other_action_handler())
    resync_task = asyncio.create_task(periodic_order_resync())
    resync_balance_task = asyncio.create_task(periodic_resync_balance())
    cleanup_task = asyncio.create_task(period_cleanup())

    for item in items_to_replay:
        await inbound_event_queue.put(item)


    async def reinit_oms(msg):
        logger.info("reinit oms...")
        ts = int(time.time() * 1000)
        # reload(and validate) refdata from db
        await reload_refdata(oms_cmd_config, confdata_mgr, db_config_loader, is_init=False)

        # reload the gw_sender
        gw_sender.reload_config(confdata_mgr.get_gw_configs())

        # reinit oms core
        reload_request = OMSCoreSchedRequest(
            ts_ms=ts,
            request_type=SchedRequestType.RELOAD_CONFIG
        )
        await inbound_event_queue.put(reload_request)

        # resync balances with all gws
        items_to_replay = await resync_with_gw(
            confdata_mgr=confdata_mgr, oms_core=oms_core, gw_sender=gw_sender,
            sync_order=False, sync_balance=True)
        for item in items_to_replay:
            await inbound_event_queue.put(item)

        # reload subscriber also involve subscribing new channels
        await subscriber.reload_and_restart(
            account_routes=confdata_mgr.get_account_routes(),
            gw_config_table=confdata_mgr.get_gw_configs())

        redis_handler.reload_config(account_ids=list(confdata_mgr.get_managed_accounts()))

    await nats_client.subscribe(f"tq.reload.oms.{oms_cmd_config.oms_id}", cb=reinit_oms)


async def shutdown(signal_, loop):
    print(f"Received {signal_.name} signal. Shutting down...")
    tasks = [t for t in asyncio.all_tasks() if t is not asyncio.current_task()]

    for task in tasks:
        task.cancel()

    with suppress(asyncio.CancelledError):
        await asyncio.gather(*tasks, return_exceptions=True)

    loop.stop()


if __name__ == "__main__":
    from tqrpc_utils.config_utils import try_load_config

    oms_config: OMSConfig = try_load_config(OMSConfig)

    loop = asyncio.get_event_loop()

    # Register signal handlers for graceful shutdown
    signals = (signal.SIGTERM, signal.SIGINT)
    for s1 in signals:
        loop.add_signal_handler(s1, lambda s=s1: asyncio.create_task(shutdown(s, loop)))

    try:
        main_task = loop.create_task(main(oms_config))
        loop.run_until_complete(main_task)
        loop.run_forever()
    except KeyboardInterrupt:
        print("Interrupted by user.")
    finally:
        loop.close()
