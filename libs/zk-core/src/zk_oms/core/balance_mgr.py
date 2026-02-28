import copy
from typing import Optional, Union

from .models import *
import zk_datamodel.exch_gw as gw
import datetime
import logging

logger = logging.getLogger(__name__)


@dataclass
class BalanceManageConfigEntry:
    account_id: int = None
    allow_override_oms_managed_balance: bool = False


class BalanceManager:
    def __init__(self, instrument_refdata_lookuptable: dict[int, dict[str, InstrumentRefdata]],
                 account_mapping: list[OMSRouteEntry],
                 gw_configs: list[GwConfigEntry],
                 balance_mgr_config: list[BalanceManageConfigEntry] = None):


        # state
        self._balances: dict[int, dict[str, OMSPosition]] = {} # calculated balances
        self._exch_balances: dict[int, dict[str, OMSPosition]] = {} # gw sync balances
        self._SUCCESS = ValidateResult(True, None)

        # parsed config
        self._balance_config: dict[int, BalanceManageConfigEntry] = {}
        if balance_mgr_config:
            for bc in balance_mgr_config:
                self._balance_config[bc.account_id] = bc

        self._refdata: dict[str, InstrumentRefdata] = {}

        self._gw_configs: dict[str, GwConfigEntry] = {}
        self._account_reverse_map: dict[str, OMSRouteEntry] = {}  # exch_account_code -> accoutn_entry
        self._symbol_map: dict[int, dict[str, InstrumentRefdata]] = {}  # account_id -> exch_symbol_code -> refdata

        self.reload_instrument_lookuptable(instrument_refdata_lookuptable)
        self.reload_account_config(account_mapping, gw_configs)

        # for ir in instrument_refdata:
        #     if ir.disabled:
        #         continue # skip disabled instruments
        #     #account_id = None
        #     self._refdata[ir.instrument_code] = ir
        #     for acc in account_mapping:
        #         account_id = acc.accound_id
        #         gw_key = acc.gw_key
        #         gw_config = self._gw_configs[gw_key]
        #         if gw_config is None:
        #             raise Exception(f"gw_config {gw_key} not found")
        #         if gw_config.exch_name == ir.exch_name:
        #             if account_id not in self._symbol_map:
        #                 self._symbol_map[account_id] = {}
        #             self._symbol_map[account_id][ir.exch_symbol] = ir.instrument_code

                    #logger.info(f"adding balance symbol mapping entry for: account_id={account_id}, symbol={ir.instrument_code}, exch_symbol={ir.exch_symbol}")


    def reload_instrument_lookuptable(self, instrument_refdata_lookuptable: dict[int, dict[str, InstrumentRefdata]]):
        self._symbol_map = instrument_refdata_lookuptable


    def reload_account_config(self, account_routes: list[OMSRouteEntry], gw_configs: list[GwConfigEntry]):
        self._gw_configs = {}
        self._account_reverse_map = {}
        for gw_config in gw_configs:
            self._gw_configs[gw_config.gw_key] = gw_config

        for acc in account_routes:
            account_id = acc.accound_id
            exch_account_id = acc.exch_account_id
            # check duplicate
            if exch_account_id in self._account_reverse_map:
                raise Exception(f"duplicate mapping found for exch_account_id: {exch_account_id} ")
            self._account_reverse_map[exch_account_id] = acc

            if account_id not in self._exch_balances:
                self._exch_balances[account_id] = {}
            if account_id not in self._balances:
                self._balances[account_id] = {}


    def init_balances(self, init_balances: list[OMSPosition]):
        for b in init_balances:
            # update from-exchange balances
            b1 = b
            self.update_balance(b1, update_exch_data=True)

            # update oms-managed balances
            b2 = copy.deepcopy(b)
            b2.position_state.sync_timestamp = None # clear sync timestamp
            b2.position_state.is_from_exch=False
            b2.position_state.exch_data_raw = None
            self.update_balance(b2, update_exch_data=False)

    def get_balances_for_account(self, account_id:int, use_exch_data=False) -> list[OMSPosition]:
        if use_exch_data:
            if account_id in self._exch_balances:
                return list(self._exch_balances[account_id].values())
            else:
                return []
        else:
            if account_id in self._balances:
                return list(self._balances[account_id].values())
            else:
                return []


    def get_balance_for_symbol(self, account_id:int, symbol:str,
                               is_short=False, use_exch_data=False, create_if_not_exists=False) -> Optional[OMSPosition]:
        _balance_dict = self._exch_balances if use_exch_data else self._balances
        if account_id in _balance_dict:
            if symbol in _balance_dict[account_id]:
                return _balance_dict[account_id][symbol]
            else:
                if create_if_not_exists:
                    refdata_entry = self._refdata.get(symbol)
                    if refdata_entry is None:
                        instrument_type = common.InstrumentType.INST_TYPE_SPOT
                    else:
                        instrument_type = refdata_entry.instrument_type
                    oms_pos = OMSPosition.create_position(account_id=account_id, symbol=symbol,
                                                          is_short=is_short,
                                                          instrument_type=instrument_type,
                                                          is_from_exch=use_exch_data)
                    _balance_dict[account_id][symbol] = oms_pos
                    return oms_pos
                else:
                    return None


    def update_balance(self, balance_entry: OMSPosition, timestamp=None, update_exch_data=True):
        acc_id = balance_entry.account_id
        balances_to_be_updated = self._exch_balances if update_exch_data else self._balances
        if acc_id not in balances_to_be_updated:
            balances_to_be_updated[acc_id] = {}
        balances_to_be_updated[acc_id][balance_entry.symbol] = balance_entry  # todo: short position?

    def create_balance_change(self, account_id, orig_position:OMSPosition):
        pos_change = PositionChange(account_id=account_id, symbol=orig_position.symbol)
        pos_change.position = orig_position
        pos_change.is_short = orig_position.is_short
        return pos_change

    # def create_balance_change(self, account_id: int, symbol: str, is_short=False, orig_position:OMSPosition = None):
    #     pos_change = PositionChange(account_id=account_id, symbol=symbol)
    #     if orig_position is None:
    #         position = self.get_balance_for_symbol(account_id=account_id, symbol=symbol,
    #                                                is_short=is_short, create_if_not_exists=True)
    #         pos_change.position = position
    #     else:
    #         pos_change.position = orig_position
    #     pos_change.is_short = is_short
    #     return pos_change

    def check_changes(self, balance_changes: list[PositionChange]) -> ValidateResult:
        for pc in balance_changes:
            p = pc.position.position_state
            if p.avail_qty + pc.avail_change < .0 or p.total_qty + pc.total_change < .0:
                res = ValidateResult(False, f'Not enough available balance for {p.instrument_code}')
                return res
        return self._SUCCESS

    def apply_changes(self, balance_changes: list[PositionChange], timestamp: int) -> set[str]:
        changed_symbols = set()
        for pc in balance_changes:
            symbol = pc.symbol
            changed_symbols.add(symbol)
            p = pc.position.position_state
            
            p.total_qty += pc.total_change
            p.frozen_qty += pc.frozen_change

            # make sure available qty is not more than total qty
            if p.avail_qty + pc.avail_change > p.total_qty:
                logger.warning(f"available qty {p.avail_qty} + change {pc.avail_change} > total qty {p.total_qty} for {symbol}")
                p.avail_qty = p.total_qty
            else:
                p.avail_qty += pc.avail_change
            p.update_timestamp = int(timestamp)

            if p.total_qty < 0:
                # revert direction
                pc.position.is_short = not pc.position.is_short
                pc.position.position_state.long_short_type = \
                    common.LongShortType.LS_SHORT if pc.position.is_short else common.LongShortType.LS_LONG
                p.total_qty = abs(p.total_qty)
                p.avail_qty = abs(p.avail_qty)
        return  changed_symbols

    def merge_gw_balance_updates(self, gw_balance_updates: gw.BalanceUpdate) -> Optional[int]:
        if len(gw_balance_updates.balances) == 0:
            return None
        account_id = self._get_account_id(gw_balance_updates.balances[0].exch_account_code)
        if account_id is None:
            # todo: log error?
            error_msg = f"Cannot find account id mapping for {gw_balance_updates.balances[0].exch_account_code}"
            logger.error(error_msg)
            return None

        # only update oms managed balance for initialization
        no_oms_managed_balance = len(self._balances.get(account_id, {})) == 0

        should_update_oms_managed_balance = no_oms_managed_balance or \
            (account_id in self._balance_config and self._balance_config[account_id].allow_override_oms_managed_balance)

        for b in gw_balance_updates.balances:
            if b.instrument_type == common.InstrumentType.INST_TYPE_SPOT:
                symbol = b.instrument_code # should be symbols like USDC/ETH/NEAR etc so should not need conversion
            else:
                symbol_refdata = self._symbol_map.get(account_id, {}).get(b.instrument_code, None)
                symbol = symbol_refdata.instrument_id if symbol_refdata else None

            if symbol is None:
                err_msg = f"Cannot find position for {b.instrument_code} as {str(b.instrument_type)}; ignoring this one"
                logger.error(err_msg)
                continue

            is_short_pos = b.long_short_type == common.LongShortType.LS_SHORT
            pos=self.get_balance_for_symbol(account_id=account_id, symbol=symbol,
                                            is_short=is_short_pos, use_exch_data=True,
                                            create_if_not_exists=True)
            pos.symbol_exch = b.instrument_code
            pos.position_state.frozen_qty = b.qty - b.avail_qty
            pos.position_state.avail_qty = b.avail_qty
            pos.position_state.total_qty = b.qty
            pos.position_state.long_short_type = b.long_short_type
            pos.position_state.instrument_type = b.instrument_type
            pos.position_state.update_timestamp = b.update_timestamp
            pos.position_state.sync_timestamp = int(datetime.datetime.now().timestamp() * 1000)  # TODO: add use_time_emulation flag
            pos.position_state.exch_data_raw = b.message_raw

            # if oms_mananged_pos exists, update it when:
            # 1. it was never synced (sync_timestamp == None)
            # 2. it is allowed to override by configuration
            oms_managed_pos = self.get_balance_for_symbol(account_id=account_id, symbol=symbol,
                                                          is_short=is_short_pos, use_exch_data=False,
                                                          create_if_not_exists=False)
            if (should_update_oms_managed_balance or
                    (oms_managed_pos is not None and oms_managed_pos.position_state.sync_timestamp is None)):
                pos1 = copy.deepcopy(pos)
                pos1.position_state.is_from_exch = False
                pos1.position_state.exch_data_raw = None
                self.update_balance(pos1, update_exch_data=False)

        return account_id

    def _get_account_id(self, exch_account_code:str) -> Optional[int]:
        if exch_account_code not in self._account_reverse_map:
            raise Exception(f"Unknown account code {exch_account_code}")
        return self._account_reverse_map.get(exch_account_code).accound_id





