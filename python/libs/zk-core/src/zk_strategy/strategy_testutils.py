
from zk_strategy.backtester import StrategyBacktestor, BacktestConfig
import zk_strategy.datasource as ds
from zk_strategy.datasource import COLS
from zk_strategy.strategy_core import StrategyConfig
from datetime import datetime
import pandas as pd
import zk_proto_betterproto.rtmd as rtmd
import zk_proto_betterproto.exch_gw as gw 
import zk_proto_betterproto.tqrpc_exch_gw as gw_rpc
import zk_proto_betterproto.oms as oms
import zk_simulator.sim_core as sim
import enum
from typing import Optional, Union, Protocol
from dataclasses import dataclass
import zk_oms.tests.gw_test_utils as gw_test_utils

import zk_proto_betterproto.common as common

class TestTickDataDatasource(ds.MDDataSource):
    def __init__(self, tickdata: list[rtmd.TickData]):
        self._tickdata = tickdata
        self._start_dt: datetime = None
        self._end_dt: datetime = None

    def get_data(self, symbol: str, start_dt: datetime, end_dt: datetime) -> pd.DataFrame:
        # convert list of tickdata to dataframe
        flattened_tickdata = []
        for tick in self._tickdata:
            tick_dict = {}
            # try map the tick with ds.COLS
            tick_dict[ds.COLS.timestamp.value] = tick.original_timestamp
            tick_dict[ds.COLS.symbol.value] = tick.instrument_code
            tick_dict[ds.COLS.tick_type.value] = tick.tick_type.name
            #tick_dict[ds.COLS.exchange.value] = tick.exchange
            tick_dict[ds.COLS.b1.value] = tick.buy_price_levels[0].price if len(tick.buy_price_levels) >= 1 else None
            tick_dict[ds.COLS.b2.value] = tick.buy_price_levels[1].price if len(tick.buy_price_levels) >= 2 else None
            tick_dict[ds.COLS.b3.value] = tick.buy_price_levels[2].price if len(tick.buy_price_levels) >= 3 else None
            tick_dict[ds.COLS.b4.value] = tick.buy_price_levels[3].price if len(tick.buy_price_levels) >= 4 else None
            tick_dict[ds.COLS.b5.value] = tick.buy_price_levels[4].price if len(tick.buy_price_levels) >= 5 else None

            tick_dict[ds.COLS.bv1.value] = tick.buy_price_levels[0].qty if len(tick.buy_price_levels) >= 1 else None
            tick_dict[ds.COLS.bv2.value] = tick.buy_price_levels[1].qty if len(tick.buy_price_levels) >= 2 else None
            tick_dict[ds.COLS.bv3.value] = tick.buy_price_levels[2].qty if len(tick.buy_price_levels) >= 3 else None
            tick_dict[ds.COLS.bv4.value] = tick.buy_price_levels[3].qty if len(tick.buy_price_levels) >= 4 else None
            tick_dict[ds.COLS.bv5.value] = tick.buy_price_levels[4].qty if len(tick.buy_price_levels) >= 5 else None

            tick_dict[ds.COLS.s1.value] = tick.sell_price_levels[0].price if len(tick.sell_price_levels) >= 1 else None
            tick_dict[ds.COLS.s2.value] = tick.sell_price_levels[1].price if len(tick.sell_price_levels) >= 2 else None
            tick_dict[ds.COLS.s3.value] = tick.sell_price_levels[2].price if len(tick.sell_price_levels) >= 3 else None
            tick_dict[ds.COLS.s4.value] = tick.sell_price_levels[3].price if len(tick.sell_price_levels) >= 4 else None
            tick_dict[ds.COLS.s5.value] = tick.sell_price_levels[4].price if len(tick.sell_price_levels) >= 5 else None

            tick_dict[ds.COLS.sv1.value] = tick.sell_price_levels[0].qty if len(tick.sell_price_levels) >= 1 else None
            tick_dict[ds.COLS.sv2.value] = tick.sell_price_levels[1].qty if len(tick.sell_price_levels) >= 2 else None    
            tick_dict[ds.COLS.sv3.value] = tick.sell_price_levels[2].qty if len(tick.sell_price_levels) >= 3 else None
            tick_dict[ds.COLS.sv4.value] = tick.sell_price_levels[3].qty if len(tick.sell_price_levels) >= 4 else None
            tick_dict[ds.COLS.sv5.value] = tick.sell_price_levels[4].qty if len(tick.sell_price_levels) >= 5 else None

            tick_dict[ds.COLS.trade_price.value] = tick.latest_trade_price
            tick_dict[ds.COLS.trade_qty.value] = tick.latest_trade_qty
            tick_dict[ds.COLS.trade_direction.value] = tick.latest_trade_side
            flattened_tickdata.append(tick_dict)

        df = pd.DataFrame(flattened_tickdata)

        return df


