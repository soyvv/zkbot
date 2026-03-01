import functools
import logging
from dataclasses import dataclass
from datetime import datetime, timedelta
from typing import Callable, Optional, Union
import copy
import betterproto as bp
import pandas as pd

from zk_datamodel.ods import OmsConfigEntry
from zk_datamodel.strategy import StrategyCommand
from zk_oms.core.confdata_mgr import ConfdataManager

from .timer import TimerManager
from .models import *
from .strategy_core import StrategyTemplate, StrategyConfig

import zk_simulator.sim_core as sim
from zk_simulator.sim_balance import SimGwBalanceMgr
from zk_oms.core.models import OMSAction, OMSActionType, GwConfigEntry, InstrumentRefdata, OMSRouteEntry, OMSPosition, \
    InstrumentTradingConfig
from zk_utils.instrument_utils import MARGIN_TYPES, infer_instrument_type_from_symbol
from zk_oms.core.oms_core import OMSCore

import zk_datamodel.rtmd as rtmd
import zk_datamodel.oms as oms
import zk_utils.zk_utils as utils

import zk_strategy.datasource as ds
from zk_simulator.sim_core import MatchPolicy

import heapq

import pprint

import progressbar


@dataclass
class BacktestConfig:
    start_dt: datetime = None
    end_dt: datetime = None
    symbols: Union[dict[str, str], list[InstrumentRefdata]] = None  # sym -> sym_exch or list of InstrumentRefdata
    account_ids: list[int] = None # each account_id is mapped to the same gw_key

    trading_config_overrides: dict[str, dict[str, any]] = None # symbol -> {property -> value}

    print_logs: bool = False

    init_balances: dict[int, dict[str, float]] = None # account_id -> symbol -> balance

    init_data_fetcher: Callable[[any, datetime], any] = None

    simulator_gw: sim.Simulator = None

    # required if simulator_gw is not configured
    match_policy: MatchPolicy = None
    match_policy_cls: type = None # ignored if match_policy is configured
    match_policy_config: dict = None # kwargs for object creation

    fund_asset: Optional[str] = None # e.g. "USD", "USDC"; required for SimGwBalanceMgr fund tracking

    # mostly required; for bar driven strategy, this is optional
    data_source: ds.MDDataSource = None
    data_source_cls: type = None  # ignored if data_source is configured
    data_source_config: dict = None # kwargs for object creation

    # optional
    signal_data_source: ds.SignalDataSource = None
    signal_data_source_class: type = None # ignored if signal_data_source is configured
    signal_data_source_config: dict = None # kwargs for object creation

    # optional
    kline_symbols: dict[str, str] = None # use symbols if not configured
    kline_data_source: ds.KlineDataSource = None
    kline_data_source_class: type = None # ignored if kline_data_source is configured
    kline_data_source_config: dict = None # kwargs for object creation
    include_kline_in_res: bool = False # include kline in the result for visualization/analysis



class ProgressEvent:
    pass

_type_ord = {
    rtmd.TickData: 10,
    rtmd.RealtimeSignal: 9,
    rtmd.Kline: 8,
    oms.PositionUpdateEvent: 7,
    oms.OrderUpdateEvent: 6,
    TimerEvent: 5,
    StrategyCommand: 4,
    ProgressEvent: 3,
}


class ProgressHandler:
    def __init__(self, start_ts:int, end_ts:int, cb: Callable[[int], None]=None):
        if cb:
            self.cb = cb
        else:
            _pb = progressbar.ProgressBar(max_value=100, redirect_stdout=True, redirect_stderr=True)
            def _progress_bar_cb(i):
                _pb.update(i)

            self.cb = _progress_bar_cb

        self.start_ts = start_ts
        self.end_ts = end_ts
        self.total_millis = end_ts - start_ts
        self.one_percent = self.total_millis / 100
        self._i = 0
        self._sched = [int(self.start_ts + self.one_percent * i) for i in range(101)]


    def update_to_next(self) -> int:
        self.cb(self._i)
        self._i += 1
        if self._i >= 101:
            return None
        else:
            return self._sched[self._i]






