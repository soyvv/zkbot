import asyncio
from dataclasses import dataclass
import datetime
from loguru import logger
import pymongo
import time
import schedule

from tqrpc_utils.config_utils import try_load_config
from zk_proto_betterproto.oms import Order, OrderStatus, OrderUpdateEvent, Trade
from tq_service_oms.trade_backfiller import TradeBackfiller, AppConfig, OrderRequest
from tq_service_helper.slack_notifier import SlackChannelNotifier


class MongoDBClient:
    '''
    Mongo DB Client for trade update monitor
    '''
    
    UPDATE_EVENT_TABLE_NAME = "order_events"
    TRADE_TABLE_NAME = "trade"
    SNAPSHOT_TABLE_NAME = "orders"

    def __init__(self, mongo_url: str, db_name, logger=logger):
        self.client = pymongo.MongoClient(mongo_url)
        self.db = self.client[db_name]
        self.update_event_table = self.db[self.UPDATE_EVENT_TABLE_NAME]
        self.snapshot_table = self.db[self.SNAPSHOT_TABLE_NAME]
        self.trade_table = self.db[self.TRADE_TABLE_NAME]
        self.logger = logger

    def get_trade_total_filled_qty_by_order_id(self, order_id):
        # sum all filled_qty of all trades of order_id
        results = list(self.trade_table.aggregate([
            {"$match": {"order_id": str(order_id)}},
            {"$group": {
                "_id": "$order_id",
                "filled_qty": {"$sum": "$filled_qty"}}}
        ]))
        
        if len(results) > 1:
            raise ValueError(f"Expected query to give to at most 1 result but got {len(results)}.")
        
        if not results:
            return None
        
        return list(results)[0].get('filled_qty')
 
    def get_distinct_orders_from_log_in_window(self, window_sec:int, offset_sec:int):
        end_ts = (time.time() - offset_sec) * 1000
        start_ts = end_ts - window_sec * 1000
        self.logger.info(f"Looking for latest terminal order events between {start_ts} and {end_ts}]...")
        pipeline = [
            {"$match": {
                "filled_qty": {"$ne": 0}, # records where qty is not equal to 0
                "updated_at": {
                    "$gte": start_ts,
                    "$lte": end_ts}, # within window
                "order_status": {"$in":
                    [OrderStatus.ORDER_STATUS_CANCELLED.name,
                    OrderStatus.ORDER_STATUS_REJECTED.name,
                    OrderStatus.ORDER_STATUS_FILLED.name]}, # records in terminal states
                }},
            {"$sort": {"updated_at": -1}}, # sort by latest event
            {"$group": {
                "_id": "$order_id",
                "latest_timestamp": {"$first": "$updated_at"},
                "doc": {"$first": "$$ROOT"}}},
            {"$replaceRoot": {"newRoot": "$doc"}} # get by latest update time
        ]
        return self.update_event_table.aggregate(pipeline)
    
    def get_all_order_snapshots_from_snapshots_in_window(self, window_sec:int, offset_sec:int):
        end_ts = (time.time() - offset_sec) * 1000
        start_ts = end_ts - window_sec * 1000
        self.logger.info(f"Looking for terminal order snapshots between {start_ts} and {end_ts}]...")
        return self.snapshot_table.find(filter={
            "filled_qty": {"$ne": 0}, # records with non-zero filled qty
            "updated_at": {
                "$gte": start_ts,
                "$lte": end_ts}, # within window
            "order_status": {
                "$in": [OrderStatus.ORDER_STATUS_CANCELLED.name,
                        OrderStatus.ORDER_STATUS_REJECTED.name,
                        OrderStatus.ORDER_STATUS_FILLED.name]} # records in terminal states
            })

@dataclass
class TradeUpdateConfig:
    mongo_url_base: str = None,
    # mongo_uri: str = None,
    db_name: str = None,
    config_db_name: str = None,
    nats_url: str = None,
    window_sec: int = None,
    offset_sec: int = None,
    interval_sec: int = None,
    order_source: str = None, # (LOG, SNAPSHOT)
    slack_token: str = None,
    slack_channel: str = None,
    dry_run: bool = None