class TestMDDataSource(ds.MDDataSource):
    
    def __init__(self, data: list[dict[str, any]]) -> None:
        self.data = data

        self.symbol_data = {}
        
        self.symbol_df = {}

        for tick in self.data:
            symbol = tick[COLS.symbol.value]
            if symbol not in self.symbol_data:
                self.symbol_data[symbol] = []
            self.symbol_data[symbol].append(tick)

        for symbol, data in self.symbol_data.items():
            df = pd.DataFrame(data)
            self.symbol_df[symbol] = df


    def get_data(self, symbol: str, start_dt: datetime, end_dt: datetime) -> pd.DataFrame:
        if symbol not in self.symbol_df:
            return None
        df = self.symbol_df[symbol]
        return df



class TestMatchType(enum.Enum):
    DIRECT_MATCH = "DIRECT_MATCH"
    DELAY_MATCH = "DELAY_MATCH"
    NO_MATCH = "NO_MATCH"
    IOC_CANCEL = "IOC_CANCEL"
    IOC_PARTIAL_FILL = "IOC_PARTIAL_FILL"
    RECOVERABLE_REJECT = "SOFT_REJECT"
    REJECT = "REJECT"



class TestSim(sim.Simulator):

    def __init__(self, match_sched: list[Union[TestMatchType, tuple[TestMatchType, any]]]) -> None:
        self.match_sched = match_sched or []
        self.already_dealt = set()
        self.orders = {}
        self.id_mapping = {}

        self._id_counter = 100
        self._gw_test_helper = gw_test_utils.GwMessageHelper(exch_account_id="test", gw_key="SIM1")

    def on_new_order(self, order: gw_rpc.ExchSendOrderRequest, ts: int) -> list[sim.SimResult]:
        self.orders[order.correlation_id] = order
        if self.match_sched is None or len(self.match_sched) == 0:
            raise ValueError("match schedule is empty or already drained")
        
        id = str(self._gen_next_id())
        self.id_mapping[order.correlation_id] = id
        match = self.match_sched.pop(0)
        if isinstance(match, tuple):
            match_type, match_data = match
            if match_type == TestMatchType.DELAY_MATCH:
                delay = match_data
                rep1 = self._gen_order_booked(order, exch_order_id=id, ts=ts+1)
                rep2 = self._gen_order_filled(order, exch_order_id=id, ts=ts+1)
                sim1 = sim.SimResult(delay_in_millis=1, order_report=rep1)
                sim2 = sim.SimResult(delay_in_millis=delay, order_report=rep2)
                self.already_dealt.add(order.correlation_id)
                return [sim1, sim2]
            elif match_type == TestMatchType.REJECT:
                rej_reason = match_data
                rep1 = self._gen_order_rejected(order, exch_order_id=id, reason=rej_reason)
                sim1 = sim.SimResult(delay_in_millis=1, order_report=rep1)
                self.already_dealt.add(order.correlation_id)
                return [sim1]
            elif match_type == TestMatchType.RECOVERABLE_REJECT:
                rej_reason = match_data
                rep1 = self._gen_order_rejected(order, rej_reason)
                sim1 = sim.SimResult(delay_in_millis=1, order_report=rep1)
                self.already_dealt.add(order.correlation_id)
                return [sim1]
            elif match_type == TestMatchType.IOC_PARTIAL_FILL:
                filled_qty = match_data
                rep1 = self._gen_order_booked(order, exch_order_id=id, ts=ts+1)
                rep2 = self._gen_order_cancelled(exch_order_ref=id, order_req=order, ts=ts+2, filled_qty=filled_qty)
                sim1 = sim.SimResult(delay_in_millis=1, order_report=rep1)
                sim2 = sim.SimResult(delay_in_millis=5, order_report=rep2)
                self.already_dealt.add(order.correlation_id)
                return [sim1, sim2]
            
        else:
            if match == TestMatchType.DIRECT_MATCH:
                rep1 = self._gen_order_booked(order, exch_order_id=id, ts=ts+1)
                rep2 = self._gen_order_filled(order, exch_order_id=id, ts=ts+1)
                sim1 = sim.SimResult(delay_in_millis=1, order_report=rep1)
                sim2 = sim.SimResult(delay_in_millis=5, order_report=rep2)
                self.already_dealt.add(order.correlation_id)
                return [sim1, sim2]
            elif match == TestMatchType.NO_MATCH:
                rep1 = self._gen_order_booked(order, exch_order_id=id, ts=ts+1)
                sim1 = sim.SimResult(delay_in_millis=1, order_report=rep1)
                return [sim1]
            elif match == TestMatchType.IOC_CANCEL:
                rep1 = self._gen_order_booked(order, exch_order_id=id, ts=ts+1)
                cancel_rep = self._gen_order_cancelled(id, order, ts+2)
                self.already_dealt.add(order.correlation_id)
                sim1 = sim.SimResult(delay_in_millis=1, order_report=rep1)
                sim2 = sim.SimResult(delay_in_millis=5, order_report=cancel_rep)
                return [sim1, sim2]
            else:
                raise ValueError(f"invalid match type: {match}")

    def on_tick(self, tick: rtmd.TickData) -> list[sim.SimResult]:
        return []

    def on_cancel_order(self, cancel_req: gw_rpc.ExchCancelOrderRequest, ts: int) -> list[sim.SimResult]:
        exch_order_id = cancel_req.exch_order_ref
        order_id = cancel_req.order_id
        order_req = self.orders[order_id]
        if order_id in self.already_dealt:
            rep = self._gen_cancel_rejected(cancel_req)
            return [sim.SimResult(delay_in_millis=1, order_report=rep)]
        else:
            rep = self._gen_order_cancelled(exch_order_id, order_req, ts)
            self.already_dealt.add(order_id)
            return [sim.SimResult(delay_in_millis=1, order_report=rep)] 

    def _gen_next_id(self):
        self._id_counter += 1
        return self._id_counter

    def _gen_order_booked(self, order_req: gw_rpc.ExchSendOrderRequest, exch_order_id:str, ts:int):
        linkage_report = self._gw_test_helper.generate_linkage_report(
            exch_order_ref=exch_order_id,
            client_order_id=order_req.correlation_id,
            ts=ts
        ) 
        return linkage_report

    def _gen_order_filled(self, order_req: gw_rpc.ExchSendOrderRequest, exch_order_id:str, ts: int):
        fill_report = self._gw_test_helper.generate_trade_report(
            order_id=order_req.correlation_id,
            exch_order_ref=exch_order_id,
            filled_price=order_req.scaled_price,
            filled_qty=order_req.scaled_qty,
            ts=ts
        )
        # Add order_info to trade report entries (needed by SimGwBalanceMgr)
        for entry in fill_report.order_report_entries:
            if entry.report_type == gw.OrderReportType.ORDER_REP_TYPE_TRADE:
                order_info = gw.OrderInfo()
                order_info.instrument = order_req.instrument
                order_info.buy_sell_type = order_req.buysell_type
                entry.trade_report.order_info = order_info

        state_report = self._gw_test_helper.generate_orderstate_report(
            state=gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_FILLED,
            order_id=order_req.correlation_id,
            exch_order_ref=exch_order_id,
            qty=order_req.scaled_qty,
            filled_qty=order_req.scaled_qty,
            unfilled_qty=.0,
            ts=ts,
            gw_order_report=fill_report
        )
        return state_report

    def _gen_order_cancelled(self, exch_order_ref: str,
                             order_req: gw_rpc.ExchSendOrderRequest, ts: int,
                             filled_qty=.0):

        if filled_qty > .0:
            fill_report = self._gw_test_helper.generate_trade_report(
                order_id=order_req.correlation_id,
                exch_order_ref=exch_order_ref,
                filled_price=order_req.scaled_price,
                filled_qty=filled_qty,
                ts=ts
            )
            # Add order_info to trade report entries (needed by SimGwBalanceMgr)
            for entry in fill_report.order_report_entries:
                if entry.report_type == gw.OrderReportType.ORDER_REP_TYPE_TRADE:
                    order_info = gw.OrderInfo()
                    order_info.instrument = order_req.instrument
                    order_info.buy_sell_type = order_req.buysell_type
                    entry.trade_report.order_info = order_info
        else:
            fill_report = None
        cancel_report = self._gw_test_helper.generate_orderstate_report(
            state=gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_CANCELLED,
            order_id=order_req.correlation_id,
            exch_order_ref=exch_order_ref,
            qty=order_req.scaled_qty,
            filled_qty=filled_qty,
            unfilled_qty=order_req.scaled_qty,
            ts=ts,
            gw_order_report=fill_report
        )

        return cancel_report


    def _gen_order_rejected(self, order_req: gw_rpc.ExchSendOrderRequest,
                            exch_order_id: str, reason: str):
        rej_report = self._gw_test_helper.generate_order_error_report_with_details(
            exch_order_ref=exch_order_id,
            order_req=order_req,
            error_code=-1,
            error_msg=reason,
            error_recoverable=False,
            include_order_state=True
        )
        return rej_report

    def _gen_cancel_rejected(self, cancel_req: gw_rpc.ExchCancelOrderRequest):
        rej_report = self._gw_test_helper.generate_cancel_reject_report(
            order_id=cancel_req.order_id,
            ts=cancel_req.timestamp,
            reason="already matched"
        )
        return rej_report
    

