from zk_datamodel.ods import OmsConfigEntry
from zk_oms.core.models import OMSRouteEntry, GwConfigEntry, InstrumentTradingConfig, InstrumentRefdata


class ConfdataManager:


    def __init__(self, oms_id: str):
        self._oms_id = oms_id
        self._oms_managed_accounts: set[int] = None
        self._account_routes: list[OMSRouteEntry] = None
        self._gw_configs: dict[str, GwConfigEntry] = None
        self._gw_configs_list: list[GwConfigEntry] = None
        self._instrument_refdata: list[InstrumentRefdata] = None
        self._instrument_refdata_dict: dict[str, InstrumentRefdata] = None
        self._instrument_trading_configs: list[InstrumentTradingConfig] = None


    def validate_config(self,
                        oms_config_entry: OmsConfigEntry,
                        account_routes: list[OMSRouteEntry],
                        gw_configs: list[GwConfigEntry],
                        refdata: list[InstrumentRefdata],
                        trading_configs: list[InstrumentTradingConfig]):
        '''
        validate logic:
        1. account_routes: account_id to exch_account_id mapping should be unique
        2. for each account_id, the gw_key/gw_config should be valid
        3. No duplicate rpc/report/balance channel in gw_config
        4. managed accounts should be a subset of account_routes
        '''

        # 1. managed accounts should be unique
        if len(oms_config_entry.managed_account_ids) != len(set(oms_config_entry.managed_account_ids)):
            raise Exception("managed_account_ids should be unique")

        # 2. managed accounts should be a subset of account_routes
        account_ids = set([x.account_id for x in account_routes])
        managed_account_ids = set(oms_config_entry.managed_account_ids)
        if not managed_account_ids.issubset(account_ids):
            raise Exception("managed_account_ids should be a subset of account_routes")

        # 3. account_routes: account_id to exch_account_id mapping should be unique
        managed_exch_account_ids = set([x.exch_account_id for x in account_routes if x.account_id in managed_account_ids])
        if len(managed_account_ids) != len(managed_exch_account_ids):
            raise Exception("account_routes: account_id to exch_account_id mapping should be unique")

        # 4. for each account_id, the gw_key/gw_config should be valid
        gw_keys = set([x.gw_key for x in account_routes if x.account_id in managed_account_ids])
        gw_config_dict = {x.gw_key: x for x in gw_configs}
        for gw_key in gw_keys:
            if gw_key not in gw_config_dict:
                raise Exception(f"gw_key {gw_key} not found in gw_configs")

        # 5. No duplicate rpc/report/balance channel in gw_config
        rpc_channels = set()
        report_channels = set()
        balance_channels = set()
        for gw_config in gw_configs:
            if gw_config.rpc_endpoint in rpc_channels:
                raise Exception(f"duplicate rpc_channel {gw_config.rpc_endpoint}")
            if gw_config.report_endpoint in report_channels:
                raise Exception(f"duplicate report_channel {gw_config.report_endpoint}")
            if gw_config.balance_endpoint in balance_channels:
                raise Exception(f"duplicate balance_channel {gw_config.balance_endpoint}")
            rpc_channels.add(gw_config.rpc_endpoint)
            report_channels.add(gw_config.report_endpoint)
            balance_channels.add(gw_config.balance_endpoint)


    def reload_config(self,
                      oms_config_entry: OmsConfigEntry,
                      account_routes: list[OMSRouteEntry],
                      gw_configs: list[GwConfigEntry],
                      refdata: list[InstrumentRefdata],
                      trading_configs: list[InstrumentTradingConfig]):

        self.validate_config(oms_config_entry, account_routes, gw_configs, refdata, trading_configs)

        self._oms_managed_accounts = set(oms_config_entry.managed_account_ids)
        self._account_routes = [ x for x in account_routes if x.account_id in self._oms_managed_accounts ]
        _gw_keys = set([ x.gw_key for x in self._account_routes ])
        self._gw_configs_list = [x for x in gw_configs if x.gw_key in _gw_keys]
        self._gw_configs = {x.gw_key: x for x in gw_configs if x.gw_key in _gw_keys }
        self._instrument_refdata = [ref for ref in refdata if not ref.disabled]
        self._instrument_refdata_dict = {ref.instrument_id: ref for ref in self._instrument_refdata}
        self._instrument_trading_configs = trading_configs

        self._exch_name_to_gw_keys = {}
        for gw_key, gw_config in self._gw_configs.items():
            exch_name = gw_config.exch_name
            if exch_name not in self._exch_name_to_gw_keys:
                self._exch_name_to_gw_keys[exch_name] = set()
            self._exch_name_to_gw_keys[exch_name].add(gw_key)

        self._instrument_lookuptable_by_account_id = self.build_lookuptable_by_account_id(
            self._account_routes,
            self._gw_configs_list,
            self._instrument_refdata
        )
        self._instrument_lookuptable_by_gw_key = self.build_lookuptable_by_gw_key(
            self._gw_configs_list,
            self._instrument_refdata
        )


    def get_managed_accounts(self) -> set[int]:
        return self._oms_managed_accounts

    def get_account_routes(self) -> list[OMSRouteEntry]:
        return self._account_routes


    def get_gw_configs(self) -> list[GwConfigEntry]:
        return self._gw_configs_list


    def get_instrument_refdata(self) -> list[InstrumentRefdata]:
        return self._instrument_refdata


    def get_instrument_refdata_dict(self) -> dict[str, InstrumentRefdata]:
        return self._instrument_refdata_dict


    def get_instrument_trading_configs(self) -> list[InstrumentTradingConfig]:
        return self._instrument_trading_configs


    def get_gw_config_by_key(self, gw_key) -> GwConfigEntry:
        pass


    def get_gw_config_by_account_id(self, account_id) -> GwConfigEntry:
        pass


    def get_instrument_lookuptable_gw_key(self) -> dict[str, dict[str, InstrumentRefdata]]:
        return self._instrument_lookuptable_by_gw_key


    def get_instrument_lookuptable_account_id(self) -> dict[int, dict[str, InstrumentRefdata]]:
        return self._instrument_lookuptable_by_account_id


    @staticmethod
    def build_lookuptable_by_account_id(
        account_routes: list[OMSRouteEntry],
        gw_configs: list[GwConfigEntry],
        instrument_refdata: list[InstrumentRefdata]
    ) -> dict[int, dict[str, InstrumentRefdata]]:
        refdata_lookup: dict[int, dict[str, InstrumentRefdata]] = {}  # account_id -> instrument_code -> refdata
        gw_config_lookup = {x.gw_key: x for x in gw_configs}
        for ir in instrument_refdata:
            if ir.disabled:
                continue # skip disabled instruments
            for acc in account_routes:
                account_id = acc.account_id
                gw_key = acc.gw_key
                gw_config = gw_config_lookup[gw_key]
                if gw_config is None:
                    raise Exception(f"gw_config {gw_key} not found")
                if gw_config.exch_name == ir.exchange_name:
                    if account_id not in refdata_lookup:
                        refdata_lookup[account_id] = {}
                    refdata_lookup[account_id][ir.instrument_id_exchange] = ir

        return refdata_lookup


    @staticmethod
    def build_lookuptable_by_gw_key(
        gw_configs: list[GwConfigEntry],
        instrument_refdata: list[InstrumentRefdata]
    ) -> dict[str, dict[str, InstrumentRefdata]]:
        refdata_lookup: dict[str, dict[str, InstrumentRefdata]] = {}  # gw_key -> instrument_code -> refdata
        exch_name_to_gw_keys = {}
        for gw_config in gw_configs:
            exch_name = gw_config.exch_name
            if exch_name not in exch_name_to_gw_keys:
                exch_name_to_gw_keys[exch_name] = set()
            exch_name_to_gw_keys[exch_name].add(gw_config.gw_key)
        for ref in instrument_refdata:
            # if not ref.disabled:
            #     self.refdata[ref.instrument_code] = ref
            exch_name = ref.exchange_name
            gw_keys_for_exch = exch_name_to_gw_keys.get(exch_name, set())
            for gw_key in gw_keys_for_exch:
                if gw_key not in refdata_lookup:
                    refdata_lookup[gw_key] = {}
                refdata_lookup[gw_key][ref.instrument_id_exchange] = ref

        return refdata_lookup





