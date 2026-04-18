import math
from dataclasses import dataclass
from datetime import datetime
from typing import Callable

import pandas as pd
import numpy as np



@dataclass
class PnLProfile:
    total_pnl: float
    total_fee: float
    num_trades: int               # number of round trips
    num_winning: int
    num_losing: int
    win_rate: float               # num_winning / num_trades
    avg_pnl_per_trade: float
    avg_winning_pnl: float
    avg_losing_pnl: float
    profit_loss_ratio: float      # avg_winning_pnl / abs(avg_losing_pnl)
    max_drawdown: float           # max peak-to-trough of cumulative PnL
    max_drawdown_pct: float       # max_drawdown as % of peak equity
    trades_per_day: float
    avg_holding_period_s: float
    max_consecutive_wins: int
    max_consecutive_losses: int
    pnl_per_trade: list           # per-round-trip PnLs (for histogram)


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



def extract_round_trips(pnl_df: pd.DataFrame) -> list[dict]:
    """Extract round-trip trades from a PnL DataFrame.

    A round trip is a complete position cycle: flat -> open -> flat.
    Detected by tracking when holding_qty transitions through zero.
    """
    round_trips = []
    entry_idx: int | None = None
    prev_qty = 0.0
    r_pnl_at_entry = 0.0
    fee_at_entry = 0.0
    direction = "long"

    ts_arr = pnl_df["ts"].values
    r_pnl_arr = pnl_df["r_pnl"].values
    qty_arr = pnl_df["holding_qty"].values
    fee_arr = pnl_df["acc_fee"].values

    for i in range(len(pnl_df)):
        qty = float(qty_arr[i])

        if prev_qty == 0.0 and qty != 0.0:
            entry_idx = i
            r_pnl_at_entry = float(r_pnl_arr[i - 1]) if i > 0 else 0.0
            fee_at_entry = float(fee_arr[i - 1]) if i > 0 else 0.0
            direction = "long" if qty > 0 else "short"

        elif prev_qty != 0.0 and qty == 0.0:
            if entry_idx is not None:
                entry_ts = int(ts_arr[entry_idx])
                exit_ts = int(ts_arr[i])
                pnl = float(r_pnl_arr[i]) - r_pnl_at_entry
                fee = float(fee_arr[i]) - fee_at_entry
                holding_period_s = (exit_ts - entry_ts) / 1000.0
                round_trips.append({
                    "entry_ts": entry_ts,
                    "exit_ts": exit_ts,
                    "pnl": pnl,
                    "fee": fee,
                    "direction": direction,
                    "holding_period_s": holding_period_s,
                })
                entry_idx = None

        elif prev_qty != 0.0 and qty != 0.0 and np.sign(prev_qty) != np.sign(qty):
            if entry_idx is not None:
                entry_ts = int(ts_arr[entry_idx])
                exit_ts = int(ts_arr[i])
                pnl = float(r_pnl_arr[i]) - r_pnl_at_entry
                fee = float(fee_arr[i]) - fee_at_entry
                holding_period_s = (exit_ts - entry_ts) / 1000.0
                round_trips.append({
                    "entry_ts": entry_ts,
                    "exit_ts": exit_ts,
                    "pnl": pnl,
                    "fee": fee,
                    "direction": direction,
                    "holding_period_s": holding_period_s,
                })
            entry_idx = i
            r_pnl_at_entry = float(r_pnl_arr[i])
            fee_at_entry = float(fee_arr[i])
            direction = "long" if qty > 0 else "short"

        prev_qty = qty

    return round_trips