class TradeUpdateMonitor:
    def __init__(self, config: TradeUpdateConfig, logger=logger):
        if config.order_source not in ("SNAPSHOT", "LOG"):
            raise ValueError(f"Invalid value for order_source: {config.order_source}. Must be one of (SNAPSHOT, LOG).")
        
        self.mongo_url_base: str = config.mongo_url_base
        self.mongo_uri: str = ""
        self.db_name: str = config.db_name
        self.config_db_name: str = config.config_db_name
        self.nats_url: str = config.nats_url
        self.window_sec: int = config.window_sec
        self.offset_sec: int = config.offset_sec
        self.order_source: str = config.order_source
        self.slack_token: str = config.slack_token
        self.slack_channel: str = config.slack_channel
        self.dry_run: bool = config.dry_run
        
        self.logger = logger        
        self.db_client = MongoDBClient(mongo_url=self.mongo_url_base, db_name=self.db_name)
        self.trade_backfiller = TradeBackfiller(
            AppConfig(
                nats_url=self.nats_url,
                mongo_url_base=self.mongo_url_base,
                mongo_uri=self.mongo_uri,
                mongo_config_dbname=self.config_db_name,
                mongo_dbname=self.db_name),
            logger=logger)
        self.slack_notifier = SlackChannelNotifier(token=self.slack_token, channel=self.slack_channel)
        
        self.slack_messages = []

    async def validate_order_with_log(self, order_log: dict):
        order_update_event = OrderUpdateEvent().from_dict(order_log)
        order = order_update_event.order_snapshot
        trade_filled_qty = self.db_client.get_trade_total_filled_qty_by_order_id(order_update_event.order_id)
        
        if trade_filled_qty is None:
            msg = f"No trade found for order id-[{order_update_event.order_id}]"
            self.logger.info(msg)
            self.slack_messages.append(msg)
            
            if self.dry_run:
                self.logger.info(f"[DRY-RUN] Syncing missing trade for order id-[{order_update_event.order_id}]. Running trade_backfiller...")
                return
            
            order_request: OrderRequest = OrderRequest(exch_order_ref=order.exch_order_ref, instrument_tq=order.instrument, source_id=order.source_id)
            msg = f"Syncing missing trade for order id-[{order_update_event.order_id}]. Running trade_backfiller..."
            self.logger.info(msg)
            self.slack_messages.append(msg)
            await self.trade_backfiller.run(account_id=order.account_id, orders=[order_request], is_dry_run=self.dry_run)
            return
        
        # validate against trade
        if abs(float(trade_filled_qty) - float(order.filled_qty)) < 1e-6:
            self.logger.info(f"Trade filled_qty matched for order id-[{order_update_event.order_id}]")
            return
        
        # filled qty not matched
        msg = f"Trade filled_qty [{trade_filled_qty}] not matched with order filled_qty [{order.filled_qty}] for order id-[{order_update_event.order_id}]"
        self.logger.info(msg)
        self.slack_messages.append(msg)
        
        if self.dry_run:
            self.logger.info(f"[DRY-RUN] Running trade_backfiller for order id-[{order_update_event.order_id}]...")
            return
        
        order_request: OrderRequest = OrderRequest(exch_order_ref=order.exch_order_ref, instrument_tq=order.instrument, source_id=order.source_id)
        msg = f"Running trade_backfiller for order id-[{order_update_event.order_id}]..."
        self.logger.info(msg)
        self.slack_messages.append(msg)
        await self.trade_backfiller.run(account_id=order.account_id, orders=[order_request], is_dry_run=self.dry_run)

    async def validate_order_with_snapshot(self, order_snapshot:dict):
        order_id = order_snapshot['order_id']
        order = Order().from_dict(order_snapshot['order_state'])
        trade_filled_qty = self.db_client.get_trade_total_filled_qty_by_order_id(order_id)
        
        if trade_filled_qty is None:
            msg = f"No trade found for order id-[{order_id}]"
            self.logger.info(msg)
            self.slack_messages.append(msg)
            
            if self.dry_run:
                self.logger.info(f"[DRY-RUN] Syncing missing trade for order id-[{order_id}]. Running trade_backfiller...")
                return
            
            order_request: OrderRequest = OrderRequest(exch_order_ref=order.exch_order_ref, instrument_tq=order.instrument, source_id=order.source_id)
            msg = f"Syncing missing trade for order id-[{order_id}]. Running trade_backfiller..."
            self.logger.info(msg)
            self.slack_messages.append(msg)
            await self.trade_backfiller.run(account_id=order.account_id, orders=[order_request], is_dry_run=self.dry_run)
            return
        
        # validate against trade
        if abs(float(trade_filled_qty) - float(order_snapshot['filled_qty'])) < 1e-6:
            self.logger.info(f"Trade filled_qty matched for order id-[{order_id}]")
            return
        
        # filled qty not matched
        msg = f"Trade filled_qty [{trade_filled_qty}] not matched with order filled_qty [{order_snapshot['filled_qty']}] for order id-[{order_id}]"
        self.logger.info(msg)
        self.slack_messages.append(msg)
        
        if self.dry_run:
            self.logger.info(f"[DRY-RUN] Running trade_backfiller for order id-[{order_id}]...")
            return
        
        order_request: OrderRequest = OrderRequest(exch_order_ref=order.exch_order_ref, instrument_tq=order.instrument, source_id=order.source_id)
        msg = f"Running trade_backfiller for order id-[{order_id}]..."
        self.logger.info(msg)
        self.slack_messages.append(msg)
        await self.trade_backfiller.run(account_id=order.account_id, orders=[order_request], is_dry_run=self.dry_run)
    
    async def run_with_log(self):
        order_logs = list(self.db_client.get_distinct_orders_from_log_in_window(self.window_sec, self.offset_sec))
        self.logger.info(f"Order ids found from latest terminal order update events: {[order_log['order_id'] for order_log in order_logs]}")
        for order_log in order_logs:
            await self.validate_order_with_log(order_log)
    
    async def run_with_snapshot(self):
        order_snapshots = list(self.db_client.get_all_order_snapshots_from_snapshots_in_window(self.window_sec, self.offset_sec))
        
        self.logger.info(f"Order ids of terminal orders from order snapshots: {[order_snapshot['order_id'] for order_snapshot in order_snapshots]}")
        for order_snapshot in order_snapshots:
            await self.validate_order_with_snapshot(order_snapshot)

    async def run(self):
        await self.trade_backfiller.connect()

        if self.order_source == "LOG":
            await self.run_with_log()
        elif self.order_source == "SNAPSHOT":
            await self.run_with_snapshot()
        
        if len(self.slack_messages) > 0:
            if self.dry_run:
                self.logger.info(f"[DRY-RUN] Sent trade mismatch messages to Slack.")
            else:
                slack_response = self.slack_notifier.notify("\n".join(self.slack_messages))
                self.logger.info(f"Sent trade mismatch messages to Slack. Slack response: {slack_response}")

            # clear message queue
            self.slack_messages = []


if __name__ == '__main__':
    config: TradeUpdateConfig = try_load_config(TradeUpdateConfig)
    
    monitor = TradeUpdateMonitor(config=config, logger=logger)
    logger.info(f"Creating trade update monitor with monitor window-[{config.window_sec}]sec, offset-[{config.offset_sec}]sec, schedule interval-[{config.interval_sec}]sec, order source-[{config.order_source}], dry-run mode-[{config.dry_run}]...")
    schedule.every(config.interval_sec).seconds.do(lambda: asyncio.run(monitor.run()))
    
    # run immediately when starting
    schedule.run_all()
    while True:
        n = schedule.idle_seconds()
        if n is None:
            logger.info(f"No more jobs scheduled next. Stopping...")
            break
        elif n > 0:
            # sleep exactly the right amount of time
            logger.info(f"Next trade update monitor job in {datetime.timedelta(seconds=n)}...")
            time.sleep(n)
        schedule.run_pending()