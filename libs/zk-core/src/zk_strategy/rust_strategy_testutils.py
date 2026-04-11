from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime
from typing import Any

import zk_backtest


def _to_unix_ms(dt: datetime) -> int:
    return int(dt.timestamp() * 1000)


def _default_refdata(symbol: str) -> dict[str, Any]:
    return {
        "instrument_id": symbol,
        "instrument_id_exchange": symbol,
        "exchange_name": "SIM1",
    }


class TestEventSequenceBuilder:
    def __init__(self) -> None:
        self._current_ts_ms = 0
        self._events: list[dict[str, Any]] = []

    def start_at(self, dt: datetime) -> "TestEventSequenceBuilder":
        self._current_ts_ms = _to_unix_ms(dt)
        return self

    def start_at_ms(self, ts_ms: int) -> "TestEventSequenceBuilder":
        self._current_ts_ms = ts_ms
        return self

    def clock_forward_millis(self, delta_ms: int) -> "TestEventSequenceBuilder":
        self._current_ts_ms += delta_ms
        return self

    def add_tickdata_ob(
        self,
        symbol: str,
        b1: float,
        s1: float,
        bv1: float = 1.0,
        sv1: float = 1.0,
        trade_price: float | None = None,
        trade_qty: float | None = None,
    ) -> "TestEventSequenceBuilder":
        self._events.append(
            {
                "kind": "tick",
                "ts_ms": self._current_ts_ms,
                "symbol": symbol,
                "bids": [(b1, bv1)],
                "asks": [(s1, sv1)],
                "trade_price": trade_price,
                "trade_qty": trade_qty,
            }
        )
        return self

    def add_bar(
        self,
        symbol: str,
        open: float,
        high: float,
        low: float,
        close: float,
        volume: float = 0.0,
    ) -> "TestEventSequenceBuilder":
        self._events.append(
            {
                "kind": "bar",
                "ts_ms": self._current_ts_ms,
                "symbol": symbol,
                "open": open,
                "high": high,
                "low": low,
                "close": close,
                "volume": volume,
            }
        )
        return self

    def build(self) -> list[dict[str, Any]]:
        return sorted(self._events, key=lambda event: event["ts_ms"])


@dataclass
class StrategyTestHarness:
    strategy: Any
    config: dict[str, Any]
    account_ids: list[int]
    refdata: list[dict[str, Any]]
    init_balances: dict[int, dict[str, float]] | None
    init_positions: dict[int, dict[str, float]] | None
    match_policy: str
    event_sequence: list[dict[str, Any]]
    init_data_fetcher: Any | None = None

    def __post_init__(self) -> None:
        self.result: dict[str, Any] | None = None

    def run(self) -> dict[str, Any]:
        bt = zk_backtest.RustBacktester(
            account_ids=self.account_ids,
            refdata=self.refdata,
            init_balances=self.init_balances,
            init_positions=self.init_positions,
            match_policy=self.match_policy,
            include_klines_in_result=True,
        )
        for event in self.event_sequence:
            kind = event["kind"]
            if kind == "tick":
                bt.push_tick(
                    event["ts_ms"],
                    event["symbol"],
                    event["bids"],
                    event["asks"],
                    event.get("trade_price"),
                    event.get("trade_qty"),
                )
            elif kind == "bar":
                bt.push_bar(
                    event["ts_ms"],
                    event["symbol"],
                    event["open"],
                    event["high"],
                    event["low"],
                    event["close"],
                    event["volume"],
                )
            else:
                raise ValueError(f"unsupported event kind: {kind}")

        self.result = bt.run(self.strategy, self.config, self.init_data_fetcher)
        return self.result


class StrategyTestBuilder:
    def __init__(self) -> None:
        self._strategy = None
        self._config: dict[str, Any] = {}
        self._account_ids: list[int] = []
        self._refdata: list[dict[str, Any]] = []
        self._init_balances: dict[int, dict[str, float]] | None = None
        self._init_positions: dict[int, dict[str, float]] | None = None
        self._match_policy = "immediate"
        self._event_sequence: list[dict[str, Any]] = []
        self._init_data_fetcher: Any | None = None

    def for_strategy(self, strategy: Any, config: dict[str, Any]) -> "StrategyTestBuilder":
        self._strategy = strategy
        self._config = config
        return self

    def with_accounts(self, account_ids: list[int]) -> "StrategyTestBuilder":
        self._account_ids = account_ids
        return self

    def with_refdata(self, refdata: list[dict[str, Any]]) -> "StrategyTestBuilder":
        self._refdata = refdata
        return self

    def with_symbols(self, symbols: list[str]) -> "StrategyTestBuilder":
        self._refdata = [_default_refdata(symbol) for symbol in symbols]
        return self

    def with_init_balances(
        self, init_balances: dict[int, dict[str, float]]
    ) -> "StrategyTestBuilder":
        self._init_balances = init_balances
        return self

    def with_init_positions(
        self, init_positions: dict[int, dict[str, float]]
    ) -> "StrategyTestBuilder":
        self._init_positions = init_positions
        return self

    def with_event_sequence(
        self, event_sequence: list[dict[str, Any]]
    ) -> "StrategyTestBuilder":
        self._event_sequence = event_sequence
        return self

    def with_match_policy(self, match_policy: str) -> "StrategyTestBuilder":
        self._match_policy = match_policy
        return self

    def with_init_data(self, init_data: Any) -> "StrategyTestBuilder":
        self._init_data_fetcher = lambda: init_data
        return self

    def with_init_data_fetcher(self, init_data_fetcher: Any) -> "StrategyTestBuilder":
        self._init_data_fetcher = init_data_fetcher
        return self

    def build(self) -> StrategyTestHarness:
        if self._strategy is None:
            raise ValueError("strategy instance is required")
        if not self._account_ids:
            raise ValueError("at least one account_id is required")
        if not self._refdata:
            raise ValueError("refdata or symbols are required")

        return StrategyTestHarness(
            strategy=self._strategy,
            config=self._config,
            account_ids=self._account_ids,
            refdata=self._refdata,
            init_balances=self._init_balances,
            init_positions=self._init_positions,
            match_policy=self._match_policy,
            event_sequence=self._event_sequence,
            init_data_fetcher=self._init_data_fetcher,
        )
