import math
from typing import List, Union, Optional

import betterproto

from .models import *
from .utils import *
import zk_proto_betterproto.oms as oms
import zk_proto_betterproto.tqrpc_exch_gw as gw_rpc
import zk_proto_betterproto.exch_gw as gw

from collections import deque
from loguru import logger

import snowflake

class OrderManager:
    def __init__(self, instrument_refdata_lookuptable: dict[str, dict[str, InstrumentRefdata]],
                 use_time_emulation=False, max_cached_orders=1000):
        self.use_time_emulation = use_time_emulation
        self.order_dict: dict[int, OMSOrder] = {} # order_id -> OMSOrder
        self.open_order_ids = set()
        self.max_cached_orders = max_cached_orders
        self.order_id_queue = deque()
        self.context_cache: dict[int, OrderContext] = {}  # order_id -> context
        self.exch_ref_to_order_id: dict[str, dict[str, int]] = {} # gw_key -> {exch_order_ref -> order_id}

        self.nontq_order_dict: dict[str, dict[str, int]] = {} # gw_key -> {exch_order_ref -> dummy_order_id}

        # a temporary solution to generate dummy order id
        instance_id = int(time.time() * 1000_000) % snowflake.snowflake.MAX_INSTANCE
        self.order_gen = snowflake .SnowflakeGenerator(instance=instance_id)
        self.reload_instrument_lookuptable(instrument_refdata_lookuptable)


    def reload_instrument_lookuptable(self, instrument_refdata_lookup_table: dict[str, dict[str, InstrumentRefdata]]):
        self.refdata_lookup_table: dict[str, dict[str, InstrumentRefdata]] = instrument_refdata_lookup_table

    def init_with_orders(self, curr_orders: list[OMSOrder]):
        for order in curr_orders:
            err = self._validate_order(order)
            if err:
                logger.error(f"invalid order to initialize: {order.order_id}, err: {err}")
                continue

            oid = order.order_id
            self.order_dict[oid] = order
            if not self.is_in_terminal_state(order):
                if order.oms_order_state.filled_qty != order.oms_order_state.qty:
                    self.open_order_ids.add(oid)
            if order.oms_order_state.exch_order_ref:
                exch_order_ref = order.oms_order_state.exch_order_ref
                gw_key = order.oms_order_state.gw_key
                self.exch_ref_to_order_id.setdefault(
                    gw_key, {})[exch_order_ref] = oid


    def _validate_order(self, order: OMSOrder) -> str:
        if not order .oms_order_state:
            return "missing order_state"
        return None

    def remove_open_order(self, order_id: int):
        if order_id in self.open_order_ids:
            self.open_order_ids.remove(order_id)
        self.order_id_queue.append(order_id)
        if len(self.order_id_queue) > self.max_cached_orders:
            oldest_order_id = self.order_id_queue.popleft()
            self.cleanup_order(oldest_order_id)
    def cleanup_order(self, order_id: int) -> bool:
        if order_id not in self.order_dict:
            # ignore if order is not found
            # return true to indicate that order is cleaned up
            return True
        order = self.order_dict[order_id]
        gw_key = order.oms_order_state.gw_key
        exch_order_ref = order.oms_order_state.exch_order_ref
        if order_id in self.order_dict:
            del self.order_dict[order_id]

        if gw_key in self.exch_ref_to_order_id:
            if exch_order_ref in self.exch_ref_to_order_id[gw_key]:
                del self.exch_ref_to_order_id[gw_key][exch_order_ref]

        if order_id in self.context_cache:
            del self.context_cache[order_id]

        return True

    def create_order(self, order_req:oms.OrderRequest, ctx: OrderContext) -> OMSOrder:
        gw_req = self._create_gw_order_req(order_req, ctx) if ctx else None

        order_state = oms.Order()
        order_state.snapshot_version = 1
        order_state.gw_key = ctx.route.gw_key if ctx else None
        order_state.order_id = order_req.order_id
        order_state.exch_order_ref = None
        order_state.account_id = order_req.account_id
        order_state.order_status = oms.OrderStatus.ORDER_STATUS_PENDING # initial state
        order_state.instrument = order_req.instrument_code
        order_state.instrument_exch = ctx.symbol_ref.instrument_id_exchange if ctx else None
        order_state.price = gw_req.scaled_price if gw_req else order_req.price
        order_state.qty = gw_req.scaled_qty if gw_req else order_req.qty
        order_state.filled_qty = .0
        order_state.source_id = order_req.source_id
        order_state.buy_sell_type = order_req.buy_sell_type
        order_state.open_close_type = order_req.open_close_type
        order_state.created_at = order_req.timestamp if self.use_time_emulation else gen_timestamp()
        order_state.updated_at = order_state.created_at

        oms_order = OMSOrder(
            is_from_external=False,
            order_id=order_req.order_id,
            account_id=order_req.account_id,
            exch_order_ref=None,
            oms_req=order_req,
            gw_req=gw_req,
            oms_order_state=order_state
        )
        # oms_order.order_id = order_req.order_id
        # oms_order.account_id = order_req.account_id
        # oms_order.gw_req = self._create_gw_order_req(order_req, ctx)
        # oms_order.oms_req = order_req

        self.order_dict[oms_order.order_id] = oms_order
        self.open_order_ids.add(oms_order.order_id)

        return oms_order

    def handle_external_order_report(self, order_report: gw.OrderReport) -> oms.OrderUpdateEvent:
        gw_key = order_report.exchange
        exch_order_ref = order_report .exch_order_ref
        if not self.is_marked_as_external(exch_order_ref, gw_key):
            # create the order and mark this order as external
            oms_order: OMSOrder = self._create_external_order(gw_key=gw_key, exch_order_ref=exch_order_ref, init_report=order_report)

            if gw_key not in self.nontq_order_dict:
                self.nontq_order_dict[gw_key] = {}
            self.nontq_order_dict[gw_key][exch_order_ref] = oms_order.order_id
            self.order_dict[oms_order.order_id] = oms_order
            self.exch_ref_to_order_id.setdefault(gw_key, {})[exch_order_ref] = oms_order.order_id
        else:
            oms_order = self.get_external_order(gw_key, exch_order_ref)

        # override the order_id to oms generated order_id;
        order_report.order_id = oms_order.order_id

        # enrich the order if more info is provided in the report
        if betterproto.serialized_on_wire(order_report .order_details):
            order_details = order_report.order_details
            if order_details.instrument and not oms_order.oms_order_state.instrument:
                instrument_id = self._get_instrument_id(gw_key, order_details.instrument)
                oms_order.oms_order_state.instrument = instrument_id
                oms_order.oms_order_state.instrument_exch = order_details.instrument
            if order_details.buy_sell_type and not oms_order.oms_order_state.buy_sell_type:
                oms_order.oms_order_state.buy_sell_type = order_details.buy_sell_type
            if order_details .place_qty and not oms_order.oms_order_state.qty:
                oms_order.oms_order_state.qty = order_details.place_qty
            if order_details.place_price and not oms_order.oms_order_state.price:
                oms_order.oms_order_state.price = order_details.place_price

        oue = self._update_external_order(order=oms_order, report=order_report)
        return oue

    def _get_instrument_id(self, gw_key: str, exch_symbol: str) -> str:
        if gw_key in self.refdata_lookup_table:
            refdata = self.refdata_lookup_table[gw_key].get(exch_symbol, None)
            if refdata:
                return refdata.instrument_id
        return None


    def is_marked_as_external(self, exch_order_ref: str, gw_key: str) -> bool:
        return gw_key in self.nontq_order_dict and exch_order_ref in self.nontq_order_dict[gw_key]


    def get_external_order(self, gw_key: str, exch_order_ref: str) -> OMSOrder:
        if gw_key in self.nontq_order_dict:
            order_id = self.nontq_order_dict[gw_key].get(exch_order_ref, None)
            if order_id:
                return self.order_dict.get(order_id, None)
        return None


    def _create_external_order(self, gw_key: str, exch_order_ref: str, init_report: gw.OrderReport) -> OMSOrder:

        order = OMSOrder()
        order_id = self._gen_dummy_order_id()

        order.is_from_external = True
        order.order_id = order_id
        order.account_id = init_report.account_id
        order.exch_order_ref = exch_order_ref
        order.gw_req = None
        order.oms_req = None
        order.trades = []
        order.exec_msgs = []
        order.fees = []
        order.cancel_req = None
        order.cancel_attempts = 0

        oms_order_state = oms.Order()
        oms_order_state.order_id = order_id
        oms_order_state.exch_order_ref = exch_order_ref
        oms_order_state.account_id = init_report.account_id
        oms_order_state .instrument = None # not known yet
        oms_order_state .order_status = oms.OrderStatus.ORDER_STATUS_PENDING
        oms_order_state.gw_key = gw_key
        oms_order_state.created_at = init_report.update_timestamp
        oms_order_state.updated_at = init_report.update_timestamp
        oms_order_state.source_id = f"External@{gw_key}"
        order.oms_order_state = oms_order_state

        self.open_order_ids.add(order_id)

        return order

    def _gen_dummy_order_id(self) -> int:
        return next(self.order_gen)


    def _update_external_order(self, order: OMSOrder, report: gw.OrderReport) -> oms.OrderUpdateEvent:
        oues = self.update_with_report(order_report=report)
        if len(oues) > 0:
            return oues[0] # should be only one at most
        return None


    def _update_order_on_error(self, order: OMSOrder,
                               order_id: int,
                               error_msg:str, ts: int,
                               exec_type: oms.ExecType,
                               rejection: common.Rejection) -> OMSOrder:
        if order is None and order_id is None:
            raise ValueError("order and order_id cannot be both None")
        if order is None:
            order = self.order_dict.get(order_id, None)
        if order is None:
            raise ValueError("order not found for order_id: %s" % order_id)

        exec_msg = oms.ExecMessage()
        exec_msg.order_id = order.order_id
        exec_msg.exec_type = exec_type
        exec_msg.exec_success = False
        exec_msg.timestamp = ts
        exec_msg.error_msg = error_msg
        exec_msg.rejection_info = rejection

        order.exec_msgs.append(exec_msg)

        if exec_type == oms.ExecType.EXEC_TYPE_PLACING_ORDER:
            order.oms_order_state.snapshot_version += 1
            order.oms_order_state.order_status = oms.OrderStatus.ORDER_STATUS_REJECTED
            order.oms_order_state.updated_at = ts
            order.oms_order_state.error_msg = error_msg

            if order_id in self.open_order_ids:
                self.remove_open_order(order.oms_order_state.order_id)

        return order


    def _generate_event_on_error(self, order: OMSOrder, ts:int=None) -> oms.OrderUpdateEvent:
        update_event = oms.OrderUpdateEvent()
        update_event.order_id = order.order_id
        update_event.account_id = order.account_id
        update_event.timestamp = order.oms_order_state.updated_at if ts is None else ts

        exec_msg = order.exec_msgs[-1]

        update_event.order_snapshot = order.oms_order_state
        update_event.exec_message = exec_msg

        update_event.order_source_id = order.oms_order_state.source_id

        return update_event

    def update_with_oms_error(self, oms_order: OMSOrder, order_req: oms.OrderRequest, error_msg: str, rejection: common.Rejection):
        ts_millis = order_req.timestamp if self.use_time_emulation else gen_timestamp()
        if oms_order is None:
            order = self.create_order(order_req=order_req, ctx=None)
        else:
            order = oms_order

        self._update_order_on_error(order=order,
                                    order_id=order_req.order_id,
                                    error_msg=error_msg,
                                    ts=ts_millis,
                                    exec_type=oms.ExecType.EXEC_TYPE_PLACING_ORDER,
                                    rejection=rejection)

        return self._generate_event_on_error(order)

    def update_with_gw_error(self, order: OMSOrder, ts: int,  error_msg: str,
                            exec_type: oms.ExecType, rejection: common.Rejection):
        self._update_order_on_error(order=order,
                                    order_id=order.order_id,
                                    error_msg=error_msg,
                                    ts=ts,
                                    exec_type=exec_type,
                                    rejection=rejection)

        return self._generate_event_on_error(order, ts=ts)


    def get_order_by_id(self, order_id) -> OMSOrder:
        return self.order_dict.get(order_id, None)

    def get_order(self, order_id: int, exchange_id: str, exch_order_ref: str) -> OMSOrder:
        ##    raise Exception("cannot find order with id: %s" % order_id)
        #    Exception: cannot find order with id: 0
        ## TODO: check this exception
        if order_id is not None and order_id != 0:
            return self.order_dict.get(order_id, None)
        else:
            found_order_id = self.exch_ref_to_order_id.get(exchange_id, {}).get(exch_order_ref, None)
            if found_order_id:
                return self.order_dict.get(found_order_id, None)
            else:
                return None

    def get_open_orders(self, account_id:int) -> List[OMSOrder]:
        open_orders = []
        for id in self.open_order_ids:
            order: OMSOrder = self.order_dict.get(id, None)
            if order is None:
                raise Exception("illegal state: cannot find order with id: %s" % id)
            if order.account_id == account_id:
                open_orders.append(order)
        return open_orders


    def get_order_by_exch_ref(self, gw_key: str, exch_order_ref: str) -> OMSOrder:
        if gw_key in self.exch_ref_to_order_id:
            order_id = self.exch_ref_to_order_id[gw_key].get(exch_order_ref, None)
            if order_id:
                return self.order_dict.get(order_id, None)
        return None

    def update_with_report(self, order_report: gw.OrderReport) -> list[oms.OrderUpdateEvent]:
        update_events = []
        original_order_updated = False
        new_trade = False
        new_oms_trade = False
        new_fee = False
        new_exec_msg = False
        order_id = order_report.order_id if order_report.order_id and order_report.order_id != 0 else None
        exch_order_id = order_report.exch_order_ref
        gw_key = order_report.exchange
        ts = order_report.update_timestamp
        order = self.get_order(order_id=order_id, exchange_id=gw_key, exch_order_ref=exch_order_id)

        if order is None:
            raise Exception("cannot find order with id: %s" % order_id)

        order_id = order.order_id
        order_state = order.oms_order_state
        order_state.updated_at = ts
        if order_state.exch_order_ref is None:
            order_state.exch_order_ref = exch_order_id
        report_entries = order_report.order_report_entries
        exec_reports_from_oms = None

        self._infer_state_report(report_entries, order)

        for report_entry in report_entries:
            if report_entry.report_type == gw.OrderReportType.ORDER_REP_TYPE_LINKAGE:
                order.exch_order_ref = report_entry.order_id_linkage_report.exch_order_ref
                #order.order_id = report_entry.order_id_linkage_report.order_id
                order.oms_order_state.exch_order_ref = exch_order_id
                if gw_key is None:
                    raise Exception("gw_key is None in linkage report")
                if gw_key not in self.exch_ref_to_order_id:
                    self.exch_ref_to_order_id[gw_key] = {}
                self.exch_ref_to_order_id[gw_key][exch_order_id] = order_id

                if len(report_entries) == 1:
                    # if linkage is the only report, then we need to update the order status too
                    if order.oms_order_state.order_status in {
                        oms.OrderStatus.ORDER_STATUS_PENDING, oms.OrderStatus.ORDER_STATUS_REJECTED
                    }:
                        order.oms_order_state.order_status = oms.OrderStatus.ORDER_STATUS_BOOKED
                        original_order_updated = True
                        # TODO: put back to open orders
            elif report_entry.report_type == gw.OrderReportType.ORDER_REP_TYPE_STATE:
                if order_report.order_source_type == gw.OrderSourceType.ORDER_SOURCE_NON_TQ:
                    # in case of non-tq order report, we need to enrich the order state when possible
                    self._try_enrich_order_state(order_state, report_entry.order_state_report)
                original_filled_qty = order_state.filled_qty if order_state.filled_qty is not None else .0
                original_avg_price = order_state.filled_avg_price
                filled_qty = None
                if report_entry.order_state_report.filled_qty !=.0:
                    filled_qty = report_entry.order_state_report.filled_qty
                elif report_entry.order_state_report.unfilled_qty != .0:
                    filled_qty = order_state.qty - report_entry.order_state_report.unfilled_qty

                if filled_qty is not None and order_state.filled_qty is not None:
                    order_state.filled_qty = max(filled_qty, order_state.filled_qty)

                if report_entry.order_state_report.avg_price is not None and \
                        report_entry.order_state_report.avg_price != .0:
                    order_state.filled_avg_price = report_entry.order_state_report.avg_price

                if order_state.filled_qty is not None and order_state.filled_qty - original_filled_qty > 1e-8:
                    fill_increment = order_state.filled_qty - original_filled_qty
                    pseudo_trade_id = f"{order_id}_{len(order.order_inferred_trades)}"
                    inferred_filled_price = self._infer_oms_trade_price(
                        original_filled_qty=original_filled_qty,
                        original_avg_price=original_avg_price,
                        new_filled_qty=order_state.filled_qty,
                        new_avg_price=order_state.filled_avg_price
                    )
                    trade_increment = oms.Trade(
                        order_id=order_state.order_id,
                        ext_order_ref=order_state.exch_order_ref,
                        ext_trade_id=pseudo_trade_id,
                        filled_ts=order_report.update_timestamp,
                        filled_qty=fill_increment,
                        filled_price=inferred_filled_price
                    )
                    order.order_inferred_trades.append(trade_increment)
                    new_oms_trade = True


                gw_order_status = report_entry.order_state_report.exch_order_status
                # to handle out-of-order status update, need to do some checking
                _gw_inferred_status = self._map_order_status(gw_order_status, order_state.order_status)
                _can_update_status: bool = self._check_can_update_status(
                    old_status=order_state.order_status,
                    new_status=_gw_inferred_status)
                if _can_update_status:
                    order_state.order_status = _gw_inferred_status
                else:
                    pass # TODO: log warning
                original_order_updated = True
            elif report_entry.report_type == gw.OrderReportType.ORDER_REP_TYPE_TRADE:
                if order_report.order_source_type == gw.OrderSourceType.ORDER_SOURCE_NON_TQ:
                    # in case of non-tq order report, we need to enrich the order state when possible
                    self._try_enrich_order_state(order_state, report_entry.trade_report)
                trade = oms.Trade()
                trade.order_id = order_state.order_id
                trade.ext_order_ref = order_state.exch_order_ref
                trade.filled_qty = report_entry.trade_report.filled_qty
                trade.filled_price = report_entry.trade_report.filled_price
                trade.filled_ts = ts
                trade.account_id = order_state.account_id
                trade.buy_sell_type = order_state.buy_sell_type
                trade.instrument = order_state.instrument
                trade.fill_type = report_entry.trade_report.fill_type
                trade.ext_trade_id = report_entry.trade_report.exch_trade_id
                trade.exch_pnl = report_entry.trade_report.exch_pnl
                order.trades.append(trade)
                order.acc_trades_filled_qty += trade.filled_qty
                order.acc_trades_value += trade.filled_qty * trade.filled_price
                original_order_updated = True
                new_trade = True
            elif report_entry.report_type == gw.OrderReportType.ORDER_REP_TYPE_EXEC:
                exec_report = report_entry.exec_report
                exec_type = exec_report.exec_type
                if exec_type == gw.ExchExecType.EXCH_EXEC_TYPE_REJECTED:
                    if exec_report.error_info:
                        error_msg = f"err from gw: code={exec_report.error_info.error_code}, " \
                                    f" msg={exec_report.error_info.error_message}"
                        self._update_order_on_error(
                            order=order, order_id=None, ts=ts, error_msg=error_msg,
                            exec_type=oms.ExecType.EXEC_TYPE_PLACING_ORDER,
                            rejection=exec_report.rejection_info)
                        original_order_updated = True
                        new_exec_msg = True
                elif exec_type == gw.ExchExecType.EXCH_EXEC_TYPE_CANCEL_REJECT:
                    if exec_report.error_info:
                        error_msg = f"err from gw: code={exec_report.error_info.error_code}, " \
                                    f" msg={exec_report.error_info.error_message}"
                        self._update_order_on_error(
                            order=order, order_id=None, ts=ts, error_msg=error_msg,
                            exec_type=oms.ExecType.EXEC_TYPE_CANCEL,
                            rejection=exec_report.rejection_info)
                        original_order_updated = True
                        new_exec_msg = True
                # todo: handle other exec types
            elif report_entry.report_type == gw.OrderReportType.ORDER_REP_TYPE_FEE:
                gw_fee_msg: gw.FeeReport = report_entry.fee_report
                fee_msg: oms.Fee = oms.Fee()
                fee_msg.fee_amount = gw_fee_msg.fee_qty
                fee_msg.fee_symbol = gw_fee_msg.fee_symbol
                fee_msg.order_id = order_state.order_id
                fee_msg.fee_ts = ts
                if hasattr(gw_fee_msg, "fee_type"):
                    fee_msg.fee_type = gw_fee_msg.fee_type
                order.fees.append(fee_msg)
                new_fee = True
                original_order_updated = True # force update
            else:
                raise Exception("Unknown order report type")
            pass
        if original_order_updated:
            self._check_and_finalize_avg_price(order)
            order_state.snapshot_version += 1
            event1 = oms.OrderUpdateEvent()
            event1.order_id = order_id
            event1.account_id = order_state.account_id
            event1.timestamp = ts if self.use_time_emulation else gen_timestamp()
            event1.order_snapshot = order_state
            event1.order_source_id = order_state.source_id
            if new_oms_trade:
                event1.order_inferred_trade = order.order_inferred_trades[-1]
            if new_trade:
                event1.last_trade = order.trades[-1]
            if new_exec_msg:
                event1.exec_message = order.exec_msgs[-1]
            if new_fee:
                event1.last_fee = order.fees[-1]
            update_events.append(event1)

            if order_id in self.open_order_ids and self.is_in_terminal_state(order=order):
                self.remove_open_order(order_id)

        return update_events

    def remove_order(self, order_id):
        pass


    def _try_enrich_order_state(self, order_state: oms.Order,
                                report_entry: Union[gw.OrderStateReport, gw.TradeReport]):
        pass


    def _infer_state_report(self, report_entries: List[gw.OrderReportEntry], order: OMSOrder):
        # if there is trade but not orderstate in report_entries
        # then infer the state from the trade report
        # should only update when the inferred state is terminal?

        if self.is_in_terminal_state(order):
            return
        trades = [r.trade_report for r in report_entries if r.report_type == gw.OrderReportType.ORDER_REP_TYPE_TRADE]
        statereports = [r.order_state_report for r in report_entries if r.report_type == gw.OrderReportType.ORDER_REP_TYPE_STATE]
        if len(trades) == 0 or len(statereports) != 0:
            return

        new_trades_qty = sum([t.filled_qty for t in trades])
        new_trades_value = sum([t.filled_qty * t.filled_price for t in trades])
        current_trades_qty = order.acc_trades_filled_qty
        total_trades_qty = new_trades_qty + current_trades_qty

        if total_trades_qty > order.oms_order_state.qty:
            logger.warning(f"total trades qty {total_trades_qty} > order qty {order.oms_order_state.qty}")
            return

        avg_price = (order.acc_trades_value + new_trades_value)/total_trades_qty if total_trades_qty != 0 else .0

        status = gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_FILLED if (
                abs(total_trades_qty - order.oms_order_state.qty) < 1e-8) \
            else gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_PARTIAL_FILLED

        inferred_state_report = gw.OrderReportEntry(
            report_type=gw.OrderReportType.ORDER_REP_TYPE_STATE,
            order_state_report=gw.OrderStateReport(
                exch_order_status=status,
                filled_qty=total_trades_qty,
                unfilled_qty=order.oms_order_state.qty - total_trades_qty,
                avg_price=avg_price
            )
        )

        report_entries.append(inferred_state_report)


    def _check_and_finalize_avg_price(self, order: OMSOrder):
        oms_state: oms.Order = order.oms_order_state

        if len(order.trades) == 0:
            return

        is_in_terminal_state = self.is_in_terminal_state(order)
        if is_in_terminal_state and \
            (oms_state.filled_avg_price is None or oms_state.filled_avg_price == .0):
            # calculate avg price if needed
            total_trades_qty = order.acc_trades_filled_qty
            total_trades_value =  order.acc_trades_value
            avg_price = total_trades_value / total_trades_qty if total_trades_qty != .0 else .0
            oms_state.filled_avg_price = avg_price




    def generate_order_snapshot(self, order: OMSOrder, order_id: int, ts: int) -> oms.OrderUpdateEvent:
        o = self.get_order_by_id(order_id) if order is None else order
        if not o:
            raise Exception("cannot find order with id: %s" % order_id)
        event1 = oms.OrderUpdateEvent()
        event1.order_id = order_id
        event1.account_id = o.account_id
        event1.timestamp = ts
        event1.order_snapshot = o.oms_order_state
        event1.order_source_id = o.oms_order_state.source_id

        return event1

    def _create_gw_order_req(self, order_req: oms.OrderRequest,
                             ctx: OrderContext):
        gw_req = gw_rpc.ExchSendOrderRequest()
        gw_req.exch_account_id = ctx.route.exch_account_id
        gw_req.order_type = order_req.order_type
        gw_req.buysell_type = order_req.buy_sell_type
        gw_req.openclose_type = order_req.open_close_type
        gw_req.correlation_id = order_req.order_id
        gw_req.instrument = ctx.symbol_ref.instrument_id_exchange
        gw_req.scaled_qty = self._round(order_req.qty, ctx.symbol_ref.qty_precision) \
            if ctx.symbol_ref.qty_precision is not None else order_req.qty

        if gw_req.scaled_qty == .0:
            ctx.errors.append("order qty rounded to 0; please check the qty")

        gw_req.scaled_price = self._round(order_req.price, ctx.symbol_ref.price_precision) \
            if ctx.symbol_ref.price_precision is not None else order_req.price
        gw_req.timestamp = order_req.timestamp if self.use_time_emulation else gen_timestamp()
        gw_req.time_inforce_type = order_req.time_inforce_type
        gw_req.exch_specific_params = order_req.extra_properties

        return gw_req

    def _round(self, val: float, precision: int) -> float:
        # if precision >= 0:
        #     return round(val, precision)
        # else:
        #     div = 10 ** abs(precision)
        #     return math.floor(val / div) * div
        return int(val * (10 ** precision)) / (10 ** precision)

    def _map_order_status(self, gw_order_status: gw.ExchangeOrderStatus, cur_order_status: oms.OrderStatus) \
            -> oms.OrderStatus:
        if gw_order_status == gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_BOOKED:
            return oms.OrderStatus.ORDER_STATUS_BOOKED
        elif gw_order_status == gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_PARTIAL_FILLED:
            return oms.OrderStatus.ORDER_STATUS_PARTIALLY_FILLED
        elif gw_order_status == gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_FILLED:
            return oms.OrderStatus.ORDER_STATUS_FILLED
        elif gw_order_status == gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_CANCELLED:
            return oms.OrderStatus.ORDER_STATUS_CANCELLED
        elif gw_order_status == gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_EXCH_REJECTED:
            return oms.OrderStatus.ORDER_STATUS_REJECTED
        elif gw_order_status == gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_EXPIRED:
            return oms.OrderStatus.ORDER_STATUS_CANCELLED # fixme
        else:
            raise ValueError("Unknown order status: " + str(gw_order_status))


    def is_in_terminal_state(self, order: OMSOrder):
        return order.oms_order_state.order_status in \
            [oms.OrderStatus.ORDER_STATUS_FILLED,
             oms.OrderStatus.ORDER_STATUS_CANCELLED,
             oms.OrderStatus.ORDER_STATUS_REJECTED]
        # todo: and expired

    def _check_can_update_status(self, old_status: oms.OrderStatus, new_status: oms.OrderStatus):
        # will not update on cancelled or filled order; but might update on rejected order
        if old_status == oms.OrderStatus.ORDER_STATUS_CANCELLED or old_status == oms.OrderStatus.ORDER_STATUS_FILLED:
            return False

        return True

    def _infer_oms_trade_price(self, original_avg_price:float, new_avg_price:float,
                           original_filled_qty:float, new_filled_qty:float) \
            -> Optional[float]:
        '''
        infer the trade qty and trade price from the avg price and filled qty
        return None or [fill_qty_increment, inferred_trade_price]
        '''
        original_filled_qty = .0 if original_filled_qty is None else original_filled_qty
        # validate the input
        if new_filled_qty is None or new_filled_qty == 0:
            return None
        if original_avg_price is None or original_avg_price == 0:
            return new_avg_price
        if new_avg_price is None or new_avg_price == 0:
            return None

        # infer the increment avg price
        new_qty = new_filled_qty - original_filled_qty
        if new_qty == 0:
            return None
        original_value = original_avg_price * original_filled_qty
        new_value = new_avg_price * new_filled_qty
        increment_value = new_value - original_value
        increment_avg_price = increment_value / new_qty
        return increment_avg_price




