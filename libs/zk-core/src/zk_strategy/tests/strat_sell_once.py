"""Test strategy: sell once on first tick, then do nothing."""
from zk_strategy.strategy_base import StrategyBase
from zk_strategy.api import TokkaQuant


class Strategy(StrategyBase):
    def on_reinit(self, config, tq: TokkaQuant):
        self.symbol = config.get("symbol")
        self.account_id = config.get("account_id")
        self.order_qty = config.get("order_qty", 1.0)
        self.order_price = config.get("order_price", None)
        self._ordered = False

    def on_tick(self, tick, tq: TokkaQuant):
        if not self._ordered:
            price = self.order_price if self.order_price else tick.buy_price_levels[0].price
            tq.sell(account_id=self.account_id, symbol=self.symbol,
                    qty=self.order_qty, price=price)
            self._ordered = True
