import abc
from dataclasses import dataclass
import uuid
import zk_datamodel.rtmd as rtmd
import zk_datamodel.tqrpc_exch_gw as gw_rpc
import zk_datamodel.exch_gw as gw
import zk_datamodel.common as common
import typing 

@dataclass
class SimOrderRequest:
    order_id: int = None
    symbol: str = None
    side: str = None
    price: float = None
    qty: float = None

@dataclass
class SimOrder:
    order_request: gw_rpc.ExchSendOrderRequest = None
    exch_order_id: str = None # simulator should generate this
    status: gw.ExchangeOrderStatus = None
    filled_qty: float = None
    remaining_qty: float = None
    update_ts: int = None # millis

# @dataclass
# class SimOrderReport:
#     order_id: int = None
#     exch_order_id: str = None
#     status: gw.ExchangeOrderStatus = None
#     filled_qty: float = None
#     filled_price: float = None
#     filled_value: float = None
#     update_time: int = None # millis



@dataclass
class SimResult:
    delay_in_millis: int = None
    order_report: gw.OrderReport = None


@dataclass
class MatchResult:
    trigger_ts: int = None
    qty: float = None
    price: float = None
    delay: int = None
    IOC_cancel: bool = False



class MatchPolicy(abc.ABC):
    @abc.abstractmethod
    def match(self, order: SimOrder, orderbook:rtmd.TickData) -> list[MatchResult]:
        pass

    def need_tick(self) -> bool:
        return True


class ImmediateMatchPolicy(MatchPolicy):
    def match(self, order: SimOrder, orderbook: any) -> list[MatchResult]:
        res = MatchResult(trigger_ts=order.order_request.timestamp + 1,
                          qty=order.order_request.scaled_qty,
                          price=order.order_request.scaled_price,
                          delay=1)
        return [res]

    def need_tick(self) -> bool:
        return False


class FirstComeFirstServedMatchPolicy(MatchPolicy):
    def match(self, order: SimOrder, orderbook:rtmd.TickData) -> list[MatchResult]:
        if orderbook is None:
            return []
        if order.order_request.buysell_type == common.BuySellType.BS_BUY:
            return self._match_buy(order, orderbook)
        else:
            return self._match_sell(order, orderbook)

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
            if ask.price <= order.order_request.scaled_price:
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
            if bid.price >= order.order_request.scaled_price:
                match_qty = min(remaining_qty, bid.qty)
                match_res.append(MatchResult(trigger_ts=orderbook.original_timestamp,
                                             qty=match_qty, price=bid.price, delay=delay))
                remaining_qty -= match_qty
                delay += 10
        return match_res



class Simulator(typing.Protocol):
    @abc.abstractmethod
    def on_new_order(self, order: gw_rpc.ExchSendOrderRequest, ts: int) -> list[SimResult]:
        pass

    @abc.abstractmethod
    def on_tick(self, tick: rtmd.TickData) -> list[SimResult]:
        pass

    @abc.abstractmethod
    def on_cancel_order(self, cancel_req: gw_rpc.ExchCancelOrderRequest, ts: int) -> list[SimResult]:
        pass


