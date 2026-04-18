import asyncio
import datetime
import traceback

import nats

from tqrpc_utils.simple_rpc_client import rpc

from zk_oms.core.models import GwConfigEntry, ExchOrderRef, OMSAction, OMSActionType, OrderRecheckRequest, \
    CancelRecheckRequest
from zk_proto_betterproto import exch_gw as gw, common
from zk_proto_betterproto import tqrpc_exch_gw as gw_rpc
from tqrpc_utils import rpc_utils

from loguru import logger


async def _put_back_to_inbound_queue(msg:any, queue:asyncio.Queue):
    wait_time = 0.1
    retries = 0
    while True:
        try:
            queue.put_nowait(msg)
            break # break the loop if put is successful
        except asyncio.QueueFull:
            logger.warning(f"queue full, retrying later")
            retries += 1
            if retries >= 10:
                logger.error(f"queue full, giving up; dropping msg: {msg}")
                break
            wait_time = min(wait_time*2, 5)
            await asyncio.sleep(wait_time)


class GWSender:
    def __init__(self,
                 nats_client: nats.NATS,
                 gw_config_table: list[GwConfigEntry],
                 report_queue: asyncio.Queue=None):
        self.nc = nats_client
        self.queue = report_queue
        self.gw_route_table: dict[str, GwConfigEntry] = {}
        for gw_entry in gw_config_table:
            self.gw_route_table[gw_entry.gw_key] = gw_entry

    def reload_config(self, gw_config_table: list[GwConfigEntry]):
        self.gw_route_table = {}
        for gw_entry in gw_config_table:
            self.gw_route_table[gw_entry.gw_key] = gw_entry

    async def query_orders(self, gw_key: str, order_queries: list[ExchOrderRef], timeout:int=2) -> list[gw.OrderReport]:
        gw_config = self.gw_route_table[gw_key]
        if not gw_config.support_order_query:
            logger.error(f"gw {gw_key} does not support order query")
            return []
        query_channel = gw_config.rpc_endpoint
        headers = {"rpc_method": "QueryOrderDetails"}

        reports = []
        for o_query in order_queries:
            query_request = gw_rpc.ExchQueryOrderDetailRequest()
            query_request.order_queries = [
                gw_rpc.ExchSingleOrderQuery(
                    exch_order_ref=o_query.exch_order_id,
                    order_id=o_query.order_id,
                    symbol=o_query.symbol_exch,
                    exch_account=None # TODO
                )
            ]
            try:
                reply_msg = await self.nc.request(subject=query_channel, headers=headers, payload=bytes(query_request),
                                                  timeout=timeout)
            except:
                err_msg = traceback.format_exc()
                logger.error(f"error when trying to query order for {gw_key}: {err_msg}")
                continue

            resp = gw_rpc.ExchOrderDetailResponse().parse(reply_msg.data)
            if resp.orders:
                reports.append(resp.orders[0].order_report)

        return reports


    async def query_balances(self, gw_key: str, explicit_symbols:list[str]=None,
                             timeout=5, retry_on_timeout=False) -> gw_rpc.ExchAccountResponse:
        gw_config = self.gw_route_table[gw_key]
        if not gw_config.support_position_query:
            logger.error(f"gw {gw_key} does not support balance query")
            return gw_rpc.ExchAccountResponse()
        query_channel = gw_config.rpc_endpoint
        headers = {"rpc_method": "QueryAccountBalance"}
        query_request = gw_rpc.ExchQueryAccountRequest()

        if explicit_symbols:
            query_request.explicit_symbols = explicit_symbols

        try:

            reply_msg, is_error = await rpc(
                nc=self.nc,
                method="QueryAccountBalance",
                request_payload=query_request,
                response_type=gw_rpc.ExchAccountResponse,
                subject=query_channel,
                timeout_in_secs=timeout,
                retry_on_timeout=retry_on_timeout
            )

        except:
            err_msg = traceback.format_exc()
            logger.error(f"error when trying to query balances for {gw_key}: {err_msg}")
            return gw_rpc.ExchAccountResponse() # return empty response

        if not is_error:
            return reply_msg
        else:
            logger.error(f"error when trying to query balances for {gw_key}: {reply_msg}")
            return gw_rpc.ExchAccountResponse() # return empty response



    async def send_to_gw(self, action: OMSAction):
        gw_key = action.action_meta.get("exch_name", None)
        gw_config = self.gw_route_table[gw_key]

        if action.action_type == OMSActionType.BATCH_SEND_ORDER_TO_GW:
            order_channel = gw_config.rpc_endpoint
            headers = {"rpc_method": "BatchPlaceOrders"}
            batch_order: gw_rpc.ExchBatchSendOrdersRequest = action.action_data
            err_msg = None
            reply_msg = None
            try:
                reply_msg = await self.nc.request(
                    subject=order_channel,
                    headers=headers,
                    payload=bytes(batch_order),
                    timeout=2)
                if reply_msg.headers is not None and 'error' in reply_msg.headers:
                    rpc_err: rpc_utils.RPCCallError = rpc_utils.RPCCallError.from_json(str(reply_msg.data))
                    err_msg = rpc_err.message
            except:
                # should be mostly transport related errors
                err_msg = traceback.format_exc()

            if err_msg:
                # could be timeout or could be gw internal errors
                # in both cases, will need to wait for a while before determining the actual order state
                logger.error(f"error when trying to place batch orders to {gw_key}: {err_msg}")
                for order in batch_order.order_requests:
                    order_recheck = OrderRecheckRequest()
                    order_recheck.order_id = int(order.correlation_id)
                    order_recheck.timestamp = order.timestamp
                    order_recheck.check_delay_in_sec = 15
                    async def recheck():
                        await asyncio.sleep(order_recheck.check_delay_in_sec)
                        if self.queue:
                            #await self.queue.put(order_recheck)
                            asyncio.create_task(_put_back_to_inbound_queue(order_recheck, self.queue))

                    asyncio.create_task(recheck())
            elif reply_msg:
                resp = gw_rpc.GatewayResponse().parse(reply_msg.data)
                if resp.status == gw_rpc.GatewayResponseStatus.GW_RESP_STATUS_SUCCESS:
                    logger.debug("send batch placeorder to gw succeeded")
                else:
                    err_msg = f"error when placing batch order to gw: {resp.to_json()}"
                    logger.error(err_msg)
                    # TODO: handle batch error?
        elif action.action_type == OMSActionType.BATCH_SEND_CANCEL_TO_GW:
            cancel_channel = gw_config.rpc_endpoint
            headers = {"rpc_method": "BatchCancelOrders"}
            batch_cancel: gw_rpc.ExchBatchCancelOrdersRequest = action.action_data
            err_msg = None
            reply_msg = None
            try:
                reply_msg = await self.nc.request(
                    subject=cancel_channel,
                    headers=headers,
                    payload=bytes(batch_cancel),
                    timeout=2)
                if reply_msg.headers is not None and 'error' in reply_msg.headers:
                    rpc_err: rpc_utils.RPCCallError = rpc_utils.RPCCallError.from_json(str(reply_msg.data))
                    err_msg = rpc_err.message
            except:
                # should be mostly transport related errors
                err_msg = traceback.format_exc()

            if err_msg:
                logger.error(logger.error(f"error when trying to batchcancel order for {gw_key}: {err_msg}"))
                for cancel in batch_cancel.cancel_requests:
                    cancel_recheck = CancelRecheckRequest()
                    cancel_recheck.orig_request = cancel
                    cancel_recheck.check_delay_in_sec = 15
                    cancel_recheck.retry = False  # todo: should use config value

                    async def recheck():
                        await asyncio.sleep(cancel_recheck.check_delay_in_sec)
                        if self.queue:
                            #await self.queue.put(cancel_recheck)
                            asyncio.create_task(_put_back_to_inbound_queue(cancel_recheck, self.queue))
                    asyncio.create_task(recheck())

            elif reply_msg:
                resp = gw_rpc.GatewayResponse().parse(reply_msg.data)
                if resp.status == gw_rpc.GatewayResponseStatus.GW_RESP_STATUS_SUCCESS:
                    logger.debug("send batch cancelorder to gw succeeded")
                else:
                    err_msg = f"rejected when batch cancelling order to gw: {resp.to_json()}"
                    logger.error(err_msg)
                    # TODO: handle batch error?
        elif action.action_type == OMSActionType.SEND_ORDER_TO_GW:
            order_channel = gw_config.rpc_endpoint
            headers = {"rpc_method": "PlaceOrder"}
            order: gw_rpc.ExchSendOrderRequest = action.action_data
            err_msg = None
            reply_msg = None
            try:
                reply_msg = await self.nc.request(subject=order_channel, headers=headers, payload=bytes(order),
                                                  timeout=2)
                if reply_msg.headers is not None and 'error' in reply_msg.headers:
                    rpc_err: rpc_utils.RPCCallError = rpc_utils.RPCCallError.from_json(str(reply_msg.data))
                    err_msg = rpc_err.message
            except:
                # should be mostly transport related errors
                err_msg = traceback.format_exc()

            if err_msg:
                # could be timeout or could be gw internal errors
                # in both cases, will need to wait for a while before determining the actual order state
                logger.error(f"error when trying to place order for {gw_key}: {err_msg}")

                order_recheck = OrderRecheckRequest()
                order_recheck.order_id = int(order.correlation_id)
                order_recheck.timestamp = order.timestamp
                order_recheck.check_delay_in_sec = 5
                async def recheck():
                    await asyncio.sleep(order_recheck.check_delay_in_sec)
                    if self.queue:
                        asyncio.create_task(_put_back_to_inbound_queue(order_recheck, self.queue))
                asyncio.create_task(recheck())
            elif reply_msg:
                resp = gw_rpc.GatewayResponse().parse(reply_msg.data)
                if resp.status == gw_rpc.GatewayResponseStatus.GW_RESP_STATUS_SUCCESS:
                    logger.debug("send placeorder to gw succeeded")
                else:
                    err_msg = f"error when placing order to gw: {resp.to_json()}"
                    rejection = resp.rejection_info
                    gw_report = self._mock_gw_reject_report(order, err_msg, rejection=rejection)
                    gw_report.exchange = gw_key
                    if self.queue:
                        asyncio.create_task(_put_back_to_inbound_queue(gw_report, self.queue))

        elif action.action_type == OMSActionType.SEND_CANCEL_TO_GW:
            cancel_channel = gw_config.rpc_endpoint
            cancel: gw_rpc.ExchCancelOrderRequest = action.action_data
            headers = {"rpc_method": "CancelOrder"}
            err_msg = None
            reply_msg = None
            try:
                reply_msg = await self.nc.request(subject=cancel_channel, headers=headers, payload=bytes(cancel))
                if reply_msg.headers is not None and 'error' in reply_msg.headers:
                    rpc_err: rpc_utils.RPCCallError = rpc_utils.RPCCallError.from_json(str(reply_msg.data))
                    err_msg = rpc_err.message
            except:
                err_msg = traceback.format_exc()

            if err_msg:
                logger.error(logger.error(f"error when trying to cancel order for {gw_key}: {err_msg}"))
                cancel_recheck = CancelRecheckRequest()
                cancel_recheck.orig_request = cancel
                cancel_recheck.check_delay_in_sec = 5
                cancel_recheck.retry = False  # todo: should use config value

                async def recheck():
                    await asyncio.sleep(cancel_recheck.check_delay_in_sec)
                    if self.queue:
                        #await self.queue.put(cancel_recheck)
                        asyncio.create_task(_put_back_to_inbound_queue(cancel_recheck, self.queue))
                asyncio.create_task(recheck())

            elif reply_msg:
                resp = gw_rpc.GatewayResponse().parse(reply_msg.data)
                if resp.status == gw_rpc.GatewayResponseStatus.GW_RESP_STATUS_SUCCESS:
                    logger.debug("send cancelorder to gw succeeded")
                else:
                    err_msg = f"rejected when cancelling order to gw: {resp.to_json()}"
                    logger.error(err_msg)
                    gw_report = self._mock_cancel_reject_report(cancel, resp.rejection_info)
                    gw_report.exchange = gw_key
                    if self.queue:
                        #await self.queue.put(gw_report)
                        asyncio.create_task(_put_back_to_inbound_queue(gw_report, self.queue))

    def _mock_gw_reject_report(self, order_req: gw_rpc.ExchSendOrderRequest, err_msg: str, rejection: common.Rejection):
        gw_report = gw.OrderReport()
        gw_report.order_id = int(order_req.correlation_id)
        # wrap state report
        gw_order_state_report = gw.OrderStateReport()
        gw_order_state_report.exch_order_status = gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_EXCH_REJECTED
        gw_order_state_report.unfilled_qty = order_req.scaled_qty
        gw_order_state_report.filled_qty = 0
        gw_report.order_report_entries = []
        gw_report.order_report_entries.append(
            gw.OrderReportEntry(report_type=gw.OrderReportType.ORDER_REP_TYPE_STATE,
                                order_state_report=gw_order_state_report))

        # exec report with error info
        gw_exec_report = gw.ExecReport()
        gw_exec_report.exec_type = gw.ExchExecType.EXCH_EXEC_TYPE_REJECTED
        gw_exec_report.exec_message = err_msg
        gw_exec_report.error_info = gw.ExchErrorInfo()
        gw_exec_report.error_info.error_message = err_msg
        gw_exec_report.error_info.error_code = -1 # todo: error code
        gw_exec_report.rejection_info = rejection
        gw_report.order_report_entries.append(
            gw.OrderReportEntry(report_type=gw.OrderReportType.ORDER_REP_TYPE_EXEC,
                                exec_report=gw_exec_report))

        gw_report.update_timestamp = order_req.timestamp
        return gw_report

    def _mock_cancel_reject_report(self, cancel_req: gw_rpc.ExchCancelOrderRequest, rejection: common.Rejection):
        gw_report = gw.OrderReport()
        gw_report.order_id = cancel_req.order_id
        gw_report.exch_order_ref = cancel_req.exch_order_ref

        # exec report with error info
        gw_exec_report = gw.ExecReport()
        gw_exec_report.exec_type = gw.ExchExecType.EXCH_EXEC_TYPE_CANCEL_REJECT
        gw_exec_report.rejection_info = rejection
        gw_exec_report.exec_message = rejection.error_message
        gw_report.order_report_entries.append(
            gw.OrderReportEntry(report_type=gw.OrderReportType.ORDER_REP_TYPE_EXEC,
                                exec_report=gw_exec_report))

        gw_report.update_timestamp = int(datetime.datetime.now().timestamp()*1000)
        return gw_report