class TestSimBuilder:

    def __init__(self) -> None:
        self.match_sched: list[Union[TestMatchType, tuple[TestMatchType, any]]] = []
        
    def reject_next_order(self):
        self.match_sched.append((TestMatchType.REJECT, "reject"))
        return self

    def soft_reject_next_order(self):
        self.match_sched.append((TestMatchType.RECOVERABLE_REJECT, "soft reject"))
        return self
    
    def match_next_immediately(self):
        self.match_sched.append(TestMatchType.DIRECT_MATCH)
        return self

    def no_match_next(self):
        self.match_sched.append(TestMatchType.NO_MATCH)
        return self
    
    def match_next_with_delay(self, delay: int):
        self.match_sched.append((TestMatchType.DELAY_MATCH, delay))
        return self

    def ioc_cancel_next(self):
        self.match_sched.append(TestMatchType.IOC_CANCEL)
        return self

    def ioc_partial_fill_next(self, filled_qty: float):
        self.match_sched.append((TestMatchType.IOC_PARTIAL_FILL, filled_qty))
        return self
    
    def build(self) -> TestSim:
        return TestSim(self.match_sched)

@dataclass
class TestEventSequence:
    ticks: list[dict] = None
    position_updates: list[oms.PositionUpdateEvent] = None
    order_trades: list[oms.OrderUpdateEvent] = None
    start_dt: datetime = None
    end_dt: datetime = None

