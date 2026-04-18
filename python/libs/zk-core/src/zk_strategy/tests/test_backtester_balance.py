"""End-to-end backtester tests for balance tracking across spot and margin instruments."""
import os
import pytest
from datetime import datetime

from zk_strategy.strategy_testutils import (
    StrategyTestBuilder, TestSimBuilder, TestEventSequenceBuilder,
)

STRAT_DIR = os.path.dirname(__file__)
BUY_ONCE = os.path.join(STRAT_DIR, "strat_buy_once.py")
SELL_ONCE = os.path.join(STRAT_DIR, "strat_sell_once.py")

ACCOUNT_ID = 123


def _run_backtest(symbol, sym_exch, init_balances, strategy_path, strategy_config,
                  match_builder, fund_asset=None, bid=100.0, ask=101.0):
    """Helper to build and run a single-tick backtest."""
    seq = (TestEventSequenceBuilder()
           .start_at(datetime(2024, 1, 1, 10, 0, 0))
           .clock_forward_millis(1000)
           .add_tickdata_ob(symbol=sym_exch, b1=bid, s1=ask)
           .clock_forward_millis(1000)
           .build())

    bt = (StrategyTestBuilder()
          .for_strategy(strategy_path, strategy_config)
          .with_symbols_mapping({symbol: sym_exch})
          .with_accounts([ACCOUNT_ID])
          .with_init_balances({ACCOUNT_ID: init_balances})
          .with_init_data({})
          .with_fund_asset(fund_asset)
          .with_event_sequence(seq)
          .with_match_simulator(match_builder.build())
          .build())

    bt.run()
    return bt


def _get_oms_balance(bt, account_id, symbol):
    """Get OMS balance for a symbol. Returns (avail_qty, total_qty, is_short)."""
    bal = bt.oms.oms_core.balance_mgr.get_balance_for_symbol(account_id, symbol)
    if bal is None:
        return None
    return (bal.position_state.avail_qty,
            bal.position_state.total_qty,
            bal.is_short)


def _get_exch_balance(bt, account_id, symbol):
    """Get exchange-sourced balance for a symbol (from SimGwBalanceMgr via OMS)."""
    bal = bt.oms.oms_core.balance_mgr.get_balance_for_symbol(
        account_id, symbol, use_exch_data=True)
    if bal is None:
        return None
    return (bal.position_state.avail_qty,
            bal.position_state.total_qty,
            bal.is_short)


# ---------------------------------------------------------------------------
# Spot tests
# ---------------------------------------------------------------------------

class TestSpotBalance:

    def test_spot_buy_and_fill(self):
        """Buy 1 ETH at 101 (ask), verify USD decreases and ETH increases."""
        bt = _run_backtest(
            symbol="ETH/USD@SIM1", sym_exch="ETHUSD",
            init_balances={"USD": 10000.0},
            strategy_path=BUY_ONCE,
            strategy_config={
                "symbol": "ETH/USD@SIM1",
                "account_id": ACCOUNT_ID,
                "order_qty": 1.0,
                "order_price": 101.0,
            },
            match_builder=TestSimBuilder().match_next_immediately(),
            fund_asset="USD",
            ask=101.0,
        )

        trades_df = bt.get_result().get_trades()
        assert trades_df is not None, "Expected a trade"
        assert len(trades_df) == 1
        assert trades_df.iloc[0]["side"] == "buy"
        assert trades_df.iloc[0]["price"] == 101.0
        assert trades_df.iloc[0]["qty"] == 1.0

        # OMS-managed balances: ETH position should increase
        eth_bal = _get_oms_balance(bt, ACCOUNT_ID, "ETH")
        assert eth_bal is not None, "ETH balance should exist"
        assert eth_bal[0] == pytest.approx(1.0), f"ETH avail should be 1.0, got {eth_bal[0]}"

        # OMS-managed balances: USD fund should decrease
        usd_bal = _get_oms_balance(bt, ACCOUNT_ID, "USD")
        assert usd_bal is not None, "USD balance should exist"
        assert usd_bal[0] == pytest.approx(10000.0 - 101.0), f"USD should be {10000-101}, got {usd_bal[0]}"

    def test_spot_sell_and_fill(self):
        """Sell 1 ETH at 100 (bid), verify ETH decreases and USD increases."""
        bt = _run_backtest(
            symbol="ETH/USD@SIM1", sym_exch="ETHUSD",
            init_balances={"USD": 5000.0, "ETH": 5.0},
            strategy_path=SELL_ONCE,
            strategy_config={
                "symbol": "ETH/USD@SIM1",
                "account_id": ACCOUNT_ID,
                "order_qty": 1.0,
                "order_price": 100.0,
            },
            match_builder=TestSimBuilder().match_next_immediately(),
            fund_asset="USD",
            bid=100.0,
        )

        trades_df = bt.get_result().get_trades()
        assert trades_df is not None
        assert len(trades_df) == 1
        assert trades_df.iloc[0]["side"] == "sell"

        eth_bal = _get_oms_balance(bt, ACCOUNT_ID, "ETH")
        assert eth_bal is not None
        assert eth_bal[0] == pytest.approx(4.0), f"ETH should be 4.0, got {eth_bal[0]}"

        usd_bal = _get_oms_balance(bt, ACCOUNT_ID, "USD")
        assert usd_bal is not None
        assert usd_bal[0] == pytest.approx(5000.0 + 100.0), f"USD should be 5100, got {usd_bal[0]}"

    def test_spot_cancel_no_balance_change(self):
        """Place order, no match, cancel — balance should not change."""
        bt = _run_backtest(
            symbol="ETH/USD@SIM1", sym_exch="ETHUSD",
            init_balances={"USD": 10000.0},
            strategy_path=BUY_ONCE,
            strategy_config={
                "symbol": "ETH/USD@SIM1",
                "account_id": ACCOUNT_ID,
                "order_qty": 1.0,
                "order_price": 101.0,
            },
            match_builder=TestSimBuilder().no_match_next(),
            fund_asset="USD",
        )

        trades_df = bt.get_result().get_trades()
        assert trades_df is None or len(trades_df) == 0

        # Balance should remain unchanged (order booked but not filled, then no cancel in this test)
        usd_bal = _get_oms_balance(bt, ACCOUNT_ID, "USD")
        assert usd_bal is not None
        # For spot, OMS freezes fund on order submission; since order is still open,
        # avail = init - frozen. But total should still be 10000.
        assert usd_bal[1] == pytest.approx(10000.0), f"USD total should be 10000, got {usd_bal[1]}"