@functools.total_ordering
class RobEvent:
    def __init__(self, ts: int, obj: any):
        self.ts = ts
        self.obj = obj

    def __repr__(self):
        return "RobEvent(ts={}, obj={})".format(self.ts, self.obj)

    def __eq__(self, other):
        if not isinstance(other, RobEvent):
            raise TypeError("can only compare with RobEvent")
        else:
            return self.ts == other.ts and type(self.obj) == type(other.obj)

    def __lt__(self, other):
        if not isinstance(other, RobEvent):
            raise TypeError("can only compare with same type")
        else:
            if self.ts < other.ts:
                return True
            elif self.ts == other.ts:
                ord1 = _type_ord.get(type(self.obj), None)
                ord2 = _type_ord.get(type(other.obj), None)
                if ord1 is None or ord2 is None:
                    raise TypeError("unknown event type: " + str(type(self.obj)) + " or " + str(type(other.obj)))
                return ord1 < ord2
            else:
                return False



@dataclass
class BTOMSOutput:
    ts_ms: int = 0
    order_update: oms.OrderUpdateEvent = None
    position_update: oms.PositionUpdateEvent = None


class ReorderQueue:
    def __init__(self, progress_handler: ProgressHandler=None):
        self._q: list[RobEvent] = []
        self._progress_event = ProgressEvent()
        self.df = None
        self.df_idx = 0
        #self.tick_holder = rtmd.TickData()
        #self.reports_q: list[tuple[int, oms.OrderUpdateEvent]] = []

        self.df_signals = None
        self.df_sig_idx = 0

        self.df_bars = None
        self.df_bars_idx = 0
        self._sig_columns: list[str] = []

        if progress_handler:
            self.progress_handler = progress_handler
            self._update_progress()


    def insert_mkd(self, df: pd.DataFrame):
        if self.df is None:
            self.df = df
        else:
            self.df = pd.concat([self.df, df], axis=0)

        self._add_next_tick()

    def insert_sigdata(self, df: pd.DataFrame):
        if self.df_signals is None:
            self.df_signals = df
        else:
            self.df_signals = pd.concat([self.df_signals, df], axis=0)

        # todo: sort by ts
        self._sig_columns = [col for col in self.df_signals.columns
                             if col not in ("ts", "symbol", "timestamp")]
        if len(self._sig_columns) == 0:
            raise ValueError("at least one value column should be found in signal dataframes")

        self._add_next_signal()

    def insert_bar(self, df: pd.DataFrame):
        if self.df_bars is None:
            self.df_bars = df
        else:
            self.df_bars = pd.concat([self.df_bars, df], axis=0)

        self._add_next_bar()

    def insert_cmd_seq(self, cmd_seq: list[tuple[datetime, StrategyCommand]]):
        cmd_with_ts = [(utils.from_datetime_to_unix_ts(dt), cmd) for dt, cmd in cmd_seq]
        for item in cmd_with_ts:
            event = RobEvent(item[0], item[1])
            heapq.heappush(self._q, event)

    def batch_insert_timer_events(self, items: list[tuple[int, TimerEvent]]):
        for item in items:
            event = RobEvent(item[0], item[1])
            heapq.heappush(self._q, event)


    def batch_insert_orderupdates(self, items: list[tuple[int, oms.OrderUpdateEvent]]):
        for item in items:
            event = RobEvent(item[0], item[1])
            heapq.heappush(self._q, event)

    def batch_insert_positionupdates(self, items: list[tuple[int, oms.PositionUpdateEvent]]):
        for item in items:
            event = RobEvent(item[0], item[1])
            heapq.heappush(self._q, event)

    def batch_insert_bt_events(self, items: list[BTOMSOutput]):
        for item in items:
            if item.order_update:
                event = RobEvent(item.ts_ms, item.order_update)
                heapq.heappush(self._q, event)
            if item.position_update:
                event = RobEvent(item.ts_ms, item.position_update)
                heapq.heappush(self._q, event)

    def poll_next(self) -> tuple[int, any]:
        if self.progress_handler:
            while len(self._q) > 0 and self._q[0].obj == self._progress_event:
                self._update_progress()
                heapq.heappop(self._q)
        if len(self._q) > 0:
            return self._q[0].ts, self._q[0].obj
        else:
            return None

    def remove_next(self) -> tuple[int, any]:

        if len(self._q) > 0:
            event = self._q[0].obj
            if isinstance(event, rtmd.TickData):
                self._add_next_tick()
            elif isinstance(event, rtmd.RealtimeSignal):
                self._add_next_signal()
            elif isinstance(event, rtmd.Kline):
                self._add_next_bar()
            event = heapq.heappop(self._q)
            return event.ts, event.obj
        else:
            return None


    def has_next(self) -> bool:
        return len(self._q) > 0


    def _add_next_tick(self):
        if self.df_idx < len(self.df):
            tick = ds.convert_to_obj_ob(self.df, self.df_idx)
            ts = tick.original_timestamp


            self.df_idx += 1
            event = RobEvent(ts, tick)
            heapq.heappush(self._q, event)

    def _add_next_signal(self):
        if self.df_sig_idx < len(self.df_signals):

            signal = ds.convert_to_obj_signal(self.df_signals, self.df_sig_idx, self._sig_columns)
            ts = signal.timestamp
            self.df_sig_idx += 1
            event = RobEvent(ts, signal)
            heapq.heappush(self._q, event)

    def _add_next_bar(self):
        if self.df_bars_idx < len(self.df_bars):
            bar: rtmd.Kline = ds.convert_to_obj_kline(self.df_bars, self.df_bars_idx)
            ts = bar.timestamp
            self.df_bars_idx += 1
            event = RobEvent(ts, bar)
            heapq.heappush(self._q, event)


    def _update_progress(self):
        next_ts = self.progress_handler.update_to_next()
        if next_ts is not None:
            event = RobEvent(next_ts, self._progress_event)
            heapq.heappush(self._q, event)


