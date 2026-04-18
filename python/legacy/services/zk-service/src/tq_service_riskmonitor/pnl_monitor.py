import asyncio
from dataclasses import dataclass

from zk_client.tqclient import TQClient, TQClientConfig, create_client
import zk_proto_betterproto.oms as oms
from tqrpc_utils import config_utils

from tq_service_riskmonitor.pnl_calculator import PnlCalculator, PnlEntry

import betterproto
#from slack import WebClient
import logging

from loguru import logger

@dataclass
class PnlRuleConfigEntry:
    account_id: int = None
    max_loss: float = None

@dataclass
class PnlCircuitBreakerRuleConfig:
    rule_entries: list[PnlRuleConfigEntry] = None



@dataclass
class PnLMonitorCmdConfig:
    nats_url: str = None
    mongo_url_base: str = None
    mongo_uri: str = None
    db_name: str = None
    rule_key: str = None
    enable_slack_notification: bool = False
    notification_channel: str = None



class PnlCircuitBreaker:

    def __init__(self, config: PnlCircuitBreakerRuleConfig,
                 tq_client: TQClient,
                 source_id: str,
                 notification_channel: str = None):
        self.config = config
        self.tq_client = tq_client
        self.notification_channel = notification_channel
        self.source_id = source_id
        self._circuitbreak_status: dict[int, bool] = {}

        self._account_rules: dict[int, PnlRuleConfigEntry] = {}

        for rule_entry in config.rule_entries:
            self._circuitbreak_status[rule_entry.account_id] = False
            self._account_rules[rule_entry.account_id] = rule_entry

    async def on_pnl(self, pnl_entry: PnlEntry):
        account_id = pnl_entry.account_id
        rule_entry = self._account_rules[account_id]
        if account_id in self._circuitbreak_status:
            if self._circuitbreak_status[account_id] == False:
                if pnl_entry.realized_pnl < -rule_entry.max_loss:
                    logger.info(f"circuit breaker triggered for account: {account_id}")
                    reason = f"Realized PnL {pnl_entry. realized_pnl} exceeds loss -{rule_entry.max_loss} for account {account_id}"
                    logger.info(reason)
                    await self.tq_client.panic(
                        account_id=account_id,
                        reason=reason,
                        source_id=self.source_id)
                    self._circuitbreak_status[account_id] = True

                    notify_msg = f"Account {account_id} has triggered circuit breaker. Reason: {reason}"
                    await self.tq_client.publish(
                        subject=self.notification_channel,
                        payload=bytes(notify_msg, 'utf-8')
                    )
                    # if self.slack_client:
                    #     self.slack_client.send_notification(
                    #         channel=self.slack_channel,
                    #         message=f"Account {account_id} has triggered circuit breaker. Reason: {reason}"
                    #     )






class RiskMonitor:

    def __init__(self, tq_client: TQClient,
                 circuit_breaker_config: PnlCircuitBreakerRuleConfig,
                 source_id: str,
                 notification_channel: str = None):
        self.client = tq_client

        self.pnl_calc = PnlCalculator()
        self.pnl_circuit_breaker = PnlCircuitBreaker(
            config=circuit_breaker_config,
            tq_client=self.client,
            source_id=source_id,
            notification_channel=notification_channel)

    async def start(self):

        await self.client.start()
        await self.client.subscribe_order_update(self.on_orderupdate)



    async def on_orderupdate(self, order_update: oms.OrderUpdateEvent):
        if betterproto.serialized_on_wire(order_update.last_trade):
            trade = order_update.last_trade
            pnl_entry = self.pnl_calc.on_trade(trade)
            logger.info(pnl_entry)

            if self.pnl_circuit_breaker:
                await self.pnl_circuit_breaker.on_pnl(pnl_entry)

    def run(self):
        pass


def _load_rule_from_db(mongo_url:str, db_name: str, rule_key:str):
    from pymongo import MongoClient

    client = MongoClient(mongo_url)
    db = client[db_name]
    collection = db["riskcheck_rule"]

    rule = collection.find_one({"rule_key": rule_key})

    if rule is None:
        raise Exception(f"Rule with key {rule_key} not found")

    rule_config = PnlCircuitBreakerRuleConfig()
    rule_config.rule_entries = []

    for rule_entry in rule['rule_config']["rule_entries"]:
        rule_config.rule_entries.append(
            PnlRuleConfigEntry(
                account_id=rule_entry["account_id"],
                max_loss=rule_entry["max_loss"]
            )
        )

    return rule_config


def run():
    loop = asyncio.get_event_loop()
    app_config: PnLMonitorCmdConfig = config_utils.try_load_config(PnLMonitorCmdConfig)


    pnl_rule_config = _load_rule_from_db(
        mongo_url=app_config.mongo_url_base + "/" + app_config.mongo_uri,
        db_name=app_config.db_name,
        rule_key=app_config.rule_key
    )

    account_ids = list(set([entry.account_id for entry in pnl_rule_config.rule_entries]))
    logger.info("querying client config for " + str(account_ids))
    tq_client = loop.run_until_complete(create_client(
        nats_url=app_config.nats_url,
        account_ids=account_ids,
        source_id=app_config.rule_key
    ))

    logger.info("tq client config: " + str(tq_client.config))

    logger.info(pnl_rule_config)

    notification_channel = app_config.notification_channel

    pnl_monitor = RiskMonitor(tq_client=tq_client,
                              circuit_breaker_config=pnl_rule_config,
                              source_id=app_config.rule_key,
                              notification_channel=notification_channel)



    loop.run_until_complete(pnl_monitor.start())

    loop.run_forever()


if __name__ == "__main__":
    run()




