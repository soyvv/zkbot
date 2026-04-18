import asyncio
from dataclasses import dataclass

import nats
from redis.asyncio import Redis

from zk_proto_betterproto import rtmd
from loguru import logger

from tqrpc_utils.config_utils import try_load_config

@dataclass
class AppConfig:
    nats_url: str = None
    rtmd_subjects: list[str] = None
    redis_host: str = None
    redis_port: str = None
    redis_password: str = None
    redis_data_prefix: str = None
    redis_registry_prefix: str = None
    registry_expire_seconds: int = 30



class RtmdManager:

    def __init__(self, config: AppConfig):
        self.config = config

    async def start(self):
        logger.info("Starting rtmd manager")

        logger.info(f"Connecting to NATS at {self.config.nats_url}")
        self.nc = await nats.connect(self.config.nats_url)

        logger.info(f"Connecting to Redis at {self.config.redis_host}:{self.config.redis_port}")
        self.redis = Redis(host=self.config.redis_host,
                           port=int(self.config.redis_port),
                           password=self.config.redis_password)

        self.queue = asyncio.Queue(1000)

        # (exchange, symbol) -> (msg_data, subject, msg_timestamp)
        self.tick_cache: dict[tuple[str, str], tuple[bytes, str, int]] = {}

        async def on_data(msg):
            try:
                self.queue.put_nowait(msg)
            except asyncio.QueueFull:
                logger.warning("Queue is full, dropping message")

        for subject in self.config.rtmd_subjects:
            logger.info(f"Subscribing to {subject}")
            await self.nc.subscribe(subject, cb=on_data)


    async def process_tick_data(self):
        while True:
            first_msg = await self.queue.get()
            try:
                first_event = rtmd.TickData().parse(first_msg.data)
            except Exception as e:
                logger.error(f"Error parsing tick data: {e}")
                self.queue.task_done()
                continue
            symbol, exchange = first_event.instrument_code, first_event.exchange
            self.tick_cache[(symbol, exchange)] = (first_msg.data, first_msg.subject, first_event.original_timestamp)

            events = {(first_event.exchange, first_event.instrument_code)}
            extra_events_num = self.queue.qsize()
            tick_skipped = 0
            if extra_events_num > 0:
                # last_tick_idx_dict: dict[tuple[str, str], int] = {}  # (symbol, exchange) -> idx
                # tick_skipped = 0
                for _ in range(extra_events_num):
                    try:
                        next_msg = self.queue.get_nowait()
                        next_event = rtmd.TickData().parse(next_msg.data)
                    except asyncio.QueueEmpty:
                        logger.warning("queue empty while processing extra events")
                        break
                    except Exception as e:
                        logger.error(f"Error parsing tick data: {e}")
                        continue
                    symbol, exchange = next_event.instrument_code, next_event.exchange
                    self.tick_cache[(symbol, exchange)] = (next_msg.data, next_msg.subject, next_event.original_timestamp)

                    events.add((next_event.exchange, next_event.instrument_code))

            logger.info(f"processing {len(events)} events")
            asyncio.create_task(self._on_update(events))
            self.queue.task_done()


    async def _on_update(self, updates: set[tuple[str, str]]):
        for exchange, symbol in updates:
            msg_data, subject, msg_timestamp = self.tick_cache[(symbol, exchange)]
            instrument_exch = symbol

            data_key = f"{self.config.redis_data_prefix}:{exchange}:{instrument_exch}"
            await self.redis.set(data_key, msg_data)
            await self.redis.expire(data_key, time=self.config.registry_expire_seconds)

            registry_key = f"{self.config.redis_registry_prefix}:{exchange}:{instrument_exch}"
            await self.redis.set(registry_key, subject.encode('utf-8'))
            await self.redis.expire(registry_key, time=self.config.registry_expire_seconds)


def main():


    app_config: AppConfig = try_load_config(AppConfig)

    logger.info(f"Starting rtmd manager with config: {app_config}")

    rtmd_mgr = RtmdManager(app_config)
    loop = asyncio.get_event_loop()
    loop.run_until_complete(rtmd_mgr.start())
    loop.run_until_complete(rtmd_mgr.process_tick_data())


if __name__ == "__main__":
    main()