class BacktestResultCollector:
    def __init__(self):
        self.orders: list[tuple[int, StrategyOrder]] = []
        self.trades: list[oms.Trade] = []
        self.logs: list[StrategyLog] = []
        self.generated_signals: list[rtmd.RealtimeSignal] = []
        self.klines: pd.DataFrame = None

    def add_klines(self, kline_df: pd.DataFrame):
        self.klines = kline_df

    def add_order_request(self, order: StrategyOrder, ts: int):
        self.orders.append((ts, order))

    def add_trade(self, trade: oms.Trade):
        self.trades.append(trade)

    def add_log(self, log: StrategyLog):
        self.logs.append(log)

    def add_signal(self, signal: rtmd.RealtimeSignal):
        self.generated_signals.append(signal)


    def get_orders(self) -> pd.DataFrame:
        if len(self.orders) == 0:
            return None
        order_items = [(ts, o.symbol, o.side.name, o.qty, o.price, o.account_id) for (ts,o) in self.orders]
        df = pd.DataFrame(order_items, columns=["ts", "symbol", "side", "qty", "price", "account"])
        df["ts"] = pd.to_datetime(df["ts"], unit="ms")
        return df

    def get_logs(self) -> pd.DataFrame:
        if len(self.logs) == 0:
            return None
        df = pd.DataFrame([(l.ts_dt, l.logline) for l in self.logs], columns=["ts", "log"])
        #df["ts"] = pd.to_datetime(df["ts"], unit="ms")
        return df

    def get_trades(self) -> pd.DataFrame:
        if len(self.trades) == 0:
            return None
        # todo: enrich the trades with order info
        df = pd.DataFrame([(t.filled_ts, t.instrument,
                            "buy" if t.buy_sell_type == common.BuySellType.BS_BUY
                            else "sell" if t.buy_sell_type == common.BuySellType.BS_SELL
                            else "unknown",
                            t.filled_price, t.filled_qty) for t in self.trades],
                          columns=["ts", "symbol", "side", "price", "qty"])

        return df

    def get_signals(self) -> pd.DataFrame:
        if len(self.generated_signals) == 0:
            return None
        data = [{**rts.data, **{"ts": rts.timestamp, "symbol": rts.instrument}} for rts in self.generated_signals]
        columns = list(data[0].keys())
        df = pd.DataFrame(data, columns=columns)
        return df

    def get_klines(self):
        if self.klines is None:
            return None
        else:
            # column rename to [ts, open, close, high, low, symbol, volume]
            _klines = self.klines.rename(columns={
                ds.BAR_COLS.timestamp.value: "ts",
                ds.BAR_COLS.open.value: "open",
                ds.BAR_COLS.close.value: "close",
                ds.BAR_COLS.high.value: "high",
                ds.BAR_COLS.low.value: "low",
                ds.BAR_COLS.symbol.value: "symbol",
                ds.BAR_COLS.volume.value: "volume"})

            return _klines

    def sprint_all(self) -> str:
        return pprint.pformat({"orders: ": self.get_orders(),
                    "trades: ": self.get_trades(),
                    "signals: ": self.get_signals(),
                    "logs: ": self.get_logs()}, indent=2)




