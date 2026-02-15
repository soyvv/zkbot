import asyncio
from loguru import logger
import time

from datetime import datetime, timedelta
from nats import NATS
from dataclasses import dataclass

from zk_datamodel import oms
from tq_service_riskmonitor.monitor_utils import *
import traceback
import json


"""
Sample OrderUpdate Msg:
Order Update
{"orderId": "7171085362914308096", 
"accountId": "9716", 
"orderSnapshot": 
    {"orderId": "7171085362914308096", 
    "exchOrderRef": "12327573465", 
    "gwKey": "hyperliquid", 
    "accountId": "9716", 
    "snapshotVersion": 5, 
    "orderStatus": "ORDER_STATUS_BOOKED", 
    "instrument": "MATIC-P/USDC@HYPERLIQUID", 
    "instrumentExch": "MATIC", 
    "buySellType": "BS_BUY", 
    "openCloseType": 
    "OC_OPEN", 
    "price": 0.8, 
    "qty": 12.0, 
    "filledAvgPrice": 0.8, 
    "sourceId": "APP", 
    "createdAt": "1709719982000", 
    "updatedAt": "1709775759691"}, 
"timestamp": "1709775759000", 
"orderSourceId": "APP"}
"""


class OrderUpdateMonitor:
    """
    Monitor OrderUpdates endpoint from OMS
    """

    def __init__(
        self,
        nats_url: str,
        # target_subject: str,
        oms_id: int,
        # notify_subject: str,
        action: MonitorAction,
        watchers: dict,
        account_id: int,
    ):
        self.nats_url = nats_url
        self.oms_id = oms_id
        self.target_subject = self.__load_target_subject()
        # self.notify_subject = notify_subject  # nats topic for slack notification hub
        self.action = action

        self.nats = NATS()
        self.watcher_names = ["reject_watcher", "booked_watcher", "volume_watcher"]
        self.watchers = self.__load_watchers(watchers)
        self.rejects = set()
        self.instrument_fills = {}
        self.account_id = account_id
        # self.account_id = None

    def __load_target_subject(self):
        _ss = f"tq.oms.service.{self.oms_id}.order_update"
        return _ss

    
    # to check watchers loaded
    # deprecated
    def __check_watcher_config(self, config) -> bool:
        # loaded_names = [watcher for watcher in config]

        # if self.watcher_names == loaded_names:
        #     return True
        # else:
        #     logger.error(f"Watcher config is incorrect. Expected: {self.watcher_names}. Got: {loaded_names}")
        #     return False
        for _name in config:
            if _name not in self.watcher_names:
                logger.error(f"Unexpected watcher name: {_name}")
                return False
        return True

    def __load_watchers(self, watchers: dict):
        
        watchers_ = {}
        # res = self.__check_watcher_config(watchers)
        # if not res:
        #     exit(1)

        for name, config in watchers.items():
            watchers_[name] = config.copy()
            watchers_[name]["timer"] = datetime.now() + timedelta(
                seconds=config["time_interval_to_monitor"]
            )

        if "reject_watcher" in watchers:
            watchers_["reject_watcher"]["count"] = 0

        if "booked_watcher" in watchers:
            watchers_["booked_watcher"]["booked"] = False

        return watchers_

    async def __on_msg(self, msg):

        data = msg.data
        order_update = json.loads(oms.OrderUpdateEvent().parse(data).to_json())
        # logger.info(f"OrderUpdateInfo: {order_update}")

        order_snapshot = order_update["orderSnapshot"]
        account_id = int(order_update["accountId"])

        order_status = order_snapshot["orderStatus"]
        # logger.info(f"{self.account_id}: order status: {order_status}")

        if account_id != self.account_id:
            # logger.info(f"debug, {account_id}, {self.account_id}")
            return

        order_id = order_update["orderId"]
        instrument = order_snapshot["instrument"]

        # for reject watcher
        if order_status == "ORDER_STATUS_REJECTED":
            if order_id not in self.rejects:
                # logger.info(f"rejected order: {order_id}")
                if "reject_watcher" in self.watchers:
                    self.watchers["reject_watcher"]["count"] += 1
                    self.rejects.add(order_id)

        # for volume watcher
        elif (
            order_status == "ORDER_STATUS_FILLED"
            or order_status == "ORDER_STATUS_PARTIALLY_FILLED"
        ):
            if "volume_watcher" in self.watchers:
                # logger.info(f"OrderUpdateInfo: {order_update}")
                if instrument not in self.instrument_fills:
                    # self.instrument_fills[instrument] = {"lastUpdate": order_snapshot["updatedAt"],"fill_qty": order_snapshot["filledQty"]}
                    self.instrument_fills[instrument] = {
                        "lastUpdate": order_snapshot["updatedAt"],
                        "fill_count": 1,
                    }
                else:
                    # check that OrderUpdateEvent for fills are not duplicates
                    if (order_snapshot["updatedAt"] > self.instrument_fills[instrument]["lastUpdate"]):
                        # self.instrument_fills[instrument]["fill_qty"] += order_snapshot["filledQty"]
                        
                        self.instrument_fills[instrument]["fill_count"] += 1
                        self.instrument_fills[instrument]["lastUpdate"] = order_snapshot["updatedAt"]

        # for booked watcher
        if "booked_watcher" in self.watchers and not self.watchers["booked_watcher"]["booked"]:
            if order_status == "ORDER_STATUS_BOOKED":
                # logger.info(f"Booked order for new timespan: {order_id}")
                self.watchers["booked_watcher"]["booked"] = True

    async def __notify(self, message: str, watcher_name: str):
        """Notify slack via tq_service_helper.notifiction_hub.
        Notificiation hub must be running and configured for notification."""
        # await self.nats.publish(self.notify_subject, message.encode())
        
        # to update different channels based on notify_subject field in rule config
        notify_subject = self.watchers[watcher_name]['notify_subject']
        await self.nats.publish(notify_subject, message.encode())

    async def __connect(self):
        await self.nats.connect(self.nats_url)
        await self.nats.subscribe(self.target_subject, cb=self.__on_msg)
        logger.info(
            f"Connected to {self.nats_url} and subscribed to {self.target_subject}."
        )

    # return give_alert
    async def __check_reject(self):
        watcher = self.watchers["reject_watcher"]
        rejected_orders = self.rejects

        if watcher["count"] > watcher["max_rejects"]:
            msg = f"Account id: {self.account_id} \n No. of rejected orders: {watcher['count']} \n In the past: {watcher['time_interval_to_monitor']} seconds \n Limit is {watcher['max_rejects']} \n Rejected orderIds: {rejected_orders}"

            # refresh counter & order_id set
            self.watchers["reject_watcher"]["count"] = 0
            self.rejects = set()
            return msg, True
        return None, False

    # check give_alert
    async def __check_booked(self):

        # watcher = self.watchers["booked_watcher"]
        # # logger.info(f"Checking for booked order... {watcher['booked']}")

        if self.watchers["booked_watcher"]["booked"]:
            self.watchers["booked_watcher"]["booked"] = False
            return None, False

        msg = f"Account id: {self.account_id} \n Current Timestamp: {datetime.now()} \n No booked orders in the past {self.watchers['booked_watcher']['time_interval_to_monitor']} seconds."
        return msg, True

    async def __check_volume(self):

        watcher = self.watchers["volume_watcher"]
        msgs = []
        _alert = False

        # logger.info(f"Account id: {self.account_id}. instrument fills: {self.instrument_fills}")

        for instrument, v in self.instrument_fills.items():
            qty = v["fill_count"]
            # logger.info(f"account id: {self.account_id}. instru: {instrument}. qtyFill: {qty}")
            if qty > watcher["max_fill_count_per_sym"]:
                msg = f"Account id: {self.account_id} \n Current Timestamp: {datetime.now()} \n Instrument: {instrument} \n Filled qty of {qty} \n Exceeded limit of: {watcher['max_fill_count_per_sym']} \n In timeframe of: {watcher['time_interval_to_monitor']} seconds."
                msgs.append(msg)
                if not _alert:
                    _alert = True
            # reset qty counter
            self.instrument_fills[instrument] = {"lastUpdate": "0", "fill_count": 0}

        return msgs, _alert

    async def __stagger(self, watcher_name):

        curr_time = datetime.now()
        # logger.info(f"acc: {self.account_id} | watcher: {watcher_name}")
        while curr_time < self.watchers[watcher_name]["timer"]:
            await asyncio.sleep(
                (self.watchers[watcher_name]["timer"] - curr_time).seconds
            )
            curr_time = datetime.now()

    async def __wait(self, watcher_name: str):

        while True:

            await self.__stagger(watcher_name)
            _give_alert = None

            if watcher_name == "reject_watcher":
                msg, _give_alert = await self.__check_reject()
            elif watcher_name == "booked_watcher":
                msg, _give_alert = await self.__check_booked()
            elif watcher_name == "volume_watcher":
                msg, _give_alert = await self.__check_volume()
            else:
                logger.error(
                    "Invalid Watcher Name detected. Please check watcher configs."
                )
                exit(1)
            if _give_alert:
                if msg is None:
                    logger.error("Error. Invalid watcher name.")

                if self.action == MonitorAction.NOTIFY:
                    if isinstance(msg, list):
                        for _m in msg:
                            logger.info(_m)
                            asyncio.create_task(self.__notify(_m, watcher_name))
                    else:
                        logger.info(msg)
                        await self.__notify(msg, watcher_name)
                else:
                    logger.error(
                        f"Verify Monitor Action in config. Expected {MonitorAction.NOTIFY} but got {self.action}"
                    )
                    logger.info(f"Notif msg: {msg}")
            self.watchers[watcher_name]["timer"] = datetime.now() + timedelta(
                seconds=self.watchers[watcher_name]["time_interval_to_monitor"]
            )

    async def start(self):

        await self.__connect()
        logger.info(f"Monitoring on message from Nats topic-[{self.target_subject}].")
        # logger.info(f"Loading OrderMonitor for account_id: {self.account_id}")
        watchers_loaded = [name_ for name_ in self.watchers]
        logger.info(f"OpenOrderMonitor Watchers loaded for account_id {self.account_id} : {watchers_loaded}")
        watchers_json = json.dumps(self.watchers, indent=2, default=str)
        logger.info(f"Watchers config: {watchers_json}")

        if self.watchers is not None:
            for _name in self.watchers:
                asyncio.create_task(self.__wait(_name))
        else:
            logger.error("Watchers config not loaded. Please check")


