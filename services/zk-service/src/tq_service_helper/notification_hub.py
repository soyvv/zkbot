import argparse
import asyncio
import datetime
import logging
from dataclasses import dataclass
from tqrpc_utils.config_utils import try_load_config

from nats.aio.client import Client as NATS
import slack_sdk
from pymongo import MongoClient

@dataclass
class AppConfig:
    mongo_uri: str = None
    config_db_name: str = None
    config_table_name: str = None
    nats_url: str = None
    slack_token: str = None
    dry_run: bool = None


class NatsSlackNotifier:
    def __init__(self,
                 nats_url:str,
                 nats_topic:str,
                 slack_token:str,
                 slack_channel:str,
                 limit:int,
                 interval_sec:int,
                 dry_run:bool=True):
        """
        Initializes the object with the provided parameters.
        
        Args:
            nats_url (str): The URL of the NATS server.
            nats_topic (str): The NATS topic to subscribe to.
            slack_token (str): The token for accessing the Slack API.
            slack_channel (str): The default Slack channel name to post notifications to.
            limit (int): The maximum number of notifications which will be sent to the Slack channel per interval. Set to 0 to send all messages.
            interval_sec (int): The length of the interval in seconds.
            dry_run (bool): Whether to run in dry-run mode. Dry-run mode will not send any messages to the Slack channel.
        """
        self.nats_topic = nats_topic
        self.nats_url = nats_url
        
        self.slack_client = slack_sdk.WebClient(token=slack_token)
        self.slack_client.api_test()
        self.slack_channel = slack_channel
        
        self.limit = limit
        self.interval_sec = interval_sec
        self.dry_run = dry_run
        
        self.notified_count = 0
        self.dropped_count = 0
        self.next_check_time = datetime.datetime.now() + datetime.timedelta(seconds=self.interval_sec)
        
        self.task = None
    
    def __flush_dropped_count(self):
        if self.dropped_count > 0:
            if not self.dry_run:
                self.slack_client.chat_postMessage(
                    channel=self.slack_channel,
                    text=f"{self.dropped_count} more messages received from topic-{self.nats_topic} and omitted since last notification.",
                )
            else:
                logging.info(f"[DRY-RUN] {self.dropped_count} more messages received from topic-{self.nats_topic} and omitted since last notification.")
            
            self.dropped_count = 0
    
    async def __on_msg(self, msg):
        message = msg.data.decode('utf-8')
                
        # process message for specific topic
        if self.nats_topic.startswith("tq.strategy.notify."):
            headers = msg.headers
            if headers is None:
                logging.info(f"No message headers. Dropping message: {message}")
                return
            
            destination = headers.get("DESTINATION", None)
            if destination != "SLACK":
                logging.info(f"Message does not require Slack notification. Dropping message: {message}")
                return
            
            topic = headers.get("TOPIC", None)
            self.slack_channel = topic if topic else self.slack_channel
        
        logging.info(f"Received message from topic {self.nats_topic}: {message}")
        
        if datetime.datetime.now() >= self.next_check_time:
            # notify about dropped message count
            self.__flush_dropped_count()
            self.notified_count = 0
            self.next_check_time = datetime.datetime.now() + datetime.timedelta(seconds=self.interval_sec)
        
        if self.notified_count < self.limit or self.limit == 0: # always notify if limit == 0
            # notify about new message
            if not self.dry_run:
                self.slack_client.chat_postMessage(
                    channel=self.slack_channel,
                    text=message)
                logging.info(f"Notified to slack channel-{self.slack_channel}: {message}")
            else:
                logging.info(f"[DRY-RUN] Notified to slack channel-{self.slack_channel}: {message}")
            self.notified_count += 1
            
        else:
            # drop messages above limit
            self.dropped_count += 1
            logging.info(f"Dropped from topic-{self.nats_topic}: {message}")
    
    async def __on_close(self):
        self.__flush_dropped_count()
    
    async def connect(self):
        """
        Runs the Slack notifier.
        """
        try:
            nc = NATS()
            async def __on_error(e):
                logging.error(e, exc_info=True)
                self.stop()
            await nc.connect(servers=self.nats_url, closed_cb=self.__on_close, error_cb=__on_error)
            await nc.subscribe(subject=self.nats_topic, cb=self.__on_msg)
        except Exception as e:
            logging.exception(e, exc_info=True)
    
    """Start the nats slack notifier."""
    async def start(self, loop:asyncio.AbstractEventLoop):
        self.task = loop.create_task(self.connect())
        logging.info(f"Starting {self.nats_topic} listener.")

    """Stop the nats slack notifier."""
    def stop(self):
        if self.task:
            self.task.cancel()
        logging.info(f"Stopping {self.nats_topic} listener.")


if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO,format="%(asctime)s [%(levelname)s] %(message)s")
    
    # argparse
    app_config: AppConfig = try_load_config(AppConfig)
    
    MONGO_URL = app_config.mongo_uri
    CONFIG_DB_NAME = app_config.config_db_name
    CONFIG_TABLE_NAME = app_config.config_table_name
    NATS_URL = app_config.nats_url
    SLACK_TOKEN = app_config.slack_token
    DRY_RUN = app_config.dry_run

    # get configs from db
    mongo_client = MongoClient(MONGO_URL)
    config_db = mongo_client[CONFIG_DB_NAME]
    config_table = config_db[CONFIG_TABLE_NAME]
    configs = config_table.find()
    
    if configs is None:
        logging.info("No config found. Exiting...")
        exit(0)
    
    notifiers = list[NatsSlackNotifier]()
    for config in configs:
        notifiers.append(
            NatsSlackNotifier(
                nats_url=NATS_URL,
                nats_topic=config['nats_topic'],
                slack_token=SLACK_TOKEN,
                slack_channel=config['slack_channel'],
                limit=config['limit'],
                interval_sec=config['interval_sec'],
                dry_run=DRY_RUN)
            )

    try:
        loop = asyncio.new_event_loop()
        
        for notifier in notifiers:
            loop.run_until_complete(notifier.start(loop))
        
        loop.run_forever()
    except Exception as e:
        logging.exception(e, exc_info=True)
    finally:
        for notifier in notifiers:
            notifier.stop()
        loop.close()