class TestEventSequenceBuilder:

    def __init__(self) -> None:
        self.test_seq = TestEventSequence(ticks=[], position_updates=[], order_trades=[])
        self._curr_ts: int = None  # virtual timestamp during building

    def start_at(self, start_dt: datetime) -> "TestEventSequenceBuilder":
        self._curr_ts = int(start_dt.timestamp() * 1000)
        self.test_seq.start_dt = start_dt
        return self 
    
    def end_at(self, end_dt: datetime) -> "TestEventSequenceBuilder":
        self.test_seq.end_dt = end_dt
        return self

    def add_tickdata_ob(self, symbol: str, b1: float, s1: float,  **kwargs) \
        -> "TestEventSequenceBuilder":

        tick_data = {
            COLS.symbol.value:  symbol,
            COLS.timestamp.value: self._curr_ts,
            COLS.tick_type.value: "OB",
            COLS.b1.value: b1,
            COLS.bv1.value: 10.0,
            COLS.s1.value: s1,
            COLS.sv1.value: 10.0,

            COLS.b2.value: None,
            COLS.bv2.value: .0,
            COLS.s2.value: None,
            COLS.sv2.value: .0,

            COLS.b3.value: None,
            COLS.bv3.value: .0,
            COLS.s3.value: None,
            COLS.sv3.value: .0,

            COLS.b4.value: None,
            COLS.bv4.value: .0,
            COLS.s4.value: None,
            COLS.sv4.value: .0,

            COLS.b5.value: None,
            COLS.bv5.value: .0,
            COLS.s5.value: None,
            COLS.sv5.value: .0
        }

        tick_data.update(kwargs)
        
        self.test_seq.ticks.append(tick_data)
        return self 

    def add_order_trade(self, account_id: int, symbol: str, 
                        filled_qty:float, filled_price:float, is_sell:bool, **kwargs) \
        -> "TestEventSequenceBuilder":
        order_update = oms.OrderUpdateEvent()
        order_update.account_id = account_id
        order_update.timestamp = self._curr_ts
        trade_dict = {
            "account_id": account_id,
            "filled_ts": self._curr_ts,
            "instrument": symbol,
            "filled_qty": filled_qty,
            "filled_price": filled_price,
            "buy_sell_type": common.BuySellType.BS_SELL if is_sell else common.BuySellType.BS_BUY,
        }
        order_update.last_trade = oms.Trade().from_dict(trade_dict)
        self.test_seq.order_trades.append(order_update) 
        return self
        

    def add_position_update(self, account_id: int, symbol: str, qty: float, **kwargs) \
        -> "TestEventSequenceBuilder":
        position_update = oms.PositionUpdateEvent()
        position_update.account_id = account_id
        position_update.position_snapshots = []
        pos = oms.Position()
        pos.account_id = account_id
        pos.instrument_code = symbol
        pos.avail_qty = abs(qty)
        pos.total_qty = abs(qty)
        pos.long_short_type = common.LongShortType.LS_SHORT if qty < 0 else common.LongShortType.LS_LONG
        pos.update_timestamp = self._curr_ts
        pos.sync_timestamp = self._curr_ts

        position_update.position_snapshots.append(pos)

        position_update.timestamp = self._curr_ts

        self.test_seq.position_updates.append(position_update)

        return self

    def clock_forward_millis(self, millis: int) -> "TestEventSequenceBuilder":
        if self._curr_ts is None:
            raise ValueError("Must call start_at() first")
        self._curr_ts += millis
        return self

    def clock_forward_seconds(self, seconds: int) -> "TestEventSequenceBuilder":
        return self.clock_forward_millis(seconds * 1000)


    def build(self) -> TestEventSequence:
        if self.test_seq.end_dt is None:
            end_dt = self._curr_ts + 10 
            end_dt_datetime = datetime.fromtimestamp(end_dt / 1000)
            self.test_seq.end_dt = end_dt_datetime

        # start_dt must be earlier than end_dt
        if self.test_seq.start_dt >= self.test_seq.end_dt:
            raise ValueError("start_dt must be earlier than end_dt")
        return self.test_seq