'''
SimulatorCore is the logic/state part of the simulator. 
It should be able to handle new order, tick, and cancel order requests.
a MatchPolicy should be provided to decide how to match orders.
'''
class SimulatorCore(Simulator):

    def __init__(self, match_policy: MatchPolicy, exch_name="SIMULATOR"):
        self.match_policy = match_policy
        self.order_cache = {}
        self.orderbook_cache = {}
        self.exch_name = exch_name

    def on_new_order(self, order: gw_rpc.ExchSendOrderRequest, ts: int) -> list[SimResult]:
        # 1. validate order; reject if invalid
        if not self._validate_order(order):
            gw_order_report = gw.OrderReport()
            gw_order_report.order_id = order.correlation_id
            gw_order_report.status = gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_EXCH_REJECTED
            gw_order_report.exch_order_ref = str(uuid.uuid4())
            gw_order_report.update_timestamp = ts
            return [SimResult(order_report=gw_order_report)]

        # 2. add order to order cache
        sim_order = SimOrder(order_request=order,
                             exch_order_id=str(uuid.uuid4()),
                             status=gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_BOOKED,
                             update_ts=ts,
                             filled_qty=0,
                             remaining_qty=order.scaled_qty)
        self.order_cache[sim_order.order_request.correlation_id] = sim_order

        # 3. try match with received orderbook; generate order report (fill/partial fill/booked)
        sim_results = []
        match_results = self._try_match(sim_order)
        if match_results:
            # should be only one
            # todo: and add linkage report here
            sim_results.extend(match_results)


        # if no market data, just book the order
        if not sim_results or len(sim_results) == 0:
            res = self._update_and_gen_reports(sim_order, ts=ts, match_res=None, add_linkage_report=True)
            if res:
                sim_results.append(res)

        return sim_results

    def on_tick(self, tick: rtmd.TickData) -> list[SimResult]:
        # 1. update orderbook cache
        self.orderbook_cache[tick.instrument_code] = tick

        # 2. try match with all orders with the same symbol; generate order reports if matched (fill/partial fill)
        sim_results = []
        for sim_order in self.order_cache.values():
            sim_order: SimOrder
            if sim_order.order_request.instrument == tick.instrument_code \
                    and sim_order.status not in [
                gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_FILLED,
                gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_CANCELLED,
                gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_EXCH_REJECTED]:
                res = self._try_match(sim_order)
                if res:
                    sim_results.extend(res)

        return sim_results

    def on_cancel_order(self, cancel_req: gw_rpc.ExchCancelOrderRequest, ts: int) -> list[SimResult]:
        results = []
        # 1. find order in order cache
        order_id = cancel_req.exch_specific_params.data_map.get("order_id", None)
        if order_id is None:
            raise Exception("order_id needed for cancel order request")
        sim_order: SimOrder = self.order_cache.get(int(order_id))

        # 2. if found, remove the order, and generate order report (cancelled) with a delay
        if sim_order:
            sim_order.status = gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_CANCELLED
            sim_order.update_ts = ts+1
            update_res = self._update_and_gen_reports(sim_order, ts=ts+1, match_res=None)
            if update_res:
                results.append(update_res)

        return results

    def _try_match(self, order: SimOrder) -> list[SimResult]:
        orderbook = self.orderbook_cache.get(order.order_request.instrument, None)

        match_res: list[MatchResult] = self.match_policy.match(order, orderbook)
        sim_results = []
        for res in match_res:
            res = self._update_and_gen_reports(order, ts=None, match_res=res)
            sim_results.append(res)
        return sim_results

    def _validate_order(self, order: gw_rpc.ExchSendOrderRequest) -> bool:
        if order.scaled_qty <= 0:
            return False
        if order.scaled_price < 0:
            return False
        if order.buysell_type not in [common.BuySellType.BS_BUY, common.BuySellType.BS_SELL]:
            return False
        # todo: other validations
        return True


    def _update_and_gen_reports(self, order: SimOrder, match_res: MatchResult, ts:int=None, add_linkage_report=False) -> SimResult:
        if match_res:
            order.filled_qty += match_res.qty
            order.remaining_qty -= match_res.qty
            if order.remaining_qty == 0:
                order.status = gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_FILLED
            else:
                if match_res.IOC_cancel:
                    order.status = gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_CANCELLED
                else:
                    order.status = gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_PARTIAL_FILLED
            if match_res.trigger_ts + match_res.delay > order.update_ts:
                order.update_ts = match_res.trigger_ts + match_res.delay
            else:
                order.update_ts += 1
        gw_report = gw.OrderReport()
        gw_report.order_id = order.order_request.correlation_id
        gw_report.exch_order_ref = order.exch_order_id
        gw_report.status = order.status
        gw_report.update_timestamp = order.update_ts if ts is None else ts
        if add_linkage_report:
            report_entry = gw.OrderReportEntry()
            report_entry.report_type = gw.OrderReportType.ORDER_REP_TYPE_LINKAGE
            linkage =  gw.OrderIdLinkageReport()
            linkage.order_id = order.order_request.correlation_id
            linkage.exch_order_ref = order.exch_order_id
            report_entry.order_id_linkage_report = linkage
            gw_report.order_report_entries.append(report_entry)

        if match_res:
            gw_trade = gw.TradeReport()
            gw_trade.filled_qty = match_res.qty
            gw_trade.exch_trade_id = str(uuid.uuid4())
            gw_trade.filled_price = match_res.price

            order_info = gw.OrderInfo()
            order_info.instrument = order.order_request.instrument
            order_info.buy_sell_type = order.order_request.buysell_type

            gw_trade.order_info = order_info
            report_entry = gw.OrderReportEntry()
            report_entry.report_type = gw.OrderReportType.ORDER_REP_TYPE_TRADE
            report_entry.trade_report = gw_trade
            gw_report.order_report_entries.append(report_entry)

        gw_order_state_report = gw.OrderStateReport()
        #gw_order_state_report.order_id = order.order_request.correlation_id
        gw_order_state_report.exch_order_status = order.status
        gw_order_state_report.filled_qty = order.filled_qty
        gw_order_state_report.unfilled_qty = order.remaining_qty
        gw_order_state_report.update_time = order.update_ts

        report_entry = gw.OrderReportEntry()
        report_entry.report_type = gw.OrderReportType.ORDER_REP_TYPE_STATE
        report_entry.order_state_report = gw_order_state_report
        gw_report.order_report_entries.append(report_entry)

        return SimResult(delay_in_millis=match_res.delay if match_res else 1,
                        order_report=gw_report)

