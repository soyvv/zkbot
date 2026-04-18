import asyncio
import traceback
import inspect

import betterproto
import traceback as tb
import signal
from zk_client.tqclient import TQClient, TQClientConfig, TQOrder, TQCancel, rpc
from contextlib import suppress

from zk_strategy.api import *
from zk_strategy.strategy_core import StrategyTemplate, StrategyConfig
from zk_strategy.timer import TimerManager
from zk_strategy.models import SAction, TimerEvent, SActionType, StrategyOrder, StrategyCancel, StrategyLog, TimerSubscription
import zk_proto_betterproto.strategy as strat_pb

from loguru import logger


class StrategyRealtimeEngine:
    def __init__(
        self,
        strategy_config: StrategyConfig = None,
        tq_client_config: TQClientConfig = None,
        tq_client: TQClient = None,
        engine_key: str = None,
        execution_id: str = None,
        enable_startup_data_init=False,
        publish_strategy_event=False,
        instance_id=None
    ):
        self._is_configured = False
        self._error_flag = False
        self.instance_id = instance_id
        if strategy_config is None:
            # load strategy config (from db) later by the caller of the engine
            return

        self._configure(
            strategy_config=strategy_config,
            tq_client_config=tq_client_config,
            tq_client=tq_client,
            engine_key=engine_key,
            execution_id=execution_id,
            enable_startup_data_init=enable_startup_data_init,
            publish_strategy_event=publish_strategy_event,
        )

    def _configure(self,
        strategy_config: StrategyConfig,
        tq_client_config: TQClientConfig=None,
        tq_client: TQClient=None,
        engine_key: str = None,
        execution_id: str = None,
        enable_startup_data_init=False,
        publish_strategy_event=False
    ):
        if tq_client is not None:
            self.tq_client = tq_client
            self.tq_client_config = tq_client.config
        else:
            self.tq_client_config = tq_client_config
            self.tq_client = TQClient(tq_client_config, source_id=engine_key)

        self.strategy_config = strategy_config
        self.q = asyncio.Queue(128)

        self.timer_mgr = TimerManager()
        self.account_ids = set(self.strategy_config.account_ids)
        self.enable_startup_data_init = enable_startup_data_init
        self.publish_strategy_event = publish_strategy_event
        self.event_subject_log = f"tq.strategy.log.{self.strategy_config.strategy_name}"
        self.event_subject_order = f"tq.strategy.order.{self.strategy_config.strategy_name}"
        self.event_subject_cancel = f"tq.strategy.cancel.{self.strategy_config.strategy_name}"
        self.event_subject_lifecycle = f"tq.strategy.lifecycle.{self.strategy_config.strategy_name}"
        self.event_subject_signal = f"tq.strategy.signal.{self.strategy_config.strategy_name}"
        self.event_subject_notify = f"tq.strategy.notify.{self.strategy_config.strategy_name}"
        self.execution_id = execution_id

        self._is_configured = True
        self._error_flag = False

    async def start(self):
        if not self._is_configured:
            raise RuntimeError("engine not configured")
        await self.tq_client.start(init_timeout=10)
        

    async def start_subscriptions(self):

        subscribed_symbols = set(self.strategy_config.symbols)
        def handle_rtmd(os: rtmd.TickData):
            if os.instrument_code in subscribed_symbols:
                try:
                    self.q.put_nowait(os)
                except asyncio.QueueFull:
                    logger.error("queue full, dropping tick data")
        await self.tq_client.subscribe_rtmd(handle_rtmd)

        def handle_kline(kline: rtmd.Kline):
            try:
                self.q.put_nowait(kline)
            except asyncio.QueueFull:
                logger.error("queue full, dropping kline data")
        await self.tq_client.subscribe_rtmd_kline(handle_kline)

        if self.tq_client_config.rt_signal_endpoints:
            def handle_signal(sig: rtmd.RealtimeSignal):
                try:
                    self.q.put_nowait(sig)
                except asyncio.QueueFull:
                    logger.error("queue full, dropping signal data")
            await self.tq_client.subscribe_signal(handle_signal)

        def handle_order_update(ou: oms.OrderUpdateEvent):
            #o: oms.Order = ou.order_snapshot
            #if ou.order_snapshot.account_id in self.account_ids:

            # order from this strategy
            if ou.order_snapshot.source_id == self.strategy_config.strategy_name:
                try:
                    self.q.put_nowait(ou)
                except asyncio.QueueFull:
                    logger.error("queue full, dropping order update data")

            # order from accounts but with trades
            if ou.account_id in self.account_ids and \
                betterproto.serialized_on_wire(ou.last_trade):
                try:
                    self.q.put_nowait(ou)
                except asyncio.QueueFull:
                    logger.error("queue full, dropping order update data")
        await self.tq_client.subscribe_order_update(handle_order_update)


        def handle_balance_update(pu: oms.PositionUpdateEvent):
            if pu.account_id in self.account_ids:
                try:
                    self.q.put_nowait(pu)
                except asyncio.QueueFull:
                    logger.error("queue full, dropping balance update data")

        await self.tq_client.subscribe_balance_update(handle_balance_update)

    async def run_cmd_subscriber(self):
        pass

    async def run_clock(self):
        while True:
            await asyncio.sleep(1)
            try:
                current_ts = datetime.now()
                timer_events = self.timer_mgr.generate_next_batch_events(current_ts)
            except:
                logger.error(tb.format_exc())
                continue
            for timer_event in timer_events:
                await self.q.put(timer_event)

    async def run_strategy(self):
        if not self._is_configured:
            raise RuntimeError("engine not configured")

        self._error_flag = False

        await self._publish_lifecycle_event(status=strat_pb.StrategyStatusType.STRAT_STATUS_INITIALIZING)



        try:
            # initialize strategy
            # step 1: create strategy from config
            strategy = StrategyTemplate(strategy_config=self.strategy_config, instance_id=self.instance_id)

            # stash the client in strategy context just in case
            strategy.strategy_ctx._tq_client = self.tq_client
            # create strategy instance and send strategy lifecycle event

            #self.tq_client.publish()
            # step1: create strategy
            actions = strategy.create_strategy(datetime.now())
            if actions:
                await self._process_actions(actions, strategy)

            # step 2: load account data from oms(open orders and account balances)
            account_balances: list[oms.Position] = []
            open_orders :list[oms.Order] = []
            for account_id in self.account_ids:
                logger.info(f"retrieving open orders and account balances from OMS for {account_id}...")
                _open_orders = await self.tq_client.get_open_orders(account_id)
                open_orders.extend(_open_orders)
                logger.info(f"{len(_open_orders)} orders retrieved from OMS for {account_id}")
                _bs: list[oms.Position] = await self.tq_client.get_account_balance(account_id)
                logger.info(f"{len(_bs)} account balance entries retrieved from OMS")
                account_balances.extend(_bs)

            strategy.tq.state_proxy.init_orders_and_balances(init_orders=open_orders, init_balances=account_balances)


            # step 3: load user define strategy init data
            if hasattr(strategy.user_strategy, "__tq_init__") and strategy.user_strategy.__tq_init__:
                # todo: check the signature of the function
                logger.info("init function found, calling to get init data...")
                init_func = strategy.user_strategy.__tq_init__
                try:
                    if inspect.iscoroutinefunction(init_func):
                        data = await init_func(self.strategy_config.custom_params, datetime.now(), self.tq_client)
                    else:
                        data = init_func(self.strategy_config.custom_params, datetime.now(), self.tq_client)
                    strategy.tq.__tq_init_output__ = data
                except:
                    logger.error("error calling init function")
                    logger.error(traceback.format_exc())

            logger.info("start subscriptions...")
            await self.start_subscriptions()


            # todo: reinit the strategy
            init_actions = strategy.init_strategy(self.strategy_config.custom_params, datetime.now())
            if init_actions:
                await self._process_actions(init_actions, strategy)
        except:
            self._error_flag = True
            msg = traceback.format_exc()
            logger.error(msg)
            if self.publish_strategy_event:
                await self._publish_lifecycle_event(status=strat_pb.StrategyStatusType.STRAT_STATUS_EXCEPTIONED,
                                           data={'error': msg})
            return

        if not self._error_flag:
            if self.publish_strategy_event:
                await self._publish_lifecycle_event(status=strat_pb.StrategyStatusType.STRAT_STATUS_RUNNING)
        else:
            return

        while True:
            first_event = await self.q.get()
            events = [first_event]
            extra_events_num = self.q.qsize()
            tick_skipped = 0
            if extra_events_num > 0:
                last_tick_idx_dict: dict[tuple[str, str], int] = {}  # (symbol, exchange) -> idx
                tick_skipped = 0
                for _ in range(extra_events_num):
                    try:
                        next_event = self.q.get_nowait()
                    except asyncio.QueueEmpty:
                        logger.warning("queue empty while processing extra events")
                        break
                    if isinstance(next_event, rtmd.TickData):
                        symbol, exchange = next_event.instrument_code, next_event.exchange
                        last_tick_idx = last_tick_idx_dict.get((symbol, exchange))
                        if last_tick_idx is not None:
                            events[last_tick_idx] = next_event
                            tick_skipped += 1
                        else:
                            events.append(next_event)
                            last_tick_idx_dict[(symbol, exchange)] = len(events) - 1
                    else:
                        events.append(next_event)

            # for all tickdata, only the last one is processed
            # TODO: but the order of the events might be out of order
            # TODO: sort the events by timestamp?
            if tick_skipped > 0:
                logger.info(f"skipped {tick_skipped} tick data")

            if self._error_flag:
                raise RuntimeError("strategy is in error state, exiting...")
            if len(events) == 1:
                await self._process_one_event(first_event, strategy)
            else:
                logger.info(f"batch processing {len(events)} events")
                logger.debug("batched event types: " + str([type(e) for e in events]))
                for event in events:
                    await self._process_one_event(event, strategy)



    async def _process_one_event(self, event, strategy: StrategyTemplate):
        try:
            if isinstance(event, rtmd.TickData):
                actions = strategy.process_tick_event(event)
            elif isinstance(event, rtmd.RealtimeSignal):
                actions = strategy.process_signal_event(event)
            elif isinstance(event, oms.OrderUpdateEvent):
                actions = strategy.process_order_update_event(event)
            elif isinstance(event, oms.PositionUpdateEvent):
                actions = strategy.process_position_update_event(event)
            elif isinstance(event, TimerEvent):
                actions = strategy.process_timer_event(event)
            elif isinstance(event, rtmd.Kline):
                actions = strategy.process_bar_event(event)
            elif isinstance(event, strat_pb.StrategyCommand):
                actions = strategy.process_strategy_cmd(event)
            else:
                logger.error("unknown event type: " + str(type(event)))
                actions = []

            if actions:
                await self._process_actions(actions, strategy)

            if strategy.strategy_ctx.is_in_error_state:
                self._error_flag = True
                if self.publish_strategy_event:
                    await self._publish_lifecycle_event(
                        status=strat_pb.StrategyStatusType.STRAT_STATUS_ERROR,
                        data={'error': strategy.strategy_ctx.errors})
        except Exception as ex:
            self._error_flag = True
            err_msg = traceback.format_exc()
            logger.error("error during strategy processing: " + err_msg)
            if self.publish_strategy_event:
                try:
                    await self._publish_lifecycle_event(
                        status=strat_pb.StrategyStatusType.STRAT_STATUS_ERROR,
                        data={'error': err_msg})
                except:
                    logger.error("error publishing strategy error event")
                    logger.error(traceback.format_exc())

    async def _process_actions(self, actions: list[SAction], strategy: StrategyTemplate):
        await self._process_trading_actions(actions)
        for action in actions:
            if action.action_type == SActionType.ORDER:
                strat_order: StrategyOrder = action.action_data
                if self.publish_strategy_event:
                    try:
                        strategy_order_event = self._build_order_event(strat_order)
                        await self.tq_client.publish(self.event_subject_order, strategy_order_event)
                    except:
                        logger.error(traceback.format_exc())


            elif action.action_type == SActionType.CANCEL:
                strat_cancel: StrategyCancel = action.action_data
                # await self.tq_client.cancel(strat_cancel.order_id)
                # todo: handle error
                if self.publish_strategy_event:
                    try:
                        cancel_event = self._build_cancel_event(strat_cancel)
                        await self.tq_client.publish(self.event_subject_cancel, cancel_event)
                    except:
                        logger.error(traceback.format_exc())

            elif action.action_type == SActionType.TIMER_SUB:
                timer_sub: TimerSubscription = action.action_data
                if timer_sub.scheduled_clock:
                    self.timer_mgr.subscribe_timer_clock(timer_key=timer_sub.timer_name, date_time=timer_sub.scheduled_clock)
                elif timer_sub.cron_expr:
                    self.timer_mgr.subscribe_timer_cron(timer_key=timer_sub.timer_name, cron_expression=timer_sub.cron_expr)

                strategy.register_timer(timer_key=timer_sub.timer_name,
                                        cb=timer_sub.cb, cb_kwargs=timer_sub.cb_extra_kwarg)
            elif action.action_type == SActionType.LOG:
                log_line = action.action_data.logline
                logger.info(log_line)
                # todo: log locally and send out
                if self.publish_strategy_event:
                    try:
                        log_event = self._build_log_event(action.action_data)
                        await self.tq_client.publish(self.event_subject_log, bytes(log_event))
                    except:
                        logger.error(traceback.format_exc())
            elif action.action_type == SActionType.SIGNAL:
                signal_data = action.action_data
                if self.publish_strategy_event:
                    try:
                        signal_event = signal_data
                        await self.tq_client.publish(self.event_subject_signal, bytes(signal_event))
                    except:
                        logger.error(traceback.format_exc())
            elif action.action_type == SActionType.NOTIFY:
                notify_data: strategy_pb.StrategyNotification = action.action_data
                if self.publish_strategy_event:
                    try:
                        headers = notify_data.message_meta_info
                        message = notify_data.message

                        # TODO: do not use tq_client.nc directly
                        await (self.tq_client.nc.publish(self.event_subject_notify,
                                                     bytes(message, encoding='utf-8'), headers=headers))
                    except:
                        logger.error(traceback.format_exc())
            elif action.action_type == SActionType.RPC_REQUEST:
                request: RPCRequest = action.action_data
                #strategy.register_rpc_callback(request_id=request.request_id, cb=request .resp_cb)
                async def task():
                    resp, is_error = await rpc(nc=self.tq_client.nc,
                                               subject=request.subject,
                                               method=request.method,
                                               payload=request.payload,
                                               timeout_in_secs=request.timeout_in_secs,
                                               retry_on_timeout=request.retry_on_timeout)

                    # since no plan to support rpc in backtesting, no need to send the response back to the queue
                    if request.resp_cb:
                        try:
                            request.resp_cb(resp, is_error)
                        except:
                            logger.error("error in calling rpc callback:" + traceback.format_exc())
                asyncio.create_task(task())



    async def _process_trading_actions(self, trading_actions: list[SAction]):
        orders: list[StrategyOrder] = [action.action_data for action in trading_actions if action.action_type == SActionType.ORDER]

        if len(orders) > 1:
            await self.tq_client.batch_send_orders(
                batched_orders=[
                    TQOrder(
                        symbol=order.symbol,
                        account_id=order.account_id,
                        order_id=order.order_id,
                        qty=order.qty,
                        price=order.price,
                        side=order.side,
                        extra_args=order.extra_params
                    ) for order in orders
                ])
        elif len(orders) == 1:
            strat_order = orders[0]
            order_id = await self.tq_client.send_order(
                symbol=strat_order.symbol,
                client_order_id=strat_order.order_id,
                account_id=strat_order.account_id,
                qty=strat_order.qty,
                price=strat_order.price,
                side=strat_order.side,
                extra_args=strat_order.extra_params)

        cancels: list[StrategyCancel] = [action.action_data for action in trading_actions if action.action_type == SActionType.CANCEL]
        if len(cancels) > 1:
            await self.tq_client.batch_cancel(
                cancels=[TQCancel(order_id=c.order_id,
                                  exch_order_id=c.exch_order_ref,
                                  account_id=c.account_id) for c in cancels]
            )
        elif len(cancels) == 1:
            await self.tq_client.cancel(cancels[0].order_id, account_id=cancels[0].account_id)

    def _build_log_event(self, strategy_log: StrategyLog) -> strat_pb.StrategyLogEvent:
        log_event = strat_pb.StrategyLogEvent()
        log_event.execution_id = self.execution_id
        log_event.strategy_id = self.strategy_config.strategy_name
        log_event.timestamp = int(strategy_log.ts_dt.timestamp() * 1000)
        log_event.log = strategy_log.logline
        log_event.log_type = str(strategy_log.loglevel.name)
        return log_event

    def _build_lifecycle_notification(self, strategy_status: strat_pb.StrategyStatusType, data:Union[str, dict]=None):
        lc_event = strat_pb.StrategyLifecycleNotifyEvent()
        lc_event.execution_id = self.execution_id if self.execution_id is not None else None
        lc_event.strategy_id = self.strategy_config.strategy_name if self.strategy_config.strategy_name is not None else ''
        lc_event.timestamp = int(datetime.now().timestamp() * 1000)
        lc_event.status = strategy_status
        if data:
            lc_event.payload_json = str(data)

        return lc_event

    def _build_order_event(self, strategy_order: StrategyOrder):
        order_event = strat_pb.StrategyOrderEvent().from_dict(strategy_order.__dict__)
        order_event.buy_sell = strategy_order.side # todo: make them the same name
        order_event.execution_id = self.execution_id
        order_event.strategy_id = self.strategy_config.strategy_name
        order_event.timestamp = int(datetime.now().timestamp() * 1000)
        return order_event

    def _build_cancel_event(self, strategy_cancel: StrategyCancel):
        cancel_event = strat_pb.StrategyCancelEvent()
        cancel_event.order_id = strategy_cancel.order_id
        cancel_event.exch_order_id = strategy_cancel.exch_order_ref
        cancel_event.execution_id = self.execution_id
        cancel_event.strategy_id = self.strategy_config.strategy_name
        cancel_event.timestamp = int(datetime.now().timestamp() * 1000)
        return cancel_event

    async def _publish_lifecycle_event(self, status: strat_pb.StrategyStatusType, data=None):
        try:
            lc_event = self._build_lifecycle_notification(
                strategy_status=status,
                data=data
            )
            await self.tq_client.publish(self.event_subject_lifecycle, lc_event)
        except:
            logger.error(traceback.format_exc())

    async def shutdown(self, signal_, loop):
        logger.info(f"Received {signal_.name} signal. Shutting down...")

        tasks = [t for t in asyncio.all_tasks() if t is not asyncio.current_task()]

        for task in tasks:
            task.cancel()

        with suppress(asyncio.CancelledError):
            await asyncio.gather(*tasks, return_exceptions=True)

        loop.stop()

    def run(self):

        loop = asyncio.get_event_loop()

        # Register signal handlers for graceful shutdown
        signals = (signal.SIGTERM, signal.SIGINT)
        for sig in signals:
            loop.add_signal_handler(sig, lambda s=sig: asyncio.create_task(self.shutdown(s, loop)))

        try:
            loop.run_until_complete(self.start())
            strategy_task = loop.create_task(self.run_strategy())
            timer_task = loop.create_task(self.run_clock())

            loop.run_until_complete(strategy_task)
            if strategy_task.exception():
                raise strategy_task.exception()

        except KeyboardInterrupt:
            logger.info(f"Interrupted by user.")
        finally:
            loop.close()

def _get_file_path(base_dir: str, file_name: str) -> str:
    import os
    return os.path.join(base_dir, file_name)
