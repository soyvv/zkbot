import unittest

from zk_oms.tests import oms_testutils
from zk_oms.tests.gw_test_utils import GwMessageHelper
import zk_datamodel.exch_gw as gw
import zk_datamodel.oms as oms

class OMSCancelTest(unittest.TestCase):

    def setUp(self) -> None:
        self.oms_core = oms_testutils.get_default_test_omscore()
        self.gw_msg_helper = GwMessageHelper(gw_key="GW1", exch_account_id="TEST1")

    def tearDown(self) -> None:
        pass


    def test_cancel_order_in_terminal_state(self):
        order_req = oms_testutils.generate_oms_order_request(
            account=100,
            symbol='ETH-P/USDC@EX1',
            side='buy',
            qty=0.2,
            price=1620.0
        )
        actions = self.oms_core.process_order(order_req)
        print(actions)

        self.assertEqual(2, len(actions))

        exch_order_ref = "test_order_1"

        linkage_report = self.gw_msg_helper.generate_linkage_report(
            exch_order_ref=exch_order_ref,
            client_order_id=order_req.order_id,
            ts=order_req.timestamp + 1
        )


        cancel_req = oms_testutils.generate_oms_cancel_request(order_id=order_req.order_id)


        state_report2 = self.gw_msg_helper.generate_orderstate_report(
            exch_order_ref=exch_order_ref,
            state=gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_CANCELLED,
            filled_qty=0.0,
            unfilled_qty=.2,
            qty=order_req.qty,
            ts=order_req.timestamp + 3
        )


        actions_2 = self.oms_core.process_order_report(linkage_report)
        print(actions_2)

        actions_3 = self.oms_core.process_cancel(cancel_req)
        self.assertEqual(1, len(actions_3))

        actions_4 = self.oms_core.process_order_report(state_report2)
        print(actions_4)


        # cancel again
        actions_5 = self.oms_core.process_cancel(cancel_req)
        self.assertEqual(1, len(actions_5))  # should send update event

        print(actions_5)
