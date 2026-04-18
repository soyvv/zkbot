"""Simulated balance manager for tracking fund and position changes on fills.

Extracted from tq_service_simulator.gw_simulator.SimGwBalanceMgr.
Used by both the simulator service (live) and BacktestOMSV2 (backtest).
"""

import datetime
import time
from typing import Optional

import zk_proto_betterproto.exch_gw as gw
import zk_proto_betterproto.common as common
from loguru import logger


class SimGwBalanceMgr:
    def __init__(self, init_balances: list[tuple[str, float]],
                 fund_asset: str,
                 exch_account_id: str,
                 refdata: list[common.InstrumentRefData]):

        self.fund_asset = fund_asset
        self.exch_account_id = exch_account_id
        self.refdata_dict: dict[str, common.InstrumentRefData] = \
            {rd.instrument_id_exchange: rd for rd in refdata}
        self.balance_reports: dict[str, gw.PositionReport] = {}
        for symbol, qty in init_balances:
            if symbol == fund_asset:
                p = gw.PositionReport()
                p.qty = qty
                p.avail_qty = qty
                p.exch_account_code = exch_account_id
                p.update_timestamp = int(datetime.datetime.now().timestamp() * 1000)
                p.instrument_code = symbol
                p.instrument_type = common.InstrumentType.INST_TYPE_SPOT
                p.long_short_type = common.LongShortType.LS_LONG
                p.message_raw = ""
                self.balance_reports[symbol] = p
            else:
                refdata = self.refdata_dict.get(symbol, None)
                p = gw.PositionReport()
                p.qty = abs(qty)
                p.avail_qty = abs(qty)
                p.exch_account_code = exch_account_id
                p.update_timestamp = int(datetime.datetime.now().timestamp() * 1000)
                if refdata and refdata.instrument_type != common.InstrumentType.INST_TYPE_SPOT:
                    p.instrument_code = refdata.instrument_id_exchange
                    p.instrument_type = refdata.instrument_type
                    p.long_short_type = common.LongShortType.LS_LONG if qty > 0 else common.LongShortType.LS_SHORT
                else:
                    p.instrument_code = symbol
                    p.instrument_type = common.InstrumentType.INST_TYPE_SPOT
                    p.long_short_type = common.LongShortType.LS_LONG
                p.message_raw = ""
                self.balance_reports[symbol] = p

    def update_balance(self, gw_reports: list[gw.OrderReport]) \
            -> Optional[gw.BalanceUpdate]:
        changed_assets = set()
        for gw_report in gw_reports:

            for rep_entry in gw_report.order_report_entries:
                if rep_entry.report_type == gw.OrderReportType.ORDER_REP_TYPE_TRADE:
                    trade = rep_entry.trade_report
                    symbol = trade.order_info.instrument
                    buy_sell = trade.order_info.buy_sell_type
                    refdata = self.refdata_dict.get(symbol)
                    if not refdata:
                        logger.warning(f"refdata not found for symbol {symbol}")
                        continue
                    price = trade.filled_price
                    qty = trade.filled_qty
                    amount = price * qty
                    if refdata.instrument_type != common.InstrumentType.INST_TYPE_SPOT:
                        asset_report = self.get_or_create_position(symbol, refdata.instrument_type)
                    else:
                        base_asset = refdata.base_asset
                        asset_report = self.get_or_create_position(base_asset, common.InstrumentType.INST_TYPE_SPOT)
                    changed_assets.add(asset_report.instrument_code)
                    changed_assets.add(self.fund_asset)
                    fund_report = self.balance_reports.get(self.fund_asset)
                    if buy_sell == common.BuySellType.BS_BUY:
                        fund_report.qty -= amount
                        fund_report.avail_qty -= amount
                        if asset_report.long_short_type == common.LongShortType.LS_LONG:
                            asset_report.qty += qty
                            asset_report.avail_qty += qty
                        else:
                            asset_report.qty -= qty
                            asset_report.avail_qty -= qty
                    else:
                        fund_report.qty += amount
                        fund_report.avail_qty += amount
                        if asset_report.long_short_type == common.LongShortType.LS_LONG:
                            asset_report.qty -= qty
                            asset_report.avail_qty -= qty
                        else:
                            asset_report.qty += qty
                            asset_report.avail_qty += qty
                    self._normalize_position(asset_report)

        if len(changed_assets) > 0:
            balance_update = gw.BalanceUpdate()
            balance_update.balances = [self.balance_reports[s] for s in changed_assets]
            return balance_update
        return None

    def _normalize_position(self, pos_report: gw.PositionReport):
        if pos_report.qty < 0 and pos_report.long_short_type == common.LongShortType.LS_LONG:
            pos_report.qty = abs(pos_report.qty)
            pos_report.avail_qty = abs(pos_report.avail_qty)
            pos_report.long_short_type = common.LongShortType.LS_SHORT
        elif pos_report.qty < 0 and pos_report.long_short_type == common.LongShortType.LS_SHORT:
            pos_report.qty = abs(pos_report.qty)
            pos_report.avail_qty = abs(pos_report.avail_qty)
            pos_report.long_short_type = common.LongShortType.LS_LONG

    def get_current_balances(self) -> list[gw.PositionReport]:
        return list(self.balance_reports.values())

    def get_or_create_position(self, asset_name: str,
                               asset_type=common.InstrumentType.INST_TYPE_SPOT) -> gw.PositionReport:
        if asset_name not in self.balance_reports:
            pos_report = gw.PositionReport()
            pos_report.instrument_code = asset_name
            pos_report.exch_account_code = self.exch_account_id
            pos_report.instrument_type = asset_type
            pos_report.qty = .0
            pos_report.avail_qty = .0
            pos_report.long_short_type = common.LongShortType.LS_LONG
            pos_report.update_timestamp = int(time.time() * 1000)
            self.balance_reports[asset_name] = pos_report

        return self.balance_reports[asset_name]
