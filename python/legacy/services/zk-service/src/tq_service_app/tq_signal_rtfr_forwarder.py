import asyncio
import json
from dataclasses import dataclass

import nats

from zk_proto_betterproto import rtmd

from loguru import logger

from tqrpc_utils.config_utils import try_load_config
from zk_oms.core.utils import pb_to_json


@dataclass
class AppConfig:
    nats_url: str = None
    rtfr_channels: list[str] = None



class RealtimeFundingRateSignalPublisher:

    def __init__(self, app_config: AppConfig):
        self.nc = None
        self.nats_url = app_config.nats_url
        self.rtfr_channels = app_config.rtfr_channels

    async def start(self):
        logger.info(f"connecting to nats: {self.nats_url}")
        self.nc = await nats.connect(self.nats_url)

        async def cb(msg):
            data = msg.data
            fr = rtmd.FundingRate().parse(data)
            await self.forward_as_signal(fr)


        for channel in self.rtfr_channels:
            logger.info(f"Subscribing to channel: {channel}")
            await self.nc.subscribe(channel, cb=cb)

    async def forward_as_signal(self, msg: rtmd.FundingRate):
        logger.info(f"receiving funding rate msg: {msg}")
        exchange = msg.exchange.lower()
        symbol = msg.instrument_code
        signal_channel = f"tq.signal.rtfr.{exchange}.{symbol}"
        data = rtmd.RealtimeSignal(
            namespace=f"rt_funding_rate_{exchange}",
            instrument=symbol,
            timestamp=msg.observe_timestamp,
            data={
                "funding_rate": msg.funding_rate,
                "mark_price": msg.mark_price,
                "index_price": msg.index_price,
                "next_funding_rate": msg.next_funding_rate,
            },
            data_int={
                "curr_fr_timestamp": msg.curr_funding_rate_timestamp,
                "next_fr_timestamp": msg.next_funding_rate_timestamp,
                "next_payment_timestamp": msg.next_payment_timestamp
            },
            message=msg.original_message
        )
        logger.info(f"Publishing signal: {pb_to_json(data)}")
        logger.info(f"to channel: {signal_channel}")
        await self.nc.publish(signal_channel, data.__bytes__())

    async def stop(self):
        await self.nc.close()


if __name__ == "__main__":
    app_config: AppConfig = try_load_config(AppConfig)
    pub = RealtimeFundingRateSignalPublisher(app_config)
    loop = asyncio.get_event_loop()
    loop.run_until_complete(pub.start())
    loop.run_forever()


