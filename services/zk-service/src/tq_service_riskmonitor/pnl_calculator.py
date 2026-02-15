from dataclasses import dataclass
import zk_datamodel.oms as oms
import zk_datamodel.common as common

@dataclass
class PnlEntry:
    account_id: int = None
    symbol: str = None
    as_of_ts: int = None
    ts: int = None
    curr_qty: float = 0.0
    curr_avg_cost: float = 0.0
    total_pnl: float = 0.0
    realized_pnl: float = 0.0
    unrealized_pnl: float = 0.0

@dataclass
class PnlBreakdown:
    account_pnl_dict = {} # account_id -> (total_pnl, realized_pnl, unrealized_pnl)
    account_symbol_pnl_dict = {} # account_id -> symbol -> (total_pnl, realized_pnl, unrealized_pnl)

class PnlCalculator:

    def __init__(self):
        self.start_time = None
        self.pnl_breakdown: PnlBreakdown = PnlBreakdown()


    def on_trade(self, trade: oms.Trade) -> PnlEntry:
        sym = trade.instrument
        account_id = trade.account_id
        if not self.start_time:
            self.start_time = trade.filled_ts
        self._ensure_entry(account_id, sym)

        pnl_entry = self.pnl_breakdown.account_symbol_pnl_dict[account_id][sym]
        self._update_pnl(trade, pnl_entry)

        return pnl_entry

    def _ensure_entry(self, account_id: int, symbol: str):

        account_symbol_pnl_dict = self.pnl_breakdown.account_symbol_pnl_dict

        if pnl_account_entry := account_symbol_pnl_dict.get(account_id, None):
            if pnl_account_entry.get(symbol, None):
                return
            else:
                pnl_account_entry[symbol] = PnlEntry(account_id=account_id,
                                             symbol=symbol,
                                             as_of_ts=self.start_time)
        else:
            account_symbol_pnl_dict[account_id] = {
                symbol: PnlEntry(account_id=account_id, symbol=symbol, as_of_ts=self.start_time)}

    def get_pnl_details(self) -> PnlBreakdown:
        return self.pnl_breakdown


    def _update_pnl(self, trade: oms.Trade, cur_pnl_entry: PnlEntry):
        ts = trade.filled_ts
        price = trade.filled_price
        qty = trade.filled_qty
        side = "sell" if trade.buy_sell_type == common.BuySellType.BS_SELL else "buy"

        curr_qty = cur_pnl_entry.curr_qty
        curr_pnl = cur_pnl_entry.total_pnl
        curr_avg_cost = cur_pnl_entry.curr_avg_cost
        curr_realized_pnl = cur_pnl_entry.realized_pnl


        if side == "buy":
            if curr_qty < .0:
                if curr_qty + qty <= .0:  # close short position
                    delta_qty = qty
                    delta_realized_pnl = delta_qty * (curr_avg_cost - price)

                    curr_qty += delta_qty
                    curr_realized_pnl += delta_realized_pnl
                    curr_pnl = (price - curr_avg_cost) * curr_qty + curr_realized_pnl
                else:
                    # split the buy to 2 buys; one to close and the other for opening long
                    delta_qty1 = -curr_qty
                    delta_qty2 = qty + curr_qty  # qty is negative

                    # for buy1, close the short position and book realized pnl
                    delta_realized_pnl1 = delta_qty1 * (curr_avg_cost - price)

                    # for buy2, open long position and book new cost
                    new_avg_cost = price

                    # to update in summary
                    curr_qty = delta_qty2
                    curr_avg_cost = new_avg_cost
                    curr_realized_pnl += delta_realized_pnl1
                    curr_pnl = curr_realized_pnl  # pnl is realized pnl only
            else:  # accumulate long position; will adjust avg cost; pnl is unrealized pnl
                delta_qty = qty
                new_cost = (curr_avg_cost * curr_qty + price * delta_qty) / (curr_qty + delta_qty)
                curr_qty += delta_qty
                curr_avg_cost = new_cost
                curr_pnl = (price - new_cost) * curr_qty + curr_realized_pnl

        elif side == "sell":
            if curr_qty > .0:
                if curr_qty - qty >= .0:  # close long position
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
                new_cost = (curr_avg_cost * curr_qty + price * delta_qty) / (
                            curr_qty + delta_qty)  # both curr_qty and delta_qty are negative
                curr_qty += delta_qty
                curr_avg_cost = new_cost
                curr_pnl = (price - curr_avg_cost) * curr_qty + curr_realized_pnl

        #pnl_series += [(ts, curr_realized_pnl, curr_pnl, curr_avg_cost, curr_qty, curr_acc_fee)]
        cur_pnl_entry.realized_pnl = curr_realized_pnl
        cur_pnl_entry.total_pnl = curr_pnl
        cur_pnl_entry.unrealized_pnl = curr_pnl - curr_realized_pnl
        cur_pnl_entry.curr_qty = curr_qty
        cur_pnl_entry.curr_avg_cost = curr_avg_cost
        cur_pnl_entry.ts = ts