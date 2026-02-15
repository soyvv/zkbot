import math
from dataclasses import dataclass
from datetime import datetime
from typing import Callable

import pandas as pd
import numpy as np



@dataclass
class PnLProfile:
    pnl_per_trade: float


class TradeGroupManager:

    def __init__(self):
        self.groups = {}
        self.group_id = 0

    def add_trade(self, trade: pd.Series):
        group_id = self.group_id
        if group_id not in self.groups:
            self.groups[group_id] = []
        self.groups[group_id].append(trade)
        return group_id


def calc_pnl_single_symbol(trades: pd.DataFrame, fee_calc: Callable[[tuple], float]) -> pd.DataFrame:

    pnl_series = [] # (ts, r_pnl, t_pnl,  avg_cost, holding_qty, acc_fee)
    curr_avg_cost = .0
    curr_qty = .0
    curr_realized_pnl = .0
    curr_pnl = .0
    curr_ts = 0
    curr_acc_fee = .0


    for trade in trades.itertuples():
        ts = trade.ts
        price = trade.price
        qty = trade.qty
        side = trade.side

        fee = fee_calc(trade) if fee_calc is not None else 0.0

        curr_acc_fee += fee

        if side == "buy":
            if curr_qty < .0:
                if curr_qty + qty <= .0: # close short position
                    delta_qty = qty
                    delta_realized_pnl = delta_qty * ( curr_avg_cost - price )

                    curr_qty += delta_qty
                    curr_realized_pnl += delta_realized_pnl
                    curr_pnl = (price - curr_avg_cost) * curr_qty + curr_realized_pnl
                else:
                    # split the buy to 2 buys; one to close and the other for opening long
                    delta_qty1 = -curr_qty
                    delta_qty2 = qty + curr_qty # qty is negative

                    # for buy1, close the short position and book realized pnl
                    delta_realized_pnl1 = delta_qty1 * ( curr_avg_cost - price )

                    # for buy2, open long position and book new cost
                    new_avg_cost = price

                    # to update in summary
                    curr_qty = delta_qty2
                    curr_avg_cost = new_avg_cost
                    curr_realized_pnl += delta_realized_pnl1
                    curr_pnl = curr_realized_pnl  # pnl is realized pnl only
            else: # accumulate long position; will adjust avg cost; pnl is unrealized pnl
                delta_qty = qty
                new_cost = (curr_avg_cost * curr_qty + price * delta_qty) / (curr_qty + delta_qty)
                curr_qty += delta_qty
                curr_avg_cost = new_cost
                curr_pnl = (price - new_cost) * curr_qty + curr_realized_pnl

        elif side == "sell":
            if curr_qty > .0:
                if curr_qty - qty >= .0: # close long position
                    delta_qty = qty
                    delta_realized_pnl = delta_qty * (price - curr_avg_cost)

                    curr_qty -= delta_qty
                    curr_realized_pnl += delta_realized_pnl
                    curr_pnl = (price - curr_avg_cost) * curr_qty + curr_realized_pnl
                else:
                    # split the sell to 2 sells; one to close and the other for opening short
                    delta_qty1 = curr_qty
                    delta_qty2 = qty - curr_qty

                    # for sell1, close the long position and book realized pnl
                    delta_realized_pnl1 = delta_qty1 * (price - curr_avg_cost)

                    # for sell2, open short position and book new cost
                    new_avg_cost = price

                    # to update in summary
                    curr_qty = -delta_qty2
                    curr_avg_cost = new_avg_cost
                    curr_realized_pnl += delta_realized_pnl1
                    curr_pnl = curr_realized_pnl  # pnl is realized pnl only
            else:
                # accumulating short position
                delta_qty = -qty
                new_cost = (curr_avg_cost * curr_qty + price * delta_qty) / (curr_qty + delta_qty) # both curr_qty and delta_qty are negative
                curr_qty += delta_qty
                curr_avg_cost = new_cost
                curr_pnl = (price - curr_avg_cost) * curr_qty + curr_realized_pnl

        pnl_series += [(ts, curr_realized_pnl, curr_pnl, curr_avg_cost, curr_qty, curr_acc_fee)]

    pnl_df = pd.DataFrame(pnl_series, columns=["ts", "r_pnl", "t_pnl", "avg_cost", "holding_qty", "acc_fee"])

    return pnl_df



def calc_pnl_profile(pnl_history: pd.DataFrame):
    pass


def calc_profit_loss_ratio(trades: pd.DataFrame):
    pass






def test():

    tests = []
    tests.append([
        ("buy", 5, 1010.0),
        ("buy", 5, 1030.0),
        ("sell", 3, 1080.0),
        ("sell", 6, 1085.0),
        ("sell", 1, 1040.0),
    ])

    tests.append([
        ("sell", 3, 1080.0),
        ("sell", 6, 1085.0),
        ("sell", 1, 1040.0),
        ("buy", 5, 1010.0),
        ("buy", 5, 1030.0),
    ])

    tests.append([
        ("buy", 5, 1010.0),
        ("buy", 5, 1030.0),
        ("sell", 20, 1080.0),
        ("sell", 1, 1040.0),
        ("buy", 5, 1050.0),
        ("buy", 5, 1070.0),
        ("buy", 5, 1030.0),
    ])


    for test in tests:
        pnl = run_pnl_test(test)
        print(pnl)


def run_pnl_test(trades: list[tuple[str, float, float]]):
    trades_df = pd.DataFrame(trades, columns=["side", "qty", "price"])
    base_ts = int(datetime.now().timestamp() * 1000)
    trades_df["ts"] = base_ts + trades_df.index * 10000

    def fee_calc(trade):
        return trade.qty * trade.price * 0.001

    pnl = calc_pnl_single_symbol(trades_df, fee_calc)

    return pnl


if __name__ == "__main__":
    test()