if __name__ == "__main__":

    app_config, rule_configs, oms_mapping, oms_watcher = order_monitor_config_load()

    NATS_URL = app_config.nats_url
    MONGO_URL = app_config.mongo_url
    DB_NAME = app_config.db_name

    logger.info(f"app config: {app_config}")
    logger.info(f"Oms -> account mapping: {oms_mapping}")
    config = rule_configs[0]

    loop = asyncio.new_event_loop()

    logger.info(f"OMS_MAPPING: {oms_mapping}")

    for oms_id, managed_accounts in oms_mapping.items():
        watchers = {}
        # breakpoint()
        for rule in rule_configs:
            if oms_id in oms_watcher and rule['name'] in oms_watcher[oms_id]:
                watchers[rule["name"]] = rule

        logger.info(f"Oms_id: {oms_id}, Managed accounts: {managed_accounts}")
        logger.info(f"Number of watchers/configs loaded: {len(watchers)}")

        for _acc_id in managed_accounts:
            order_monitor = OrderUpdateMonitor(
                nats_url=NATS_URL,
                oms_id=oms_id,
                # notify_subject=config["notify_subject"],
                action=MonitorAction[config["action"]],
                watchers=watchers.copy(),
                account_id=_acc_id,
            )
            loop.create_task(order_monitor.start())

    loop.run_forever()
