from zk_simulator.sim_core import MatchPolicy, SimOrder, MatchResult
import zk_datamodel.common as common
import zk_datamodel.rtmd as rtmd


class DefaultMatchPolicy(MatchPolicy):

    IOC_tags = {"time_in_force", "timeinforce", "time_inforce", "tif"}

    def match(self, order: SimOrder, orderbook:rtmd.TickData) -> list[MatchResult]:
        if orderbook is None:
            return []
        res = []
        if order.order_request.buysell_type == common.BuySellType.BS_BUY:
            res = self._match_buy(order, orderbook)
        else:
            res = self._match_sell(order, orderbook)

        if res and len(res) != 0:
            return res
        else:
            if self.is_ioc_order(order):
                return [MatchResult(trigger_ts=orderbook.original_timestamp,
                                    qty=0, price=0, delay=1, IOC_cancel=True)]
            else:
                return []

    def is_ioc_order(self, order: SimOrder) -> bool:
        if order.order_request.exch_specific_params and order.order_request.exch_specific_params.data_map:
            for key, val in order.order_request.exch_specific_params.data_map.items():
                if key.lower() in self.IOC_tags and val.lower() == "ioc":
                    return True
        is_ioc = order.order_request.time_inforce_type == common.TimeInForceType.TIMEINFORCE_IOC
        return is_ioc

    def _match_buy(self, order: SimOrder, orderbook: rtmd.TickData) -> list[MatchResult]:
        match_res = []
        remaining_qty = order.remaining_qty
        delay = 10
        # assuming sell price levels are sorted from low to high

        for ask in orderbook.sell_price_levels:
            if abs(remaining_qty) < 0.000001:
                break
            if ask.price is None or ask.qty is None:
                continue
            if (order.order_request.order_type == common.BasicOrderType.ORDERTYPE_MARKET or
                    ask.price <= order.order_request.scaled_price):
                match_qty = min(remaining_qty, ask.qty)
                match_res.append(MatchResult(trigger_ts=orderbook.original_timestamp,
                                             qty=match_qty, price=ask.price, delay=delay))
                remaining_qty -= match_qty
                delay += 10
        return match_res

    def _match_sell(self, order: SimOrder, orderbook:rtmd.TickData) -> list[MatchResult]:
        match_res = []
        remaining_qty = order.remaining_qty
        delay = 10
        # assuming buy price levels are sorted from high to low
        for bid in orderbook.buy_price_levels:
            if abs(remaining_qty) < 0.000001:
                break
            if bid.price is None or bid.qty is None:
                continue
            if (order.order_request.order_type == common.BasicOrderType.ORDERTYPE_MARKET or
                    bid.price >= order.order_request.scaled_price):
                match_qty = min(remaining_qty, bid.qty)
                match_res.append(MatchResult(trigger_ts=orderbook.original_timestamp,
                                             qty=match_qty, price=bid.price, delay=delay))
                remaining_qty -= match_qty
                delay += 10
        return match_res