class BacktestOMSV2:
    def __init__(self, account_ids: list[int], symbols: dict[str, str],
                 match_policy: sim.MatchPolicy = None,
                 simulator_gw: sim.Simulator = None,
                 init_balances: dict[int, dict[str, float]] = None,
                 trading_conf_overrides: dict[str, dict[str, any]] = None,
                 fund_asset: Optional[str] = None):
        exch = "SIM1"
        if simulator_gw:
            self.matcher: sim.Simulator = simulator_gw
        else:
            self.matcher:sim.Simulator = sim.SimulatorCore(
                match_policy=match_policy,
                exch_name=exch
            )
        refdata = self._infer_refdata(symbols)
        trading_config = self._infer_trading_config(refdata)

        if trading_conf_overrides:
            for sym, overrides in trading_conf_overrides.items():
                for k, v in overrides.items():
                    for conf in trading_config:
                        if conf.instrument_code == sym:
                            setattr(conf, k, v)
                            break


        gw_config_table = [
            GwConfigEntry(
                rpc_endpoint="dummy1",
                report_endpoint="dummy2",
                gw_key=exch,
                exch_name=exch,
                cancel_required_fields=["order_id"]
            )
        ]
        routing_table = [
            OMSRouteEntry(
                accound_id=acc_id,
                gw_key=exch,
                exch_account_id=str(acc_id)
            ) for acc_id in account_ids
        ]

        confdata_mgr = ConfdataManager(oms_id="bt_oms")
        confdata_mgr .reload_config(
            oms_config_entry=OmsConfigEntry(
                oms_id="bt_oms",
                managed_account_ids=account_ids,
                namespace="test"
            ),
            account_routes=routing_table,
            gw_configs=gw_config_table,
            refdata=refdata,
            trading_configs=trading_config
        )


        self.oms_core: OMSCore = OMSCore(
            confdata_mgr=confdata_mgr,
            use_time_emulation=True,
            logger_enabled=False
        )
        positions = self._create_init_positions(init_balances) if init_balances else []
        self.oms_core.init_state(curr_orders=[], curr_balances=positions)

        # Set up SimGwBalanceMgr for fund tracking (used by both spot and margin)
        self.sim_balance_mgr: Optional[SimGwBalanceMgr] = None
        if fund_asset and init_balances:
            # Flatten init_balances across accounts into tuples for first account
            first_account = account_ids[0]
            bal_tuples = [(sym, qty) for sym, qty in init_balances.get(first_account, {}).items()]
            self.sim_balance_mgr = SimGwBalanceMgr(
                init_balances=bal_tuples,
                fund_asset=fund_asset,
                exch_account_id=str(first_account),
                refdata=refdata
            )

    def _infer_refdata(self, symbols: Union[dict[str, str], list[InstrumentRefdata]]) -> list[InstrumentRefdata]:
        all_refdata = []
        if isinstance(symbols, list):
            all_refdata = symbols
        else:
            for sym, sym_exch in symbols.items():
                assert "@" in sym
                pair = sym.split("@")[0]
                assert "/" in pair
                base, quote = pair.split("/", maxsplit=2)
                #base: str

                sym_type = infer_instrument_type_from_symbol(base)

                _refdata = {
                    "instrument_id": sym,
                    "instrument_id_exchange": sym_exch,
                    "exchange_name": "SIM1",
                    "instrument_type": sym_type,
                    "quote_asset": quote,
                    "base_asset": base,
                    "price_precision": 8,
                    "qty_precision": 8,
                }


                refdata = InstrumentRefdata(**_refdata)
                all_refdata.append(refdata)
        return all_refdata

    def _infer_trading_config(self, refdata: list[InstrumentRefdata]) -> list[InstrumentTradingConfig]:
        all_trading_conf = []
        for ref in refdata:
            trading_conf = InstrumentTradingConfig(instrument_code=ref.instrument_id)
            trading_conf.bookkeeping_balance = True
            if ref.instrument_type in MARGIN_TYPES:
                trading_conf.use_margin = True
                trading_conf.margin_ratio = 0.1 # TODO: use config
            all_trading_conf.append(trading_conf)
        return all_trading_conf

    def _create_init_positions(self, init_balances: dict[int, dict[str, float]]) -> list[OMSPosition]:
        positions = []
        for acc_id, balances in init_balances.items():
            for item in balances.items():
                sym = item[0]
                qty = item[1]
                if len(item) >= 3:
                    inst_type = common.InstrumentType.from_string(item[2])
                else:
                    inst_type = common.InstrumentType.INST_TYPE_SPOT
                pos = OMSPosition.create_position(account_id=acc_id, symbol=sym, instrument_type=inst_type)
                pos.position_state.avail_qty = qty
                pos.position_state.total_qty = qty
                positions.append(pos)
        return positions

    def _process_sim_balance(self, order_reports: list, ts: int, results: list[BTOMSOutput]):
        """Feed order reports through SimGwBalanceMgr and inject balance updates into OMS."""
        if self.sim_balance_mgr and order_reports:
            balance_update = self.sim_balance_mgr.update_balance(order_reports)
            if balance_update:
                bal_actions = self.oms_core.process_balance_update(balance_update)
                for action in bal_actions:
                    if action.action_type == OMSActionType.UPDATE_BALANCE:
                        results.append(BTOMSOutput(ts_ms=ts + 3, position_update=action.action_data))

    def on_new_order(self, order: StrategyOrder, ts: int) -> list[BTOMSOutput]:
        oms_orderreq: oms.OrderRequest = self._convert_order_req(order, ts)
        oms_actions: list[OMSAction] = self.oms_core.process_order(oms_orderreq)
        results = []
        collected_reports = []
        for oms_action in oms_actions:
            if oms_action.action_type == OMSActionType.SEND_ORDER_TO_GW:
                sim_results = self.matcher.on_new_order(oms_action.action_data, ts)
                need_copy = len(sim_results) > 1
                for res in sim_results:
                    collected_reports.append(res.order_report)
                    more_oms_actions = self.oms_core.process_order_report(res.order_report)
                    for more_oms_action in more_oms_actions:
                        if more_oms_action.action_type == OMSActionType.UPDATE_ORDER:
                            ou = more_oms_action.action_data if not need_copy else copy.deepcopy(more_oms_action.action_data)
                            res = BTOMSOutput(ts_ms=ts + 2, order_update=ou)
                            results.append(res)
            elif oms_action.action_type == OMSActionType.UPDATE_ORDER:
                res = BTOMSOutput(ts_ms=ts + 1, order_update=oms_action.action_data)
                results.append(res)
            elif oms_action.action_type == OMSActionType.UPDATE_BALANCE:
                res = BTOMSOutput(ts_ms=ts + 1, position_update=oms_action.action_data)
                results.append(res)

        self._process_sim_balance(collected_reports, ts, results)
        return results


    def on_tick(self, tick: rtmd.TickData) -> list[BTOMSOutput]:
        sim_results = self.matcher.on_tick(tick)
        results = []
        ts = tick.original_timestamp
        collected_reports = []
        for res in sim_results:
            report = res.order_report
            collected_reports.append(report)
            oms_actions = self.oms_core.process_order_report(report)
            for action in oms_actions:
                if action.action_type == OMSActionType.UPDATE_ORDER:
                    res = BTOMSOutput(ts_ms=ts + 1, order_update=action.action_data)
                    results.append(res)
                elif action.action_type == OMSActionType.UPDATE_BALANCE:
                    res = BTOMSOutput(ts_ms=ts + 1, position_update=action.action_data)
                    results.append(res)
        self._process_sim_balance(collected_reports, ts, results)
        return results

    def on_cancel(self, cancel: StrategyCancel, ts: int) -> list[BTOMSOutput]:
        oms_ordercancel: oms.OrderCancelRequest = self._convert_order_cancel_req(cancel, ts)
        oms_actions: list[OMSAction] = self.oms_core.process_cancel(oms_ordercancel)
        results = []
        collected_reports = []
        for oms_action in oms_actions:
            if oms_action.action_type == OMSActionType.SEND_CANCEL_TO_GW:
                sim_results = self.matcher.on_cancel_order(oms_action.action_data, ts)
                for res in sim_results:
                    collected_reports.append(res.order_report)
                    more_oms_actions = self.oms_core.process_order_report(res.order_report)
                    for more_oms_action in more_oms_actions:
                        if more_oms_action.action_type == OMSActionType.UPDATE_ORDER:
                            res = BTOMSOutput(ts_ms=ts + 2, order_update=more_oms_action.action_data)
                            results.append(res)
            elif oms_action.action_type == OMSActionType.UPDATE_ORDER:
                res = BTOMSOutput(ts_ms=ts + 1, order_update=oms_action.action_data)
                results.append(res)
            elif oms_action.action_type == OMSActionType.UPDATE_BALANCE:
                res = BTOMSOutput(ts_ms=ts + 1, position_update=oms_action.action_data)
                results.append(res)
        self._process_sim_balance(collected_reports, ts, results)

        return results

    def _convert_order_req(self, order: StrategyOrder, ts: int) -> oms.OrderRequest:
        oms_order = oms.OrderRequest()
        oms_order.order_id = order.order_id
        oms_order.order_type = common.BasicOrderType.ORDERTYPE_LIMIT
        oms_order.price = order.price
        oms_order.qty = order.qty
        oms_order.instrument_code = order.symbol
        oms_order.account_id = order.account_id
        oms_order.buy_sell_type = order.side
        oms_order.open_close_type = common.OpenCloseType.OC_OPEN
        oms_order.timestamp = ts
        oms_order.time_inforce_type = common.TimeInForceType.TIMEINFORCE_GTC

        if order.extra_params:
            oms_order.extra_properties = common.ExtraData()
            oms_order.extra_properties.data_map = order.extra_params
        return oms_order

    def _convert_order_cancel_req(self, cancel: StrategyCancel, ts: int) -> oms.OrderCancelRequest:
        oms_cancel = oms.OrderCancelRequest()
        oms_cancel.order_id = cancel.order_id
        oms_cancel.timestamp = ts
        return oms_cancel