class StrategyTestBuilder:

    def __init__(self) -> None:
        self.sim: TestSim = None
        self.event_seq: TestEventSequence = None
        self.account_ids: list[int] = None
        self.symbols: dict[str, str] = None
        self.init_balances: dict[int, dict[str, float]] = None
        self.init_data: any = None
        self.fund_asset: Optional[str] = None
        #self._curr_ts: int = None  # virtual timestamp during building

    def for_strategy(self, strategy_script_path, custom_config: dict) -> "StrategyTestBuilder":
        strategy_config = StrategyConfig()
        strategy_config.strategy_file = strategy_script_path
        strategy_config.custom_params = custom_config
        strategy_config.strategy_name = "test_strategy"
        self.strategy_config = strategy_config
        return self 
        

    def with_symbols_mapping(self, symbol_mapping: dict[str, str]) -> "StrategyTestBuilder":
        self.symbols = symbol_mapping
        return self

    def with_accounts(self, accounts: list[int]) -> "StrategyTestBuilder":
        self.account_ids = accounts
        return self 

    def with_init_data(self, init_data: any) -> "StrategyTestBuilder":
        self.init_data = init_data
        return self

    def with_init_balances(self, init_balances: dict[int, dict[str, float]]) -> "StrategyTestBuilder":
        self.init_balances = init_balances
        return self

    def with_event_sequence(self, seq: TestEventSequence) -> "StrategyTestBuilder":
        self.event_seq = seq
        return self
    
    def with_fund_asset(self, fund_asset: str) -> "StrategyTestBuilder":
        self.fund_asset = fund_asset
        return self

    def with_match_simulator(self, sim: TestSim) -> "StrategyTestBuilder":
        self.sim = sim
        return self


    def build(self) -> StrategyBacktestor:
        strategy_config: StrategyConfig = self.strategy_config
        strategy_config.account_ids = self.account_ids
        strategy_config.symbols = self.symbols

        tick_ds = TestMDDataSource(self.event_seq.ticks)

        init_data_fetcher = lambda c, d, tq: self.init_data 

        bt_config = BacktestConfig()
        bt_config.symbols = self.symbols
        bt_config.account_ids = self.account_ids
        bt_config.start_dt = self.event_seq.start_dt
        bt_config.end_dt = self.event_seq.end_dt

        bt_config.print_logs = True
        bt_config.init_balances = self.init_balances 
        bt_config.init_data_fetcher = init_data_fetcher

        bt_config.simulator_gw = self.sim
        bt_config.fund_asset = self.fund_asset

        bt_config.data_source = tick_ds

        dummy_progress_handler = lambda p: None
        bt = StrategyBacktestor(
            strategy_config=strategy_config,
            backtest_config=bt_config,
            progress_callback=dummy_progress_handler)

        bt.rob.batch_insert_positionupdates(
            [(pos.timestamp, pos) for pos in self.event_seq.position_updates])

        bt.rob.batch_insert_orderupdates(
            [(order.timestamp, order) for order in self.event_seq.order_trades]
        )

        return bt 




