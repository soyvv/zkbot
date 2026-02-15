import datetime

import zk_datamodel.exch_gw as gw
import zk_datamodel.tqrpc_exch_gw as gw_rpc
import zk_datamodel.common as common

import math

class GwMessageHelper:

    def __init__(self, gw_key: str, exch_account_id: str):
        self.gw_key = gw_key
        self.exch_account_id = exch_account_id

    def generate_linkage_report(self, exch_order_ref: str,
                                client_order_id: int,
                                ts: int=None):
        if ts is None:
            ts = self._gen_timestamp()

        order_report = gw.OrderReport()
        order_report.exch_order_ref = exch_order_ref
        order_report.order_id = client_order_id
        order_report.exchange = self.gw_key
        order_report.update_timestamp = ts
        report_entries = []
        report = gw.OrderReportEntry()
        report.report_type = gw.OrderReportType.ORDER_REP_TYPE_LINKAGE
        link_rep = gw.OrderIdLinkageReport()
        link_rep.exch_order_ref = exch_order_ref
        link_rep.order_id = client_order_id
        report.order_id_linkage_report = link_rep

        report_entries.append(report)
        order_report.order_report_entries = report_entries

        return order_report

    def generate_trade_report(self, filled_price: float, filled_qty: float, exch_order_ref,
                              ts: int=None,
                              order_id: int = None,
                              gw_order_report:gw.OrderReport=None):
        if gw_order_report is None:
            gw_order_report = self._gen_gw_order_report(exch_order_ref=exch_order_ref,
                                                        order_id=order_id,
                                                        ts=ts)
        trade_report_entry = gw.OrderReportEntry()
        trade_report_entry.report_type = gw.OrderReportType.ORDER_REP_TYPE_TRADE
        trade_rep = gw.TradeReport()
        # trade_rep.exch_trade_id = str(execution_report['tradeId'])
        trade_rep.filled_price = filled_price
        trade_rep.filled_qty = filled_qty
        trade_report_entry.trade_report = trade_rep

        gw_order_report.order_report_entries.append(trade_report_entry)

        return gw_order_report



    def generate_orderstate_report(self, exch_order_ref:str,
                                   state: gw.ExchangeOrderStatus,
                                   qty: float, filled_qty: float, unfilled_qty: float,
                                   ts:int=None,
                                   order_id: int=None,
                                   gw_order_report:gw.OrderReport=None):
        if gw_order_report is None:
            gw_order_report = self._gen_gw_order_report(exch_order_ref=exch_order_ref,
                                                        order_id=order_id,
                                                        ts=ts)

        state_report_entry = gw.OrderReportEntry()
        state_report_entry.report_type = gw.OrderReportType.ORDER_REP_TYPE_STATE
        state_rep = gw.OrderStateReport()

        orig_qty, filled_qty, remaining_qty = self._infer_order_qty(
            qty=qty, filled_qty=filled_qty, unfilled_qty=unfilled_qty)

        state_rep.filled_qty = filled_qty
        state_rep.unfilled_qty = remaining_qty

        state_rep.exch_order_status = state
        state_report_entry.order_state_report = state_rep
        gw_order_report.order_report_entries.append(state_report_entry)

        return gw_order_report


    def generate_position_report(self, exch_symbol: str,
                                 qty: float,
                                 msg_raw: str,
                                 is_perp: bool,
                                 ts:int=None):
        pu = gw.PositionReport()

        pu.instrument_type = common.InstrumentType.INST_TYPE_PERP \
            if is_perp else common.InstrumentType.INST_TYPE_SPOT

        pu.instrument_code = exch_symbol
        pu.exch_account_code = self.exch_account_id

        pu.qty = math.fabs(qty)

        pu.long_short_type = common.LongShortType.LS_SHORT if qty < 0 else common.LongShortType.LS_LONG
        pu.avail_qty = pu.qty

        pu.update_timestamp = self._gen_timestamp() if ts is None else ts
        pu.message_raw = msg_raw

        return pu

    def generate_order_error_report(self, order_req: gw_rpc.ExchSendOrderRequest,
                                    exch_order_ref: str,
                                    rej: common.Rejection,
                                    gw_order_report: gw.OrderReport=None,
                                    include_order_state: bool=True) -> gw.OrderReport:

        if gw_order_report is None:
            gw_order_report = self._gen_gw_order_report(exch_order_ref=exch_order_ref,
                                                        order_id=order_req.correlation_id,
                                                        ts=order_req.timestamp)

        if include_order_state:
            gw_order_state_report = gw.OrderStateReport()
            gw_order_state_report.exch_order_status = gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_EXCH_REJECTED
            gw_order_state_report.unfilled_qty = order_req.scaled_qty
            gw_order_state_report.filled_qty = 0
            gw_order_report.order_report_entries.append(
                gw.OrderReportEntry(report_type=gw.OrderReportType.ORDER_REP_TYPE_STATE,
                                    order_state_report=gw_order_state_report))

        # exec report with error info
        gw_exec_report = gw.ExecReport()
        gw_exec_report.exec_type = gw.ExchExecType.EXCH_EXEC_TYPE_REJECTED
        gw_exec_report.rejection_info = rej
        gw_order_report.order_report_entries.append(
            gw.OrderReportEntry(report_type=gw.OrderReportType.ORDER_REP_TYPE_EXEC,
                                exec_report=gw_exec_report))

        return gw_order_report


    def generate_cancel_error_report(self, exch_order_ref: str,
                                     rej: common.Rejection,
                                     order_id: int = None,
                                     gw_order_report: gw.OrderReport = None):

        if gw_order_report is None:
            ts = self._gen_timestamp()
            gw_order_report = self._gen_gw_order_report(exch_order_ref=exch_order_ref,
                                                        order_id=None,
                                                        ts=ts)

        # exec report with error info
        gw_exec_report = gw.ExecReport()
        gw_exec_report.exec_type = gw.ExchExecType.EXCH_EXEC_TYPE_CANCEL_REJECT
        gw_exec_report.rejection_info = rej
        gw_order_report.order_report_entries.append(
            gw.OrderReportEntry(report_type=gw.OrderReportType.ORDER_REP_TYPE_EXEC,
                                exec_report=gw_exec_report))

        return gw_order_report

    def generate_order_error_report_with_details(
            self, order_req: gw_rpc.ExchSendOrderRequest,
            exch_order_ref: str,
            error_code: int, error_msg: str, error_recoverable: bool,
            gw_order_report: gw.OrderReport = None,
            include_order_state: bool = True) -> gw.OrderReport:
        rej = common.Rejection(
            error_code=error_code,
            error_message=error_msg,
            recoverable=error_recoverable,
            reason=common.RejectionReason.REJ_REASON_EXCH_REJECT,
            source=common.RejectionSource.EXCH_REJECT
        )
        return self.generate_order_error_report(order_req=order_req,
                                                exch_order_ref=exch_order_ref,
                                                rej=rej,
                                                gw_order_report=gw_order_report,
                                                include_order_state=include_order_state)


    def generate_cancel_error_report_with_details(
            self, exch_order_ref: str,
            error_code: int, error_msg: str, error_recoverable: bool,
            order_id:int=None,
            gw_order_report: gw.OrderReport = None) -> gw.OrderReport:
        rej = common.Rejection(
            error_code=error_code,
            error_message=error_msg,
            recoverable=error_recoverable,
            reason=common.RejectionReason.REJ_REASON_EXCH_REJECT,
            source=common.RejectionSource.EXCH_REJECT
        )
        return self.generate_cancel_error_report(exch_order_ref=exch_order_ref,
                                                 rej=rej,
                                                 order_id=order_id,
                                                 gw_order_report=gw_order_report)


    def _gen_gw_order_report(self, exch_order_ref, order_id:int = None, ts: int=None):
        gw_order_report = gw.OrderReport()
        gw_order_report.exchange = self.gw_key
        gw_order_report.exch_order_ref = str(exch_order_ref)
        if order_id is not None:
            gw_order_report.order_id = order_id
        gw_order_report.update_timestamp = ts if ts else self._gen_timestamp()
        gw_order_report.order_report_entries = []
        return gw_order_report

    def _infer_order_qty(self, qty: float, filled_qty: float, unfilled_qty: float):
        # assert at least two of the three are not None
        assert (qty is not None and filled_qty is not None) or \
               (qty is not None and unfilled_qty is not None) or \
               (filled_qty is not None and unfilled_qty is not None)

        if qty is not None and filled_qty is not None:
            return (qty, filled_qty, qty - filled_qty)
        elif qty is not None and unfilled_qty is not None:
            return (qty, qty - unfilled_qty, unfilled_qty)
        elif filled_qty is not None and unfilled_qty is not None:
            return (filled_qty + unfilled_qty, filled_qty, unfilled_qty)


    def _gen_timestamp(self) -> int:
        return int(datetime.datetime.now().timestamp() * 1000)