class StrategyBacktestor:

    def __init__(self, strategy_config: StrategyConfig,
                 backtest_config: BacktestConfig,
                 cmd_sequence: list[tuple[datetime, StrategyCommand]]=[],
                 progress_callback: Callable[[int], None]=None):
        self.strategy_config = strategy_config
        self.backtest_config = backtest_config
        self.strategy: StrategyTemplate = StrategyTemplate(
            strategy_config=self.strategy_config)

        self._setup_om_and_matcher()
        self._setup_data_source()
        self._setup_signal_data_source()
        self._setup_kline_data_source()

        progress_handler = ProgressHandler(
            start_ts=utils.from_datetime_to_unix_ts(backtest_config.start_dt),
            end_ts=utils.from_datetime_to_unix_ts(backtest_config.end_dt),
            cb=progress_callback)

        self.rob = ReorderQueue(progress_handler=progress_handler)
        self.timer: TimerManager = None
        self.cmd_seq: list[tuple[datetime, StrategyCommand]] = cmd_sequence

        self.result_collector = BacktestResultCollector()

    def _setup_om_and_matcher(self):
        bt_config = self.backtest_config
        account_ids = bt_config.account_ids if bt_config.account_ids else [123]
        if bt_config.simulator_gw:
            oms = BacktestOMSV2(account_ids=account_ids,
                                symbols=self.backtest_config.symbols,
                                simulator_gw=bt_config.simulator_gw,
                                init_balances=bt_config.init_balances,
                                trading_conf_overrides=bt_config.trading_config_overrides,
                                fund_asset=bt_config.fund_asset)
        else:
            match_policy = None
            if bt_config.match_policy:
                match_policy = bt_config.match_policy
            elif bt_config.match_policy_cls:
                kwargs = bt_config.match_policy_config if bt_config.match_policy_config else {}
                match_policy = self.backtest_config.match_policy_cls(**kwargs)
            else:
                raise ValueError("match policy not configured")

            oms = BacktestOMSV2(account_ids=account_ids,
                                symbols=self.backtest_config.symbols,
                                init_balances=bt_config.init_balances,
                                match_policy=match_policy,
                                trading_conf_overrides=bt_config.trading_config_overrides,
                                fund_asset=bt_config.fund_asset)

        self.oms = oms

    def _setup_data_source(self):
        bt_config = self.backtest_config
        ds = None
        if bt_config.data_source:
            ds = bt_config.data_source
        elif bt_config.data_source_cls:
            kwargs = bt_config.data_source_config if bt_config.data_source_config else {}
            ds = bt_config.data_source_cls(**kwargs)
        else:
            if self.oms.matcher.match_policy.need_tick():
                raise ValueError("data source not configured")
        self.data_source = ds

    def _setup_signal_data_source(self):
        bt_config = self.backtest_config
        ds = None
        if bt_config.signal_data_source:
            ds = bt_config.signal_data_source
        elif bt_config.signal_data_source_class:
            kwargs = bt_config.signal_data_source_config if bt_config.signal_data_source_config else {}
            ds = bt_config.signal_data_source_class(**kwargs)

        self.signal_data_source = ds

    def _setup_kline_data_source(self):
        bt_config = self.backtest_config
        ds = None
        if bt_config.kline_data_source:
            ds = bt_config.kline_data_source
        elif bt_config.kline_data_source_class:
            kwargs = bt_config.kline_data_source_config if bt_config.kline_data_source_config else {}
            ds = bt_config.kline_data_source_class(**kwargs)

        self.kline_data_source = ds


    def _check_error(self):
        if self.strategy.is_in_error_state():
            for err in self.strategy.get_errors():
                print(err)
            print(f"{len(self.strategy.get_errors())} error(s) detected during backtest; aborting")
            return True
        return False
            #raise Exception(f"{len(self.strategy.get_errors())} error(s) detected during backtest; aborting")

    def run(self):
        # 1. init matcher, timer and strategyTemplate

        # 2. load hist data from datasource
        start_dt = self.backtest_config.start_dt
        end_dt = self.backtest_config.end_dt
        if self.data_source:
            symbols = self.backtest_config.symbols
            hist_data = self.data_source.get_data_multiple_symbols(list(symbols.values()), start_dt, end_dt)
            self.rob.insert_mkd(hist_data)

        bt_start_ts = self.backtest_config.start_dt

        self.timer = TimerManager(init_ts=bt_start_ts)

        # 2.1 load signal hist data if configured
        if self.signal_data_source:
            signal_hist_data = self.signal_data_source.get_data(start_dt, end_dt)
            self.rob.insert_sigdata(signal_hist_data)

        # 2.2 load kline hist data if configured
        if self.kline_data_source:
            kline_symbols = self.backtest_config.kline_symbols \
                if self.backtest_config.kline_symbols \
                else self.backtest_config.symbols
            kline_hist_data = self.kline_data_source.get_data_multiple_symbols(
                symbols=list(kline_symbols.values()),
                start_dt=start_dt, end_dt=end_dt)
            self.rob.insert_bar(kline_hist_data)

            if self.backtest_config.include_kline_in_res:
                self.result_collector.add_klines(kline_hist_data)

        if self.cmd_seq:
            self.rob.insert_cmd_seq(self.cmd_seq)

        # # 3. init rob
        # if hist_data:

        bt_ts = utils.from_datetime_to_unix_ts(bt_start_ts)
        last_ts = None
        last_item = None

        # 4. create and init strategy
        create_actions = self.strategy.create_strategy(create_ts=bt_start_ts)
        self._handle_actions(create_actions, ts=bt_ts)
        if self.backtest_config.init_balances:
            init_balances: list[oms.Position] = self._create_init_balances(self.backtest_config.init_balances, bt_ts)
            self.strategy.tq.state_proxy.init_orders_and_balances(init_orders=[], init_balances=init_balances)
        if self.backtest_config.init_data_fetcher:
            # no tq_client is given in backtesting
            init_data = self.backtest_config.init_data_fetcher(self.strategy_config.custom_params,
                                                               utils.from_unix_ts_to_datetime(bt_ts), None)
            self.strategy.tq.__tq_init_output__ = init_data

        init_actions = self.strategy.init_strategy(custom_config=self.strategy_config.custom_params,
                                                   update_ts=bt_start_ts)

        if self._check_error():

            return

        self._handle_actions(init_actions, ts=bt_ts)

        while self.rob.has_next():
            error_found = self._check_error()
            if error_found:
                break
            next_event: tuple[int, any] = self.rob.poll_next()

            if next_event is None:
                # when polling from rob, there's maybe some progress event left.
                # TODO: better fix this
                break

            # todo: only handle events between daily_start_ts and daily_end_ts

            ts = next_event[0]
            item = next_event[1]
            # if not isinstance(item, TimerEvent):
            #     self._update_timer(item, ts)
            self._update_timer(item, ts)



            (ts, item) = self.rob.remove_next()
            if ts < bt_ts:
                print((ts, bt_ts, item, last_item))
                raise Exception("ts should be larger than bt_ts")
            last_ts, last_item = ts, item
            bt_ts = ts
            self.strategy.update_time(bt_ts)

            if isinstance(item, rtmd.TickData):
                reports = self.oms.on_tick(tick=item)
                self._handle_sim_results(reports)

                actions = self.strategy.process_tick_event(item)
                self._handle_actions(actions, ts=bt_ts)

            elif isinstance(item, rtmd.RealtimeSignal):
                actions = self.strategy.process_signal_event(item)
                self._handle_actions(actions, ts=bt_ts)

            elif isinstance(item, TimerEvent):
                actions = self.strategy.process_timer_event(item)
                self._handle_actions(actions, ts=bt_ts)

            elif isinstance(item, oms.OrderUpdateEvent):

                actions = self.strategy.process_order_update_event(item)
                self._handle_actions(actions, ts=bt_ts)

            elif isinstance(item, oms.PositionUpdateEvent):
                actions = self.strategy.process_position_update_event(item)
                self._handle_actions(actions, ts=bt_ts)

            elif isinstance(item, rtmd.Kline):
                actions = self.strategy.process_bar_event(item)
                self._handle_actions(actions, ts=bt_ts)

            elif isinstance(item, StrategyCommand):
                actions = self.strategy.process_strategy_cmd(item)
                self._handle_actions(actions, ts=bt_ts)
            else:
                raise Exception(f"unknown event type for {item}")


    def get_result(self):
        return self.result_collector

    def _update_timer(self, item: any, ts: int):
        dt = utils.from_unix_ts_to_datetime(ts)
        timer_events = self.timer.generate_next_batch_events(instant_ts=dt)
        if timer_events:
            timer_events_with_ts = [(utils.from_datetime_to_unix_ts(te.current_dt), te)
                                    for te in timer_events]
            self.rob.batch_insert_timer_events(timer_events_with_ts)



    def _handle_sim_results(self, sim_results: list[BTOMSOutput]):
        if sim_results:
            self.rob.batch_insert_bt_events(sim_results)
            for res in sim_results:
                if res.order_update:
                    oue = res.order_update
                    if oue.last_trade and bp.serialized_on_wire(oue.last_trade):
                        self.result_collector.add_trade(oue.last_trade)


    def _handle_actions(self, actions: list[SAction], ts: int):
        if not actions:
            return
        for action in actions:
            action_type = action.action_type
            if action_type == SActionType.ORDER:
                sim_results = self.oms.on_new_order(action.action_data, ts)
                self._handle_sim_results(sim_results)
                self.result_collector.add_order_request(action.action_data, ts)
            elif action_type == SActionType.CANCEL:
                sim_results = self.oms.on_cancel(action.action_data, ts)
                self._handle_sim_results(sim_results)
            elif action_type == SActionType.LOG:
                self.result_collector.add_log(action.action_data)
                if self.backtest_config.print_logs:
                    print(action.action_data.logline)
            elif action_type == SActionType.SIGNAL:
                self.result_collector.add_signal(action.action_data)
            elif action_type == SActionType.TIMER_SUB:
                timer_sub: TimerSubscription = action.action_data
                if timer_sub.scheduled_clock:
                    self.timer.subscribe_timer_clock(timer_key=timer_sub.timer_name,
                                                     date_time=timer_sub.scheduled_clock)
                elif timer_sub.cron_expr:
                    self.timer.subscribe_timer_cron(timer_key=timer_sub.timer_name,
                                                    cron_expression=timer_sub.cron_expr,
                                                    start_ts=timer_sub.cron_start_ts,
                                                    end_ts=timer_sub.cron_end_ts)

                self.strategy.register_timer(timer_key=timer_sub.timer_name,
                                             cb=timer_sub.cb,
                                             cb_kwargs=timer_sub.cb_extra_kwarg)
            else:
                raise Exception("unknown or unsupported action type: " + str(action_type))

    def _create_init_balances(self, init_balances: dict[int, dict[str, float]], ts: int) -> list[oms.Position]:
        init_balances_list = []
        for account_id, balance in init_balances.items():
            for symbol, qty in balance.items():
                pos = oms.Position()
                pos.instrument_code = symbol
                pos.account_id = account_id
                pos.avail_qty = qty
                pos.frozen_qty = .0
                pos.total_qty = qty
                pos.is_from_exch = False
                pos.update_timestamp = ts
                pos.long_short_type = common.LongShortType.LS_LONG
                init_balances_list.append(pos)
        return init_balances_list



