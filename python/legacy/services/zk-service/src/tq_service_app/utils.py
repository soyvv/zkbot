from typing import List, Union
import functools
from nats.aio.client import Client as NatsClient

import zk_proto_betterproto.tqrpc_oms as oms_rpc
import zk_proto_betterproto.oms as oms
import zk_proto_betterproto.common as common


def get_followed_orders_key(prefix: str, oms_id: str, account_id: Union[str, int], order_id: Union[str, int]):
    return f'{prefix}:followed_orders:{oms_id}:{account_id}:{order_id}'

async def fetch_balance(nc: NatsClient, oms_id: str, account_id: int):
    subject = f"tq.oms.service.{oms_id}.rpc"
    headers = {"rpc_method": "QueryAccountBalance"}
    query_request = oms_rpc.OmsQueryAccountRequest()
    query_request.account_id = account_id
    resp = await nc.request(subject, bytes(query_request), headers=headers, timeout=3)
    balances = oms_rpc.OmsAccountResponse().parse(resp.data)
    return balances

def calc_open_order_stats(orders: list[oms.Order]):

    def add_to_dict(orders_dict: dict[str, list[oms.Order]], order: oms.Order):
        instrument = order.instrument
        if instrument in orders_dict:
            orders_dict[instrument].append(order)
        else:
            orders_dict[instrument] = [order]
        return orders_dict

    def aggregate_orders(instrument: str, orders: list):
        buy_orders = [order for order in orders if order.buy_sell_type == common.BuySellType.BS_BUY]
        sell_orders = [order for order in orders if order.buy_sell_type == common.BuySellType.BS_SELL]

        count = len(orders)
        count_buy = len(buy_orders)
        count_sell = len(sell_orders)
        qty_buy = sum([float(order.qty) for order in buy_orders])
        qty_sell = sum([float(order.qty) for order in sell_orders])
        ts_max = max([int(order.created_at) for order in orders])

        return {
            "instrument": instrument,
            "count": count,
            "count_buy": count_buy,
            "count_sell": count_sell,
            "qty_buy": qty_buy,
            "qty_sell": qty_sell,
            "ts_max": ts_max
        }

    orders_by_instrument = functools.reduce(add_to_dict, orders, {})
    order_stats = [aggregate_orders(instrument, orders) for instrument, orders in orders_by_instrument.items()]

    return order_stats

query_accounts_pipeline = [
    {
        "$lookup": {
            "from": "refdata_gw_config", 
            "localField": "gw_key", 
            "foreignField": "gw_key", 
            "as": "config"
        }
    },
    {"$unwind": "$config"},
    {"$project": {"_id": 0, "accound_id": 1, "gw_key": 1, "exch_name": "$config.exch_name"}},
    {
        "$lookup": {
            "from": "refdata_instrument_basic",
            "localField": "exch_name",
            "foreignField": "exch_name",
            "as": "instruments"
        }
    },
    {
        "$project": {
            "account_id": "$accound_id",
            "gw_key": 1,
            "exch_name": 1,
            "instrument_codes": {"$map": {"input": "$instruments", "as": "instruments", "in": "$$instruments.instrument_code"}}
        }
    }
]

def sanitiseJsonString(string: str):
    return string.replace("\'", "\"").replace('None', 'null').replace('False', 'false').replace('True', 'true')
