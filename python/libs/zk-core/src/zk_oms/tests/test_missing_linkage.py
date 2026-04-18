import copy
import unittest

from zk_oms.tests import oms_testutils
from zk_oms.tests.gw_test_utils import GwMessageHelper
import zk_proto_betterproto.exch_gw as gw
import zk_proto_betterproto.oms as oms

class MissingLinkageTest(unittest.TestCase):

    def setUp(self) -> None:
        self.oms_core = oms_testutils.get_default_test_omscore()
        self.gw_msg_helper = GwMessageHelper(gw_key="GW1", exch_account_id="TEST1")

    def tearDown(self) -> None:
        pass


    def test_missing_linkage(self):
        order_req = oms_testutils.generate_oms_order_request(
            account=100,
            symbol='ETH-P/USDC@EX1',
            side='buy',
            qty=20.0,
            price=1620.0
        )
        actions = self.oms_core.process_order(order_req)
        print(actions)

        self.assertEqual(2, len(actions))

        exch_order_ref = "test_order_1"

        # no linkage report; only booked report
        booked_report = self.gw_msg_helper.generate_orderstate_report(
            exch_order_ref=exch_order_ref,
            order_id=order_req.order_id,
            state=gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_BOOKED,
            filled_qty=.0,
            unfilled_qty=20.0,
            qty=order_req.qty,
            ts=order_req.timestamp + 3
        )


        actions = self.oms_core.process_order_report(booked_report)
        self.assertEqual(2, len(actions))

        order_snapshot: oms.Order = copy.deepcopy(actions[0].action_data.order_snapshot)
        self.assertEqual(order_snapshot.order_status, oms.OrderStatus.ORDER_STATUS_BOOKED)
        self.assertIsNotNone(order_snapshot.exch_order_ref)