def calc_pnl_profile(pnl_df: pd.DataFrame) -> PnLProfile:
    """Compute trade statistics from a PnL DataFrame produced by calc_pnl_single_symbol."""
    round_trips = extract_round_trips(pnl_df)

    if len(round_trips) == 0:
        return PnLProfile(
            total_pnl=pnl_df["t_pnl"].iloc[-1] if len(pnl_df) > 0 else 0.0,
            total_fee=pnl_df["acc_fee"].iloc[-1] if len(pnl_df) > 0 else 0.0,
            num_trades=0, num_winning=0, num_losing=0,
            win_rate=0.0, avg_pnl_per_trade=0.0,
            avg_winning_pnl=0.0, avg_losing_pnl=0.0,
            profit_loss_ratio=0.0,
            max_drawdown=0.0, max_drawdown_pct=0.0,
            trades_per_day=0.0, avg_holding_period_s=0.0,
            max_consecutive_wins=0, max_consecutive_losses=0,
            pnl_per_trade=[],
        )

    pnls = [rt["pnl"] for rt in round_trips]
    winning_pnls = [p for p in pnls if p > 0]
    losing_pnls = [p for p in pnls if p <= 0]

    num_trades = len(round_trips)
    num_winning = len(winning_pnls)
    num_losing = len(losing_pnls)
    win_rate = num_winning / num_trades

    avg_pnl_per_trade = sum(pnls) / num_trades
    avg_winning_pnl = sum(winning_pnls) / num_winning if num_winning > 0 else 0.0
    avg_losing_pnl = sum(losing_pnls) / num_losing if num_losing > 0 else 0.0

    profit_loss_ratio = avg_winning_pnl / abs(avg_losing_pnl) if avg_losing_pnl != 0 else float("inf")

    # max drawdown from cumulative t_pnl
    cumulative_pnl = pnl_df["t_pnl"].values
    running_max = np.maximum.accumulate(cumulative_pnl)
    drawdowns = running_max - cumulative_pnl
    max_drawdown = float(np.max(drawdowns)) if len(drawdowns) > 0 else 0.0
    peak_at_max_dd = float(running_max[np.argmax(drawdowns)]) if len(drawdowns) > 0 else 0.0
    max_drawdown_pct = (max_drawdown / peak_at_max_dd * 100) if peak_at_max_dd > 0 else 0.0

    # trades per day
    first_ts = pnl_df["ts"].iloc[0]
    last_ts = pnl_df["ts"].iloc[-1]
    duration_days = (last_ts - first_ts) / (1000.0 * 86400)
    trades_per_day = num_trades / duration_days if duration_days > 0 else float(num_trades)

    # avg holding period
    avg_holding_period_s = sum(rt["holding_period_s"] for rt in round_trips) / num_trades

    # consecutive wins / losses
    max_consecutive_wins = 0
    max_consecutive_losses = 0
    curr_wins = 0
    curr_losses = 0
    for p in pnls:
        if p > 0:
            curr_wins += 1
            curr_losses = 0
            max_consecutive_wins = max(max_consecutive_wins, curr_wins)
        else:
            curr_losses += 1
            curr_wins = 0
            max_consecutive_losses = max(max_consecutive_losses, curr_losses)

    total_pnl = float(pnl_df["t_pnl"].iloc[-1])
    total_fee = float(pnl_df["acc_fee"].iloc[-1])

    return PnLProfile(
        total_pnl=total_pnl,
        total_fee=total_fee,
        num_trades=num_trades,
        num_winning=num_winning,
        num_losing=num_losing,
        win_rate=win_rate,
        avg_pnl_per_trade=avg_pnl_per_trade,
        avg_winning_pnl=avg_winning_pnl,
        avg_losing_pnl=avg_losing_pnl,
        profit_loss_ratio=profit_loss_ratio,
        max_drawdown=max_drawdown,
        max_drawdown_pct=max_drawdown_pct,
        trades_per_day=trades_per_day,
        avg_holding_period_s=avg_holding_period_s,
        max_consecutive_wins=max_consecutive_wins,
        max_consecutive_losses=max_consecutive_losses,
        pnl_per_trade=pnls,
    )






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


    for i, t in enumerate(tests):
        print(f"\n=== Test {i+1} ===")
        pnl = run_pnl_test(t)
        print(pnl)
        profile = calc_pnl_profile(pnl)
        print(f"\nPnL Profile:")
        print(f"  total_pnl={profile.total_pnl:.2f}, total_fee={profile.total_fee:.2f}")
        print(f"  num_trades={profile.num_trades}, win_rate={profile.win_rate:.1%}")
        print(f"  avg_pnl_per_trade={profile.avg_pnl_per_trade:.2f}")
        print(f"  avg_winning={profile.avg_winning_pnl:.2f}, avg_losing={profile.avg_losing_pnl:.2f}")
        print(f"  profit_loss_ratio={profile.profit_loss_ratio:.2f}")
        print(f"  max_drawdown={profile.max_drawdown:.2f} ({profile.max_drawdown_pct:.1f}%)")
        print(f"  trades_per_day={profile.trades_per_day:.2f}")
        print(f"  avg_holding_period_s={profile.avg_holding_period_s:.1f}")
        print(f"  max_consecutive_wins={profile.max_consecutive_wins}, max_consecutive_losses={profile.max_consecutive_losses}")
        print(f"  pnl_per_trade={profile.pnl_per_trade}")


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

