import unittest
import time

from zk_oms.tests import oms_testutils
from zk_oms.tests.gw_test_utils import GwMessageHelper
import zk_proto_betterproto.exch_gw as gw
import zk_proto_betterproto.oms as oms
import zk_proto_betterproto.common as common
import zk_oms.core.models as oms_models

from assertpy import assert_that

class OMSSpotTradingBalanceTest(unittest.TestCase):

    def setUp(self) -> None:
        self.oms_core = oms_testutils.get_spot_trading_omscore(
            init_positions={
                101: {
                    "ETH": 5.0,
                    "USD": 10000.0
                }
            }
        )
        self.gw_msg_helper = GwMessageHelper(gw_key="GW2", exch_account_id="TEST2")

    def tearDown(self) -> None:
        pass


    def test_balance_updates(self):
        init_balance_update = gw.BalanceUpdate(
            balances=[
                gw.PositionReport(
                    instrument_code="ETH",
                    instrument_type=common.InstrumentType.INST_TYPE_SPOT,
                    long_short_type=common.LongShortType.LS_LONG,
                    exch_account_code="TEST2",
                    qty=5.1,
                    avail_qty=5.1,
                    update_timestamp=int(time.time() * 1000)
                )
            ]
        )

        actions = self.oms_core.process_balance_update(init_balance_update)

        assert_that(actions).is_length(1)