# ---------------------------------------------------------------------------
# Margin (perp) tests
# ---------------------------------------------------------------------------

class TestMarginBalance:

    def test_perp_buy_and_fill(self):
        """Buy 1 ETH-P at 2000, verify position increases and fund decreases."""
        bt = _run_backtest(
            symbol="ETH-P/USDC@SIM1", sym_exch="ETHUSDC",
            init_balances={"USDC": 10000.0},
            strategy_path=BUY_ONCE,
            strategy_config={
                "symbol": "ETH-P/USDC@SIM1",
                "account_id": ACCOUNT_ID,
                "order_qty": 1.0,
                "order_price": 2000.0,
            },
            match_builder=TestSimBuilder().match_next_immediately(),
            fund_asset="USDC",
            bid=1999.0, ask=2000.0,
        )

        trades_df = bt.get_result().get_trades()
        assert trades_df is not None, "Expected a trade"
        assert len(trades_df) == 1

        # OMS-managed position for the perp instrument should increase
        pos = _get_oms_balance(bt, ACCOUNT_ID, "ETH-P/USDC@SIM1")
        assert pos is not None, "Perp position should exist"
        assert pos[0] == pytest.approx(1.0), f"Position avail should be 1.0, got {pos[0]}"
        assert pos[2] is False, "Should be long"

        # Fund (USDC) tracked via SimGwBalanceMgr → exch_balances
        usdc_exch = _get_exch_balance(bt, ACCOUNT_ID, "USDC")
        assert usdc_exch is not None, "USDC exch balance should exist"
        assert usdc_exch[0] == pytest.approx(10000.0 - 2000.0), \
            f"USDC should be {10000-2000}, got {usdc_exch[0]}"

    def test_perp_sell_reduces_position(self):
        """Start with long 2 ETH-P, sell 1 at 2500, verify position decreases."""
        bt = _run_backtest(
            symbol="ETH-P/USDC@SIM1", sym_exch="ETHUSDC",
            init_balances={"USDC": 10000.0, "ETH-P/USDC@SIM1": 2.0},
            strategy_path=SELL_ONCE,
            strategy_config={
                "symbol": "ETH-P/USDC@SIM1",
                "account_id": ACCOUNT_ID,
                "order_qty": 1.0,
                "order_price": 2500.0,
            },
            match_builder=TestSimBuilder().match_next_immediately(),
            fund_asset="USDC",
            bid=2500.0, ask=2501.0,
        )

        trades_df = bt.get_result().get_trades()
        assert trades_df is not None
        assert len(trades_df) == 1

        # Position should decrease from 2 to 1
        pos = _get_oms_balance(bt, ACCOUNT_ID, "ETH-P/USDC@SIM1")
        assert pos is not None
        assert pos[0] == pytest.approx(1.0), f"Position should be 1.0, got {pos[0]}"

        # Fund should increase by 2500
        usdc_exch = _get_exch_balance(bt, ACCOUNT_ID, "USDC")
        assert usdc_exch is not None
        assert usdc_exch[0] == pytest.approx(10000.0 + 2500.0), \
            f"USDC should be {10000+2500}, got {usdc_exch[0]}"


# ---------------------------------------------------------------------------
# CFD tests
# ---------------------------------------------------------------------------

class TestCFDBalance:

    def test_cfd_buy_and_fill(self):
        """Buy 10000 EUR-CFD at 1.10, verify position and fund changes."""
        bt = _run_backtest(
            symbol="EUR-CFD/USD@SIM1", sym_exch="EURUSD",
            init_balances={"USD": 50000.0},
            strategy_path=BUY_ONCE,
            strategy_config={
                "symbol": "EUR-CFD/USD@SIM1",
                "account_id": ACCOUNT_ID,
                "order_qty": 10000.0,
                "order_price": 1.10,
            },
            match_builder=TestSimBuilder().match_next_immediately(),
            fund_asset="USD",
            bid=1.09, ask=1.10,
        )

        trades_df = bt.get_result().get_trades()
        assert trades_df is not None
        assert len(trades_df) == 1

        # Position for CFD instrument
        pos = _get_oms_balance(bt, ACCOUNT_ID, "EUR-CFD/USD@SIM1")
        assert pos is not None, "CFD position should exist"
        assert pos[0] == pytest.approx(10000.0), f"Position should be 10000, got {pos[0]}"

        # Fund decreases by notional (10000 * 1.10 = 11000)
        usd_exch = _get_exch_balance(bt, ACCOUNT_ID, "USD")
        assert usd_exch is not None
        assert usd_exch[0] == pytest.approx(50000.0 - 11000.0), \
            f"USD should be {50000-11000}, got {usd_exch[0]}"
