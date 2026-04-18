import datetime
import logging
import traceback

import zk_proto_betterproto.oms as oms_proto
import zk_proto_betterproto.exch_gw as gw
import zk_proto_betterproto.common as proto_common
from .confdata_mgr import ConfdataManager

from .utils import pb_to_json
from .models import *
from zk_utils.instrument_utils import SPOT_LIKE_TYPES
from .order_mgr import OrderManager
from .balance_mgr import BalanceManager

from loguru import logger

class OMSCore:

    def __init__(self, confdata_mgr: ConfdataManager,
                 use_time_emulation: bool = False,
                 logger_enabled=True,
                 risk_check_enabled=False,
                 handle_non_tq_order_reports=False,
                 order_cache_size=1000):
        self.logger_enabled = logger_enabled
        self.risk_check_enabled = risk_check_enabled
        self.use_time_emulation = use_time_emulation
        self.handle_non_tq_order_reports = handle_non_tq_order_reports

        # init refdata manager
        self.confdata_mgr: ConfdataManager = confdata_mgr
        # self.confdata_mgr.validate_and_reload_account_config(account_routes, gw_configs)
        # self.confdata_mgr.validate_and_reload_instrument_refdata(refdata)

        self._setup_internal_state(self.confdata_mgr)

        self._SUCCESS = ValidateResult(True, None)

        # oms core states
        self._total_retries_orders = 0
        self._total_retries_cancels = 0
        self._panic_accounts = set()
        # gw_key -> exch_order_id -> list[OrderReport]
        self._pending_order_reports: dict[str, dict[str, list[gw.OrderReport]]] = {}

        self.order_mgr = OrderManager(
            instrument_refdata_lookuptable=self.refdata_lookup,
            use_time_emulation=use_time_emulation,
            max_cached_orders=order_cache_size)
        self.balance_mgr = BalanceManager(
            instrument_refdata_lookuptable=self.confdata_mgr.get_instrument_lookuptable_account_id(),
            account_mapping=self.confdata_mgr.get_account_routes(),
            gw_configs=self.confdata_mgr.get_gw_configs())  # todo: config




    def _setup_internal_state(self, refdata_mgr: ConfdataManager):
        account_routes_list = refdata_mgr.get_account_routes()
        gw_config_list = refdata_mgr.get_gw_configs()
        refdata_list = refdata_mgr.get_instrument_refdata()
        trading_config_list = refdata_mgr.get_instrument_trading_configs()
        self._account_ids = [r.accound_id for r in account_routes_list]

        self.routes = {}
        for route in account_routes_list:
            self.routes[route.accound_id] = route

        self.gw_configs = {}
        self.exch_name_to_gw_keys = {} # exch_name -> set[gw_key]
        for gw_config in gw_config_list:
            self.gw_configs[gw_config.gw_key] = gw_config
            exch_name = gw_config .exch_name
            if exch_name not in self.exch_name_to_gw_keys:
                self.exch_name_to_gw_keys[exch_name] = set()
            self.exch_name_to_gw_keys[exch_name].add(gw_config.gw_key)

        self.accountid_to_gwconfig: dict[int, GwConfigEntry] = {}
        for route in account_routes_list:
            gw_conf = self.gw_configs.get(route.gw_key)
            if gw_conf is None:
                raise Exception(f"gw config not found for route {route}")
            self.accountid_to_gwconfig[route.accound_id] = gw_conf

        self.gw_key_to_accountids: dict[str, set[int]] = {}
        for route in account_routes_list:
            if route.gw_key not in self.gw_key_to_accountids:
                self.gw_key_to_accountids[route.gw_key] = set()
            self.gw_key_to_accountids[route.gw_key].add(route.accound_id)

        self.refdata = {}
        self.refdata_lookup: dict[str, dict[str, common.InstrumentRefData]] = {} # gw_key -> instrument_code -> refdata
        for ref in refdata_list:
            if not ref.disabled:
                self.refdata[ref.instrument_id] = ref
            exch_name = ref.exchange_name
            gw_keys_for_exch = self.exch_name_to_gw_keys.get(exch_name, set())
            for gw_key in gw_keys_for_exch:
                if gw_key not in self.refdata_lookup:
                    self.refdata_lookup[gw_key] = {}
                self.refdata_lookup[gw_key][ref.instrument_id_exchange] = ref


        self.trading_configs = {}
        for tc in trading_config_list:
            self.trading_configs[tc.instrument_code] = tc

        for ref in refdata_list:
            if ref.instrument_id not in self.trading_configs:
                # add default config
                #logger.info("adding default trading config for " + ref.instrument_code)
                trading_config = InstrumentTradingConfig(instrument_code=ref.instrument_id)
                self.trading_configs[ref.instrument_id] = trading_config


    def reload_config(self):

        self._setup_internal_state(self.confdata_mgr)
        self.order_mgr.reload_instrument_lookuptable(
            self.confdata_mgr.get_instrument_lookuptable_gw_key())
        self.balance_mgr.reload_instrument_lookuptable(
            self.confdata_mgr.get_instrument_lookuptable_account_id())

        self.balance_mgr.reload_account_config(
            account_routes=self.confdata_mgr.get_account_routes(),
            gw_configs=self.confdata_mgr.get_gw_configs()
        )

        return []

    # def reload_instrument_refdata(self, refdata_reload_request: OMSRefdataReloadRequest):
    #     data_changed: bool = self.refdata_mgr.validate_and_reload_instrument_refdata(
    #         refdata_reload_request.refdata)
    #     if data_changed:
    #         self._setup_internal_state(self.refdata_mgr)
    #         self.order_mgr.reload_instrument_lookuptable(
    #             self.refdata_mgr.get_instrument_lookuptable_gw_key())
    #         self.balance_mgr.reload_instrument_lookuptable(
    #             self.refdata_mgr.get_instrument_lookuptable_account_id())

    def process_message(self, msg):

        actions:list = []
        if isinstance(msg, list):
            if isinstance(msg[0], oms_proto.OrderRequest):
                actions = self.batch_process_orders(msg)
            elif isinstance(msg[0], oms_proto.OrderCancelRequest):
                actions = self.batch_process_cancels(msg)
            else:
                print("unrecognized message type" + str(msg))
        elif isinstance(msg, oms_proto.OrderRequest):
            actions = self.process_order(msg)
        elif isinstance(msg, gw.OrderReport):
            actions = self.process_order_report(msg)
        elif isinstance(msg, oms_proto.OrderCancelRequest):
            actions = self.process_cancel(msg)
        elif isinstance(msg, gw.BalanceUpdate):
            actions = self.process_balance_update(msg)
        elif isinstance(msg, OrderRecheckRequest):
            actions = self.process_recheck_order(msg)
        elif isinstance(msg, CancelRecheckRequest):
            actions = self.process_recheck_cancel(msg)
        elif isinstance(msg, oms_rpc.PanicRequest):
            actions = self.panic(msg)
        elif isinstance(msg, oms_rpc.DontPanicRequest):
            actions = self.dontpanic(msg)
        elif isinstance(msg, OMSCoreSchedRequest):
            if msg.request_type == SchedRequestType.CLEANUP:
                actions = self.process_cleanup(msg.ts_ms)
            elif msg.request_type == SchedRequestType.RELOAD_CONFIG:
                actions = self.reload_config()
        elif isinstance(msg, str):
            print(f"received message in om core: {msg}")
        else:
            print("unrecognized message type" + str(msg))

        return actions

    def init_state(self, curr_orders:list[OMSOrder], curr_balances: list[OMSPosition]):
        self.order_mgr.init_with_orders(curr_orders)
        self.balance_mgr.init_balances(curr_balances)

    def get_external_initializer(self):
        def init_oms(position_getter, order_getter):
            positions = position_getter()
            open_order_ids = self._get_open_order()
            order_states = order_getter(open_order_ids)
            self.sync_orders(order_states)
            self.sync_balance(positions)
        return init_oms

    def sync_balance(self, exch_balance_states):
        pass

    def sync_orders(self, exch_order_states):
        pass

    def validate_order_req(self, order_req: oms_proto.OrderRequest) -> ValidateResult:
        pass


    def process_order(self, order: oms_proto.OrderRequest) -> list[OMSAction]:
        try:
            return self._process_order(order)
        except Exception as ex:
            traceback.print_exc()
            return [self._book_error_order_and_publish(order, str(ex))]


    def batch_process_orders(self, orders: list[oms_proto.OrderRequest]):
        if self.logger_enabled:
            logger.info("batch processingo orders: order#=" + str(len(orders)))
        actions: list[OMSAction] = []
        order_actions: list[OMSAction] = []
        balance_update_actions: list[OMSAction] = []
        for order in orders:
            _actions = self.process_order(order)
            for a in _actions:
                if a.action_type == OMSActionType.SEND_ORDER_TO_GW:
                    order_actions.append(a)
                elif a.action_type == OMSActionType.UPDATE_BALANCE:
                    balance_update_actions.append(a)
                else:
                    actions.append(a)
        if order_actions:
            # group order actions based on action_meta['gw_name']
            gw_actions: dict[str, list[OMSAction]] = {}
            for oa in order_actions:
                gw_name = oa.action_meta['exch_name']
                if gw_name is None:
                    logger.error( f"gw_name is None for order action {oa}")
                    continue
                if gw_name not in gw_actions:
                    gw_actions[gw_name] = []
                gw_actions[gw_name].append(oa)
            for gw_name, gw_order_actions in gw_actions.items():
                gw_conf = self.gw_configs.get(gw_name)
                support_batch = gw_conf is not None and gw_conf.support_batch_order
                if len(gw_order_actions) > 1 and support_batch:
                    gw_batch_req = gw_rpc.ExchBatchSendOrdersRequest()
                    gw_batch_req.order_requests = [oa.action_data for oa in gw_order_actions]
                    batch_order_action = OMSAction(
                        action_data=gw_batch_req,
                        action_type=OMSActionType.BATCH_SEND_ORDER_TO_GW,
                        action_meta={"exch_name": gw_name} # assume all meta be the same
                    )
                    actions.append(batch_order_action)
                else:
                    actions.extend(gw_order_actions)

        if balance_update_actions:
            # add only the last balance update for each account_id/symbol
            account_symbol_map: dict[int, dict[str, oms.Position]] = {} # account_id -> symbol -> position
            for bua in balance_update_actions:
                account_id = bua.action_data.account_id
                if account_id not in account_symbol_map:
                    account_symbol_map[account_id] = {}
                for pos in bua.action_data.position_snapshots:
                    account_symbol_map[account_id][pos.instrument_code] = pos
            
            for account_id, symbol_map in account_symbol_map.items():
                balance_update_event = oms.PositionUpdateEvent()
                balance_update_event.account_id = account_id
                balance_update_event.position_snapshots = list(symbol_map.values())
                balance_update_event.timestamp = balance_update_actions[-1].action_data.timestamp
                balance_update_action = OMSAction(
                    action_type=OMSActionType.UPDATE_BALANCE,
                    action_meta={},
                    action_data=balance_update_event)
                actions.append(balance_update_action)
            
        return actions


    def _process_order(self, order: oms_proto.OrderRequest):
        ts_millis = order.timestamp if self.use_time_emulation else int(datetime.datetime.now().timestamp() * 1000)
        if self.logger_enabled:
            logger.info(f"processing order request, {pb_to_json(order)}")
        actions = []

        # step 0: check panic state:
        if order.account_id in self._panic_accounts:
            logger.info("order request from panic account, ignore: " + str(order.order_id))
            return []

        # step 1: resolve order context
        ctx = self._resolve_context(order.instrument_code, order.account_id)
        self.order_mgr.context_cache[order.order_id] = ctx

        ctx_validate_res = self._validate_context(ctx)
        if not ctx_validate_res.is_valid:
            ctx.errors.append(ctx_validate_res.error_msg)

        # step 1.1: do order level check
        if self.risk_check_enabled:
            max_order_size = ctx.trading_config.max_order_size
            if max_order_size is not None and order.qty > max_order_size:
                err_msg = f"order size {order.qty} exceeds max order size {max_order_size}"
                logger.error(err_msg)
                ctx.errors.append(err_msg)

        # step 2: calculate fund/position/limit change
        # step 3: fundmgr/positiomgr check fund/position change
        # step 4: apply change
        fund_change = None
        pos_change = None
        if not ctx.has_error() and ctx.trading_config.bookkeeping_balance:
            fund_change = self.balance_mgr.create_balance_change(account_id=order.account_id, orig_position=ctx.fund)
            pos_change = self.balance_mgr.create_balance_change(account_id=order.account_id, orig_position=ctx.position)
            if ctx.trading_config.use_margin:
                pass  # fund managed externally (GW for live, simulator for backtest)
            else:
                if order.buy_sell_type == proto_common.BuySellType.BS_BUY:
                    fund_change.avail_change = - order.qty * order.price # todo: what if market order?
                    fund_change.frozen_change = - fund_change.avail_change # positive
                elif order.buy_sell_type == proto_common.BuySellType.BS_SELL:
                    pos_change.avail_change = - order.qty
                    pos_change.frozen_change = - pos_change.avail_change # positive
                else:
                    raise Exception("unsupported buy sell type")
            if ctx.trading_config.balance_check:
                validate_res = self.balance_mgr.check_changes([fund_change, pos_change])
            else:
                validate_res = self._SUCCESS
            if not validate_res.is_valid:
                ctx.errors.append(validate_res.error_msg)

        # step 5: create order and gw order req
        oms_order: OMSOrder = self.order_mgr.create_order(order, ctx)
        ctx.order = oms_order
        if not ctx.has_error():
            # only send out order if no error
            gw_req = oms_order.gw_req
            gw_config = ctx.gw_config
            gw_req_action = OMSAction(
                action_type=OMSActionType.SEND_ORDER_TO_GW,
                action_meta={"exch_name": gw_config.gw_key},
                action_data=gw_req)
            actions.append(gw_req_action)

            if fund_change and pos_change:
                if self.logger_enabled:
                    logger.info(f"fund changes: {fund_change}")
                    logger.info(f"position changes: {pos_change}")
                symbols_updated = self.balance_mgr.apply_changes([fund_change, pos_change], ts_millis)
                if ctx.trading_config.publish_balance_on_book:
                    balance_update_event = self._generate_balance_update_events(
                        account_id=ctx.account_id,
                        symbols=symbols_updated,
                        timestamp=ts_millis
                    )
                    publish_balance_action = OMSAction(
                        action_type=OMSActionType.UPDATE_BALANCE,
                        action_meta={},
                        action_data=balance_update_event)
                    actions.append(publish_balance_action)

            # todo: publish balances

        else:
            # update order state to rejected and publish
            # oms_order.oms_order_state.order_status = oms.OrderStatus.ORDER_STATUS_REJECTED
            # err_msg = "; ".join(ctx.errors)
            # oms_order.oms_order_state.error_msg = err_msg
            rejection = common.Rejection(
                reason=common.RejectionReason.REJ_REASON_OMS_INVALID_STATE,
                recoverable=False,
                source=common.RejectionSource.OMS_REJECT
            )
            update_event = self.order_mgr.update_with_oms_error(
                oms_order=oms_order,
                order_req=order,
                error_msg="; ".join(ctx.errors),
                rejection=rejection
            )

            publish_order_action = OMSAction(
                action_type=OMSActionType.UPDATE_ORDER,
                action_meta={},
                action_data=update_event
            )
            actions.append(publish_order_action)

        if self.order_mgr.is_in_terminal_state(oms_order):
            persist_meta = {"set_expire": True}
        else:
            persist_meta = {"set_open": True}
        persist_order_action = OMSAction(
            action_type=OMSActionType.PERSIST_ORDER,
            action_meta=persist_meta,
            action_data=oms_order
        )

        actions.append(persist_order_action)


        return actions

    def _book_error_order_and_publish(self, order: oms_proto.OrderRequest, error_msg: str):
        rejection = common.Rejection(
            reason=common.RejectionReason.REJ_REASON_GW_INTERNAL_ERROR,
            recoverable=False,
            source=common.RejectionSource.OMS_REJECT
        )
        update_event = self.order_mgr.update_with_oms_error(
            oms_order=None,
            order_req=order,
            error_msg=error_msg,
            rejection=rejection
        )
        publish_order_action = OMSAction(
            action_type=OMSActionType.UPDATE_ORDER,
            action_meta={},
            action_data=update_event
        )
        return publish_order_action

    def process_cancel(self, order_cancel: oms_proto.OrderCancelRequest) -> list[OMSAction]:
        if self.logger_enabled:
            logger.info(f"processing order cancel request: {pb_to_json(order_cancel)}")
        orig_order_id = order_cancel.order_id
        if orig_order_id not in self.order_mgr.context_cache:
            order:OMSOrder = self.order_mgr.get_order_by_id(orig_order_id)
            if order is None:
                logger.error(f"order cancel request for unknown order: {orig_order_id}")
                return []
            else:
                # try reconstructing the context for this order
                ctx = self._resolve_context(order.oms_order_state.instrument, order.account_id)
                ctx.order = order # add order back to ctx
                self.order_mgr.context_cache[order.order_id] = ctx

        ctx = self.order_mgr.context_cache.get(orig_order_id, None)
        if ctx is None:
            raise Exception(f"cannot find or reconstruct context for order: {orig_order_id}")

        ctx.order.cancel_attempts += 1
        cancel_required_fields = ctx.gw_config.cancel_required_fields
        orderstate = ctx.order

        if orderstate is None:
            logger.error(f"order cancel request for unknown order: {orig_order_id}")
            return []

        rej: common.Rejection = None        
        if self.order_mgr.is_in_terminal_state(orderstate):
            if self.logger_enabled:
                logger.info(f"order cancel request for order in terminal state: {orig_order_id}")

            rej = common.Rejection(
                reason=common.RejectionReason.REJ_REASON_OMS_INVALID_STATE, # TODO: use a different state
                source=common.RejectionSource.OMS_REJECT,
                error_message="order already in terminal state",
                recoverable=True
            )

        exch_order_ref = orderstate.oms_order_state.exch_order_ref

        if exch_order_ref is None or exch_order_ref == '':
            logger.error(f"no exch order ref found for order: {orig_order_id}")
            rej = common.Rejection(
                reason=common.RejectionReason.REJ_REASON_OMS_INVALID_STATE, # TODO: use a different state
                source=common.RejectionSource.OMS_REJECT,
                recoverable=True
            )

        if rej is not None:
            oue = self.order_mgr.update_with_gw_error(
                order=orderstate,
                error_msg=rej.error_message,
                exec_type=oms.ExecType.EXEC_TYPE_CANCEL,
                ts=order_cancel.timestamp,
                rejection=rej
            )
            if oue:
                return [OMSAction(action_type=OMSActionType.UPDATE_ORDER,
                                  action_meta={},
                                  action_data=oue)]
            return []
        extra_info = proto_common.ExtraData()
        extra_info.data_map = {}
        for required_field in cancel_required_fields:
            val = str(getattr(orderstate.oms_order_state, required_field))
            extra_info.data_map[required_field] = val

        gw_cancel = gw_rpc.ExchCancelOrderRequest(
            exch_order_ref=exch_order_ref,
            order_id=orig_order_id,
            exch_specific_params=extra_info,
            timestamp=order_cancel.timestamp
        )
        # mark the order as cancel pending
        ctx.order.cancel_req = gw_cancel


        cancel_action = OMSAction(action_type=OMSActionType.SEND_CANCEL_TO_GW,
                                  action_meta={"exch_name": ctx.route.gw_key},
                                  action_data=gw_cancel)

        return [cancel_action]

    def batch_process_cancels(self, canecls: list[oms_proto.OrderCancelRequest]):
        if self.logger_enabled:
            logger.info(f"batch processing order cancel requests: cancel#={len(canecls)}")
        actions: list[OMSAction] = []
        cancel_actions: list[OMSAction] = []
        for cancel in canecls:
            _actions = self.process_cancel(cancel)
            for a in _actions:
                if a.action_type == OMSActionType.SEND_CANCEL_TO_GW:
                    cancel_actions.append(a)
                else:
                    actions.append(a)

        if cancel_actions:
            # group cancels by action_meta['exch_name']
            exch_cancel_actions: dict[str, list[OMSAction]] = {}
            for ca in cancel_actions:
                exch_name = ca.action_meta['exch_name']
                if exch_name not in exch_cancel_actions:
                    exch_cancel_actions[exch_name] = []
                exch_cancel_actions[exch_name].append(ca)

            for exch_name, batch_cancel_actions in exch_cancel_actions.items():
                gw_conf = self.gw_configs.get(exch_name)
                support_batch = gw_conf is not None and gw_conf.support_batch_cancel
                if len(batch_cancel_actions) > 1 and support_batch:
                    gw_cancel_req = gw_rpc.ExchBatchCancelOrdersRequest()
                    gw_cancel_req.cancel_requests = [ca.action_data for ca in batch_cancel_actions]
                    batch_cancel_action = OMSAction(
                        action_data=gw_cancel_req,
                        action_type=OMSActionType.BATCH_SEND_CANCEL_TO_GW,
                        action_meta={"exch_name": exch_name}  # assume all meta be the same
                    )
                    actions.append(batch_cancel_action)
                else:
                    actions.extend(batch_cancel_actions)
        return actions


    def process_order_report(self, order_report: gw.OrderReport):
        ts_millis = order_report.update_timestamp
        actions = []
        gw_key = order_report.exchange
        exch_order_ref = order_report.exch_order_ref

        if self.logger_enabled:
            logger.info(f"processing order report: {pb_to_json(order_report, pretty=True).decode('utf-8')}")

        # handle external non-tq order:
        # condition: if order report is marked as non-tq,
        # or the source of this report is unknown but the order is marked as external already (by prev. report)
        # otherwise, we need to go through the normal processing just in case
        if self.handle_non_tq_order_reports:
            gw_key = order_report.exchange
            # need to guess its account first
            account_id = order_report.account_id
            if account_id is None or account_id == 0:
                account_ids = self.gw_key_to_accountids.get(gw_key)
                if account_ids and len(account_ids) == 1:
                    account_id = list(account_ids)[0]
            if not account_id:
                logger.error(f"cannot determine account_id for order report; discarding: {pb_to_json(order_report)}")
                return []
            order_report.account_id = account_id
            if order_report.order_source_type == gw.OrderSourceType.ORDER_SOURCE_NON_TQ or \
                    (order_report.order_source_type == gw.OrderSourceType.ORDER_SOURCE_UNKNOWN and \
                     self.order_mgr.is_marked_as_external(exch_order_ref=exch_order_ref, gw_key=gw_key)):
                # for non-tq orders, only do necessary bookkeeping

                if self.logger_enabled:
                    logger.info(f"handling non-tq order report")
                update_events = []
                oue = self.order_mgr.handle_external_order_report(order_report)
                update_events.append(oue)


                # also need to check the pending report cache
                if order_report.order_source_type == gw.OrderSourceType.ORDER_SOURCE_NON_TQ and len(self._pending_order_reports) > 0:

                    reports_to_replay: list[gw.OrderReport] = self._pop_pending_order_report_cache_if_any(
                        gw_key=gw_key, exch_order_ref=exch_order_ref)
                    for report in reports_to_replay:
                        logger.info("replaying pending order report for non tq order " + str(report.exch_order_ref))
                        update_events.append(self.order_mgr.handle_external_order_report(report))

                for event in update_events:
                    order_update_action = OMSAction(action_type=OMSActionType.UPDATE_ORDER,
                                                    action_meta={},
                                                    action_data=event)

                    order = self.order_mgr.get_external_order(gw_key, exch_order_ref)
                    # store to redis
                    if order and self.order_mgr.is_in_terminal_state(order):
                        persist_meta = {"set_expire": True, "set_closed": True}
                    else:
                        persist_meta = {}

                    order_store_action = OMSAction(action_type=OMSActionType.PERSIST_ORDER,
                                                   action_meta=persist_meta,
                                                   action_data=order)

                    actions.append(order_update_action)
                    actions.append(order_store_action)

                return actions

        if self._order_id_available(order_report):
            ctx = self.order_mgr.context_cache.get(order_report.order_id)
            if ctx is None:
                ctx = self._resolve_context_report(order_id=order_report.order_id)
        else:
            order: OMSOrder = self.order_mgr.get_order_by_exch_ref(gw_key=gw_key, exch_order_ref=exch_order_ref)
            if order is None:
                logger.error(f"order report for unknown order: {exch_order_ref}, adding to unknown order report cache")
                self._add_to_unknown_order_report_cache(gw_key=gw_key, order_report=order_report)
                return []
            else:
                ctx = self._resolve_context_report(order_id=order.order_id)

        # step1: update balance if needed
        if ctx.trading_config.bookkeeping_balance:
            balance_changes = self._calc_balance_change_with_report(ctx=ctx, order_report=order_report)
            if balance_changes:
                if self.logger_enabled:
                    logger.info(f"balance changes: {balance_changes}")
                updated_symbols = self.balance_mgr.apply_changes(balance_changes, ts_millis)
                balance_updates = self._generate_balance_update_events(
                    account_id=ctx.account_id, symbols=updated_symbols, use_exch_data=False, timestamp=ts_millis)
                balance_update_action = OMSAction(
                    action_type=OMSActionType.UPDATE_BALANCE,
                    action_meta={},
                    action_data=balance_updates)
                actions.append(balance_update_action)


        # step1: update order state
        update_events = self.order_mgr.update_with_report(order_report)

        # step1.1: check pending order report cache
        if len(self._pending_order_reports) > 0:
            reports_to_replay: list[gw.OrderReport] = self._check_pending_order_report_cache(ctx)
            for report in reports_to_replay:
                logger.info("replaying pending order report for " + str(report.exch_order_ref))
                update_events.extend(self.order_mgr.update_with_report(report))


        # step3: send action (update order/balance/limit)
        if len(update_events):
            for event in update_events:
                # publish to NATS
                order_update_action = OMSAction(action_type=OMSActionType.UPDATE_ORDER,
                                                action_meta={},
                                                action_data=event)

                order = self.order_mgr.get_order_by_id(event.order_id)
                # store to redis
                if self.order_mgr.is_in_terminal_state(order):
                    persist_meta = {"set_expire": True, "set_closed": True}
                else:
                    persist_meta = {}

                order_store_action = OMSAction(action_type=OMSActionType.PERSIST_ORDER,
                                               action_meta=persist_meta,
                                               action_data=order)

                actions.append(order_update_action)
                actions.append(order_store_action)

        return actions


    def process_balance_update(self, balance_updates: gw.BalanceUpdate):
        if self.logger_enabled:
            logger.debug(f"processing balance update: {pb_to_json(balance_updates)}")
        updated_account_id: int = self.balance_mgr.merge_gw_balance_updates(balance_updates)
        if updated_account_id is not None:
            pue = self._generate_balance_snapshot(account_id=updated_account_id, use_exch_data=True,
                                            timestamp=balance_updates.balances[0].update_timestamp)
            balance_update_action = OMSAction(action_type=OMSActionType.UPDATE_BALANCE,
                                              action_meta={},
                                              action_data=pue)
            return [balance_update_action]
        return []


    def process_cleanup(self, ts: int):

        if self.handle_non_tq_order_reports:
            reports_to_replay = []
            to_be_deleted = []
            for gw_key in self._pending_order_reports:
                for exch_order_id in self._pending_order_reports[gw_key]:
                    reports = self._pending_order_reports[gw_key][exch_order_id]
                    # filter out reports whose ts is older than 10 minutes
                    if len(reports) > 0 and ts - reports[-1].update_timestamp > 60 * 10 * 1000:
                        logger.info(f"cleaning up timeout pending order reports for {gw_key}/{exch_order_id}, remaining: {len(reports)}")

                        to_be_deleted.append((gw_key, exch_order_id))
                        for report in reports:
                            # markt the timeout report as non-tq
                            report.order_source_type = gw.OrderSourceType.ORDER_SOURCE_NON_TQ
                            reports_to_replay.append(report)
            actions = []
            for gw_key, exch_order_id in to_be_deleted:
                del self._pending_order_reports[gw_key][exch_order_id]
                if len(self._pending_order_reports[gw_key]) == 0:
                    del self._pending_order_reports[gw_key]

            for report in reports_to_replay:
                actions.extend(self.process_order_report(report))

            return actions
        else:
            return []

    def get_open_orders(self, account_id: int):
        return self.order_mgr.get_open_orders(account_id)


    def get_orders_outofsync_maybe(self) -> list[ExchOrderRef]:
        order_refs = []
        for account_id in self._account_ids:
            _open_orders = self.order_mgr.get_open_orders(account_id)
            for o in _open_orders:
                if o.cancel_attempts >= 1 or o.oms_order_state.order_status == oms.OrderStatus.ORDER_STATUS_PENDING:
                    order_refs.append(ExchOrderRef(
                        gw_key=o.oms_order_state.gw_key,
                        account_id=o.account_id,
                        exch_order_id=o.exch_order_ref,
                        order_id=o.order_id,
                        symbol_exch=o.oms_order_state.instrument_exch))

        return order_refs


    def get_account_balance(self, account_id: int, use_exch_data=True) -> list[OMSPosition]:
        gw_conf: GwConfigEntry = self.accountid_to_gwconfig.get(account_id)
        if gw_conf is None:
            return []
        _use_exch_data = not gw_conf.calc_balance_needed
        return self.balance_mgr.get_balances_for_account(
            account_id, use_exch_data=_use_exch_data)

    def process_cmd(self, cmd):
        pass



    def process_recheck_order(self, recheck_req: OrderRecheckRequest):
        self._total_retries_orders += 1
        order = self.order_mgr.get_order_by_id(recheck_req.order_id)
        if order and order.oms_order_state.order_status == oms.OrderStatus.ORDER_STATUS_PENDING:
            # if still pending, it means gw actually didn't receive; so mark this order in error
            err_msg = "Order still pending after recheck"

            can_retry = self._total_retries_orders < 20
            rejection = common.Rejection(
                reason=common.RejectionReason.REJ_REASON_NETWORK_ERROR,
                source=common.RejectionSource.EXCH_REJECT,
                recoverable=can_retry
            )
            oue = self.order_mgr.update_with_gw_error(
                order=order, error_msg=err_msg,
                exec_type=oms.ExecType.EXEC_TYPE_PLACING_ORDER,
                ts=recheck_req.timestamp + recheck_req.check_delay_in_sec*1000,
                rejection=rejection)

            if oue:
                persist_meta = {"set_expire": True, "set_closed": True}

                order_store_action = OMSAction(
                    action_type=OMSActionType.PERSIST_ORDER,
                    action_meta=persist_meta,
                    action_data=order)
                order_update_action = OMSAction(
                    action_type=OMSActionType.UPDATE_ORDER,
                    action_meta={},
                    action_data=oue)
                return [order_update_action,
                        order_store_action]
        return []


    def process_recheck_cancel(self, recheck_req: CancelRecheckRequest):
        self._total_retries_cancels += 1
        order = self.order_mgr.get_order_by_id(recheck_req.orig_request.order_id)
        if order and not self.order_mgr.is_in_terminal_state(order):
            # cancel doesn't go through, mark one cancel rejection exec message
            err_msg = "cancel doesn't go through after recheck"
            should_retry = self._total_retries_cancels < 20

            rejection = common.Rejection(
                reason=common.RejectionReason.REJ_REASON_NETWORK_ERROR,
                source=common.RejectionSource.EXCH_REJECT,
                recoverable=should_retry
            )

            oue = self.order_mgr.update_with_gw_error(
                order=order,
                error_msg=err_msg,
                exec_type=oms.ExecType.EXEC_TYPE_CANCEL,
                ts=recheck_req.orig_request.timestamp + recheck_req.check_delay_in_sec*1000,
                rejection=rejection
            )
            if oue:
                return [OMSAction(action_type=OMSActionType.UPDATE_ORDER,
                                  action_meta={},
                                  action_data=oue)]
        return []


    def panic(self, panic_request: oms_rpc.PanicRequest):
        logger.info("Received panic request: " + str(panic_request))
        self._panic_accounts.add(panic_request.panic_account_id)
        logger.info("Panic!!!! Shutting down trading for " + str(panic_request.panic_account_id))

        # TODO: cancel all orders for this account
        return []

    def dontpanic(self, dontpanic_request: oms_rpc.DontPanicRequest):
        logger.info("Received dontpanic request: " + str(dontpanic_request))
        if dontpanic_request.panic_account_id in self._panic_accounts:
            self._panic_accounts.remove(dontpanic_request.panic_account_id)
            logger.info("Dont Panic!!!! Resuming trading for " + str(dontpanic_request.panic_account_id))
        else:
            logger.info("account not panicked: " + str(dontpanic_request.panic_account_id))
        return []


    def _resolve_context_report(self, order_id: int) -> OrderContext:
        order = self.order_mgr.get_order_by_id(order_id)
        if order is None:
            return None

        symbol = order.oms_req.instrument_code
        account_id = order.account_id

        ctx = self._resolve_context(instrument_code=symbol, account_id=account_id)
        ctx.order = order
        return ctx


    def _resolve_context(self, instrument_code: str, account_id: int) -> OrderContext:
        symbol_ref = self.refdata.get(instrument_code)
        if symbol_ref:
            is_spot_like = symbol_ref.instrument_type in SPOT_LIKE_TYPES
            pos_symbol = symbol_ref.base_asset if is_spot_like else symbol_ref.instrument_id
            fund_symbol = symbol_ref.quote_asset if is_spot_like \
                else (symbol_ref.settlement_asset or symbol_ref.quote_asset)
            pos_balance = self.balance_mgr.get_balance_for_symbol(account_id, pos_symbol, create_if_not_exists=True)
            fund_balance = self.balance_mgr.get_balance_for_symbol(account_id, fund_symbol, create_if_not_exists=True)
        else:
            pos_balance=None
            fund_balance=None
        ctx = OrderContext(
            account_id=account_id,
            position=pos_balance,
            fund=fund_balance,
            symbol_ref=symbol_ref,
            route=self.routes.get(account_id),
            trading_config=self.trading_configs.get(instrument_code, None)
        )
        ctx.gw_config = self.gw_configs.get(ctx.route.gw_key)
        return ctx


    def _generate_balance_snapshot(self, account_id: int, use_exch_data=False, timestamp: int=None) -> oms.PositionUpdateEvent:
        balances = self.balance_mgr.get_balances_for_account(account_id, use_exch_data=use_exch_data)
        pos_update_events = oms.PositionUpdateEvent()
        pos_update_events.account_id = account_id
        pos_updates = [b.position_state for b in balances]

        pos_update_events.position_snapshots = pos_updates

        pos_update_events.timestamp = int(timestamp)
        return pos_update_events


    def _generate_balance_update_events(self, account_id: int, symbols: set[str],
                                        use_exch_data=False, timestamp: int=None) -> oms.PositionUpdateEvent:
        balances = [self.balance_mgr.get_balance_for_symbol(
            account_id=account_id, symbol=symbol, use_exch_data=use_exch_data)
            for symbol in symbols]

        pos_update_events = oms.PositionUpdateEvent()
        pos_update_events.account_id = account_id
        pos_updates = [b.position_state for b in balances]

        pos_update_events.position_snapshots = pos_updates

        pos_update_events.timestamp = int(timestamp)
        return pos_update_events



    def _validate_context(self, ctx: OrderContext) -> ValidateResult:
        if ctx.symbol_ref is None:
            return ValidateResult(False, f"Instrument not found")

        if ctx.gw_config is None:
            return ValidateResult(False, f"Exchange gw not found")

        return self._SUCCESS

    def _order_id_available(self, order_report: gw.OrderReport) -> bool:
        return order_report.order_id and order_report.order_id != 0



    def _add_to_unknown_order_report_cache(self, gw_key: str, order_report: gw.OrderReport):
        exch_order_id = order_report.exch_order_ref
        if exch_order_id is None or exch_order_id == "":
            return

        if gw_key not in self._pending_order_reports:
            self._pending_order_reports[gw_key] = {exch_order_id: []}
        if exch_order_id not in self._pending_order_reports[gw_key]:
            self._pending_order_reports[gw_key][exch_order_id] = []
        self._pending_order_reports[gw_key][exch_order_id].append(order_report)

    def _check_pending_order_report_cache(self, order_ctx: OrderContext) -> list[gw.OrderReport]:
        if order_ctx is None or order_ctx.order is None:
            return []
        exch_order_id = order_ctx.order.exch_order_ref
        order_id = order_ctx.order.order_id
        gw_key = order_ctx.order.oms_order_state.gw_key

        return self._pop_pending_order_report_cache_if_any(gw_key, exch_order_id)


    def _pop_pending_order_report_cache_if_any(self, gw_key:str, exch_order_ref: str) -> list[gw.OrderReport]:
        if gw_key not in self._pending_order_reports:
            return []
        if exch_order_ref not in self._pending_order_reports[gw_key]:
            return []
        pending_reports = self._pending_order_reports[gw_key][exch_order_ref]
        del self._pending_order_reports[gw_key][exch_order_ref]

        if len(self._pending_order_reports[gw_key]) == 0:
            del self._pending_order_reports[gw_key]

        return pending_reports

    def _calc_balance_change_with_report(self, ctx: OrderContext, order_report: gw.OrderReport):
        '''
        ## balance update general principle:
        1. eventual consistency;
        2. during the lifecycle of an order, make sure the calculation is more stringent than the exchange;
        3. message assumption:
            1) trade and fee message will not be duplicated or lost (guaranteed ONLY-ONCE delivery);
            2) state message could be duplicated (safely resent);
            3) order error exec message could be overriden by followed state message;
            4) order cancel error message has no effect on balance;
        ## Spot trading report update balance rule:
        1. buy order:
            1). fills will increase spot position;
            2). cancel/completed/gw-originated-error state will adjust frozen/available fund balance
            3). fees will decrease fund balance
            4). OMS-originated-error will NOT adjust fund balance (this may persist)
        2. sell order:
            1). fills will increase fund balance;
            2). cancel/completed state will adjust frozen/available spot position
            3). fees will decrease fund balance
            4). OMS-originated-error will NOT adjust position balance (this may persist)

        ## Margin trading report update balance rule:
        1. order that increases net position:
            1) fills will increase the position;
        2. order that decreases net position:
            1) fills will decrease the position;


        '''

        position_changes = []

        current_order = ctx.order.oms_order_state
        original_order_req = ctx.order.oms_req
        current_order_status = ctx.order .oms_order_state.order_status

        for gw_report_entry in order_report.order_report_entries:
            if gw_report_entry.report_type == gw.OrderReportType.ORDER_REP_TYPE_STATE:
                if self.order_mgr.is_in_terminal_state(ctx.order):
                    continue
                gw_order_state = gw_report_entry.order_state_report.exch_order_status
                if gw_order_state == gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_BOOKED or \
                    gw_order_state == gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_PARTIAL_FILLED:
                    continue
                else:
                    if gw_order_state == gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_CANCELLED or \
                        gw_order_state == gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_EXCH_REJECTED or \
                        gw_order_state == gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_FILLED:
                        if not ctx.trading_config.use_margin: # spot trading
                            if ctx.order.oms_order_state.buy_sell_type == common.BuySellType.BS_BUY:
                                fund_change = self.balance_mgr.create_balance_change(
                                    account_id=ctx.account_id,
                                    orig_position=ctx.fund)
                                remaining_qty = gw_report_entry.order_state_report.unfilled_qty
                                filled_qty = gw_report_entry.order_state_report.filled_qty
                                qty = original_order_req.qty

                                fund_change.frozen_change = - original_order_req.price * qty
                                fund_change.avail_change = original_order_req.price * remaining_qty
                                fund_change.total_change = - original_order_req.price * filled_qty
                                position_changes.append(fund_change)
                            elif ctx.order.oms_order_state.buy_sell_type == common.BuySellType.BS_SELL:
                                pos_change = self.balance_mgr.create_balance_change(
                                    account_id=ctx.account_id,
                                    orig_position=ctx.position)
                                remaining_qty = gw_report_entry.order_state_report.unfilled_qty
                                filled_qty = gw_report_entry.order_state_report.filled_qty
                                qty = original_order_req.qty
                                pos_change.frozen_change = - qty
                                pos_change.avail_change = remaining_qty
                                pos_change.total_change = - filled_qty
                                position_changes.append(pos_change)
                        else: # margin trading
                            # TODO:
                            pass
            elif gw_report_entry.report_type == gw.OrderReportType.ORDER_REP_TYPE_TRADE:
                gw_trade = gw_report_entry.trade_report
                if gw_trade.filled_qty > 0:
                    if not ctx.trading_config.use_margin: # spot trading
                        if ctx.order.oms_order_state.buy_sell_type == common.BuySellType.BS_BUY:
                            pos_change = self.balance_mgr.create_balance_change(
                                account_id=ctx.account_id,
                                orig_position=ctx.position)
                            filled_qty = gw_trade.filled_qty
                            pos_change.avail_change = filled_qty
                            pos_change.total_change = filled_qty
                            position_changes.append(pos_change)
                        elif ctx.order.oms_order_state.buy_sell_type == common.BuySellType.BS_SELL:
                            fund_change = self.balance_mgr.create_balance_change(
                                account_id=ctx.account_id,
                                orig_position=ctx.fund)
                            filled_qty = gw_trade.filled_qty
                            filled_price = gw_trade.filled_price
                            fund_change.avail_change = filled_qty * filled_price
                            fund_change.total_change = fund_change.avail_change
                            position_changes.append(fund_change)
                    else:  # margin trading — track position, fund managed externally
                        pos_change = self.balance_mgr.create_balance_change(
                            account_id=ctx.account_id, orig_position=ctx.position)
                        filled_qty = gw_trade.filled_qty
                        if ctx.order.oms_order_state.buy_sell_type == common.BuySellType.BS_BUY:
                            pos_change.avail_change = filled_qty
                            pos_change.total_change = filled_qty
                        elif ctx.order.oms_order_state.buy_sell_type == common.BuySellType.BS_SELL:
                            pos_change.avail_change = -filled_qty
                            pos_change.total_change = -filled_qty
                        position_changes.append(pos_change)


        return position_changes




