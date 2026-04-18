#import redis
import redis.asyncio as aioredis
from betterproto import Casing

import zk_oms.core.models as models

from dataclasses import asdict
import orjson as json
import sys

from zk_proto_betterproto import oms
from zk_proto_betterproto import tqrpc_exch_gw as gw_rpc

from loguru import logger

class RedisHandler:
    def __init__(self, redis_client: aioredis.Redis, account_ids:list[int], name_space:str = 'tq'):
        self.redis_conn = redis_client
        self.namespace = name_space
        self._order_prefix = f'{name_space}:order'
        self._open_order_prefix = f'{name_space}:open_order'
        self._pos_symbol_set = f'{name_space}:pos_symbol_set'
        self._position_prefix = f'{name_space}:pos'
        self._balance_report_prefix = f'{name_space}:balance_report'
        self._position_report_prefix = f'{name_space}:position_report'
        self.account_ids = account_ids


    def reload_config(self, account_ids: list[int]):
        self.account_ids = account_ids

    def convert_order_to_dict(self, order: models.OMSOrder):
        _data = {'order_state': json.dumps(order.oms_order_state.to_dict(casing=Casing.SNAKE)),
                 'trades': json.dumps([t.to_dict(casing=Casing.SNAKE) for t in order.trades]),
                 'oms_req': json.dumps(order.oms_req.to_dict(casing=Casing.SNAKE)) if order.oms_req else "{}",
                 'gw_req': json.dumps(order.gw_req.to_dict(casing=Casing.SNAKE)) if order.gw_req else "{}",
                 "cancels": order.cancel_attempts}
        return _data

    async def store_order(self, order_id:int, account_id:int, order_data: dict, set_expire: bool = False, set_open=False, set_closed=False):
        key = f"{self._order_prefix}:{order_id}"
        _data = order_data
        await self.redis_conn.hset(name=key, mapping=_data)

        if set_open:
            await self.redis_conn.sadd(f"{self._open_order_prefix}:{account_id}", order_id)
        elif set_closed:
            await self.redis_conn.srem(f"{self._open_order_prefix}:{account_id}", order_id)

        if set_expire:
            await self.redis_conn.expire(name=key, time=60*60*24) # expire after one day

    # def update_orderstate(self, orderstate: oms.Order):
    #     key = f"{self._order_prefix}:{orderstate.order_id}"
    #     _data = {'order_state': orderstate.to_json()}
    #     self.redis_conn.hset(name=key, mapping=_data)
    #
    # def udpate_trades(self, trades: list[oms.Trade]):
    #     order_id = trades[0].order_id
    #     key = f"{self._order_prefix}:{order_id}"
    #     _data = {'trades': json.dumps([t.to_json() for t in trades])}
    #     self.redis_conn.hset(name=key, mapping=_data)

    async def load_all_orders(self) -> list[models.OMSOrder]:
        orders = []
        problematic_order_keys = []
        for account_id in self.account_ids:
            open_order_key = f"{self._open_order_prefix}:{account_id}"
            open_order_ids = await self.redis_conn.smembers(open_order_key)
            for order_id in open_order_ids:
                order_id_str = order_id.decode("utf-8")
                try:
                    order = await self._load_order(f"{self._order_prefix}:{order_id_str}")
                except Exception as ex:
                    logger.error(f"error loading for order with key={order_id_str}", exc_info=True)
                    problematic_order_keys.append(order_id)
                    continue
                orders.append(order)


        logger.info(f"loaded {len(orders)} orders from redis")
        if len(problematic_order_keys) > 0:
            logger.error(f"problematic order keys during loading: {problematic_order_keys}")

        return orders

    async def _load_order(self, k: str):
        _data = await self.redis_conn.hgetall(name=k)
        oms_order_state_json: bytes = _data.get(b"order_state", None)
        trades_json = _data.get(b"trades", None)
        oms_req_json = _data.get(b"oms_req", None)
        gw_req_json = _data.get(b"gw_req", None)
        cancels = _data.get(b"cancels", 0)

        order = models.OMSOrder()

        order.cancel_attempts = int(cancels)

        if oms_order_state_json:
            oms_order_state: oms.Order = oms.Order().from_json(oms_order_state_json.decode("utf-8"))
            order.oms_order_state = oms_order_state
            order.order_id = oms_order_state.order_id
            order.exch_order_ref = oms_order_state.exch_order_ref
        else:
            logger.error(f"missing order state for order {k}")

        if trades_json:
            trades = [oms.Trade().from_dict(t) for t in json.loads(trades_json.decode("utf-8"))]
            order.trades = trades
        else:
            logger.warning(f"missing trades_json for order {k}")

        if oms_req_json:
            oms_req: oms.OrderRequest = oms.OrderRequest().from_json(oms_req_json.decode("utf-8"))
            order.oms_req = oms_req
            order.account_id = oms_req.account_id
        else:
            logger.warning(f"missing oms_req_json for order {k}")

        if gw_req_json:
            gw_req = gw_rpc.ExchSendOrderRequest().from_json(gw_req_json.decode("utf-8"))
            order.gw_req = gw_req
        else:
            logger.warning(f"missing gw_req_json for order {k}")

        return order




    def remove_order(self, order_id: int):
        pass


    async def store_position(self, position: oms.Position):
        pos_key = f"{self._position_prefix}:{position.account_id}:{position.instrument_code}"
        pos_data = position.to_json()

        all_pos_id_key = f"{self._pos_symbol_set}:{position.account_id}"

        await self.redis_conn.set(name=pos_key, value=pos_data)
        await self.redis_conn.sadd(all_pos_id_key, position.instrument_code)

    async def store_balance_report(self, balance_report: models.BalanceSnapshot):
        key = f"{self._balance_report_prefix}:{balance_report.account_id}:{balance_report.instrument_tq}"
        data = balance_report.__dict__
        await self.redis_conn.set(name=key, value=json.dumps(data))
        await self.redis_conn.expire(name=key, time=60*10)

    async def store_position_report(self, balance_report: models.BalanceSnapshot, position_extra_data: models.PositionExtraData):
        key = f"{self._position_report_prefix}:{balance_report.account_id}:{balance_report.instrument_tq}"
        data = {}
        data.update(balance_report.__dict__)
        data.update(position_extra_data.__dict__)
        await self.redis_conn.set(name=key, value=json.dumps(data))
        await self.redis_conn.expire(name=key, time=60*10)

    async def _load_position(self, k: str) -> models.OMSPosition:
        data = await self.redis_conn.get(k)
        pos = oms.Position().from_json(data)

        position = models.OMSPosition(
            account_id=pos.account_id,
            symbol=pos.instrument_code
        )
        position.position_state = pos
        position.is_short = False # TODO
        return position


    async def load_all_positions(self) -> list[models.OMSPosition]:
        positions = []
        problematic_pos_keys = []

        for account_id in self.account_ids:
            all_pos_id_key = f"{self._pos_symbol_set}:{account_id}"
            all_pos_ids = await self.redis_conn.smembers(all_pos_id_key)
            if not all_pos_ids:
                continue
            for pos_id in all_pos_ids:
                pos_id_str = pos_id.decode("utf-8")
                try:
                    pos = await self._load_position(f"{self._position_prefix}:{account_id}:{pos_id_str}")
                    positions.append(pos)
                except Exception as ex:
                    logger.error(f"error loading for position with key={pos_id_str}", exc_info=True)
                    problematic_pos_keys.append(pos_id_str)
        if len(problematic_pos_keys) > 0:
            logger.error(f"problematic position keys during loading: {problematic_pos_keys}")

        return positions