if __name__ == "__main__":
    strategy_config = {
        "symbol": "ABC/USD@SIM1",
        "symbol_rtmd": "ABCUSD",
        "account_id": 123,
        "order_size": 1
    }

    test_matchsim = TestSimBuilder() \
        .no_match_next() \
        .no_match_next() \
        .build()

    test_seq = TestEventSequenceBuilder() \
        .start_at(datetime(year=2023, month=10, day=20, hour=8, minute=20, second=0)) \
        .end_at(datetime(year=2023, month=10, day=20, hour=8, minute=25, second=0)) \
        .clock_forward_millis(1000) \
        .add_tickdata_ob(symbol="ABCUSD", b1=100.0, s1=101.0) \
        .clock_forward_seconds(60*4) \
        .add_tickdata_ob(symbol="ABCUSD", b1=100.5, s1=101.5) \
        .clock_forward_millis(1000) \
        .build()

    tester = StrategyTestBuilder() \
        .for_strategy(
            strategy_script_path="/home/centos/tokkaquant-dev/tq-strategy-repo/tq_examples/apitest/api_test.py", 
            custom_config=strategy_config) \
        .with_symbols_mapping({"ABC/USD@SIM1": "ABC"}) \
        .with_accounts([123]) \
        .with_init_balances({123: {"USD": 1000000.0}}) \
        .with_init_data({}) \
        .with_event_sequence(test_seq) \
        .with_match_simulator(test_matchsim) \
        .build()
    

    tester.run()

    print(tester.get_result().sprint_all())
