import copy
import unittest

from zk_oms.tests import oms_testutils
from zk_oms.tests.gw_test_utils import GwMessageHelper
import zk_datamodel.exch_gw as gw
import zk_datamodel.oms as oms

class VertexTest(unittest.TestCase):

    def setUp(self) -> None:
        self.oms_core = oms_testutils.get_default_test_omscore()
        self.gw_msg_helper = GwMessageHelper(gw_key="GW1", exch_account_id="TEST1")

    def tearDown(self) -> None:
        pass


    def test_immediate_cancel_IOC(self):
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

        linkage_report = self.gw_msg_helper.generate_linkage_report(
            exch_order_ref=exch_order_ref,
            client_order_id=order_req.order_id,
            ts=order_req.timestamp + 1
        )

        cancelled_report = self.gw_msg_helper.generate_orderstate_report(
            exch_order_ref=exch_order_ref,
            state=gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_CANCELLED,
            filled_qty=.0,
            unfilled_qty=20.0,
            qty=order_req.qty,
            ts=order_req.timestamp + 3
        )
        cancelled_report.order_report_entries[0].order_state_report.filled_qty = .0
        cancelled_report.order_report_entries[0].order_state_report.unfilled_qty = .0

        # reports come in with different order

        self.oms_core.process_order_report(linkage_report)

        actions_3 = self.oms_core.process_order_report(cancelled_report)
        self.assertEqual(2, len(actions_3))

        order_snapshot: oms.Order = copy.deepcopy(actions_3[0].action_data.order_snapshot)
        self.assertEqual(order_snapshot.order_status, oms.OrderStatus.ORDER_STATUS_CANCELLED)
        self.assertEqual(order_snapshot.filled_qty, .0)



    def test_IOC_partial_fill_then_cancelled(self):
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

        exch_order_ref = "test_order_2"

        linkage_report = self.gw_msg_helper.generate_linkage_report(
            exch_order_ref=exch_order_ref,
            client_order_id=order_req.order_id,
            ts=order_req.timestamp + 1
        )

        # from order_update; only unfilled_qty is provided
        partial_fill_report = self.gw_msg_helper.generate_orderstate_report(
            exch_order_ref=exch_order_ref,
            state=gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_PARTIAL_FILLED,
            filled_qty=15.0,
            unfilled_qty=5.0,
            qty=order_req.qty,
            ts=order_req.timestamp + 3
        )
        partial_fill_report.order_report_entries[0].order_state_report.filled_qty = .0

        cancelled_report = self.gw_msg_helper.generate_orderstate_report(
            exch_order_ref=exch_order_ref,
            state=gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_CANCELLED,
            filled_qty=15.0,
            unfilled_qty=5.0,
            qty=order_req.qty,
            ts=order_req.timestamp + 3
        )
        cancelled_report.order_report_entries[0].order_state_report.filled_qty = .0
        cancelled_report.order_report_entries[0].order_state_report.unfilled_qty = .0

        trade_report = self.gw_msg_helper.generate_trade_report(
            filled_price=1620.0,
            filled_qty=15.0,
            exch_order_ref=exch_order_ref,
            ts=order_req.timestamp + 2
        )


        self.oms_core.process_order_report(linkage_report)
        actions1 = self.oms_core.process_order_report(partial_fill_report)
        order_snapshot1: oms.Order = copy.deepcopy(actions1[0].action_data.order_snapshot)

        actions2 = self.oms_core.process_order_report(cancelled_report)
        order_snapshot2: oms.Order = copy.deepcopy(actions2[0].action_data.order_snapshot)

        actions3 = self.oms_core.process_order_report(trade_report)
        order_snapshot3: oms.Order = copy.deepcopy(actions3[0].action_data.order_snapshot)

        self.assertEqual(order_snapshot1.order_status, oms.OrderStatus.ORDER_STATUS_PARTIALLY_FILLED)
        self.assertEqual(order_snapshot1.filled_qty, 15.0)

        self.assertEqual(order_snapshot2.order_status, oms.OrderStatus.ORDER_STATUS_CANCELLED)
        self.assertEqual(order_snapshot2.filled_qty, 15.0)

        self.assertEqual(order_snapshot3.order_status, oms.OrderStatus.ORDER_STATUS_CANCELLED)
        self.assertEqual(order_snapshot3.filled_qty, 15.0)
        self.assertNotEqual(order_snapshot3.filled_avg_price, .0)


    def test_partial_fill_then_filled(self):
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

        exch_order_ref = "test_order_3"

        linkage_report = self.gw_msg_helper.generate_linkage_report(
            exch_order_ref=exch_order_ref,
            client_order_id=order_req.order_id,
            ts=order_req.timestamp + 1
        )

        # from order_update; only unfilled_qty is provided
        partial_fill_report = self.gw_msg_helper.generate_orderstate_report(
            exch_order_ref=exch_order_ref,
            state=gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_PARTIAL_FILLED,
            filled_qty=15.0,
            unfilled_qty=5.0,
            qty=order_req.qty,
            ts=order_req.timestamp + 2
        )
        partial_fill_report.order_report_entries[0].order_state_report.filled_qty = .0


        trade_report1 = self.gw_msg_helper.generate_trade_report(
            filled_price=1620.0,
            filled_qty=15.0,
            exch_order_ref=exch_order_ref,
            ts=order_req.timestamp + 2
        )

        fill_report = self.gw_msg_helper.generate_orderstate_report(
            exch_order_ref=exch_order_ref,
            state=gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_FILLED,
            filled_qty=20.0,
            unfilled_qty=.0,
            qty=order_req.qty,
            ts=order_req.timestamp + 3
        )

        trade_report2 = self.gw_msg_helper.generate_trade_report(
            filled_price=1620.0,
            filled_qty=15.0,
            exch_order_ref=exch_order_ref,
            ts=order_req.timestamp + 2,
            gw_order_report=fill_report
        )

        self.oms_core.process_order_report(linkage_report)
        actions1 = self.oms_core.process_order_report(partial_fill_report)
        order_snapshot1: oms.Order = copy.deepcopy(actions1[0].action_data.order_snapshot)

        actions2 = self.oms_core.process_order_report(trade_report1)
        order_snapshot2: oms.Order = copy.deepcopy(actions2[0].action_data.order_snapshot)

        actions3 = self.oms_core.process_order_report(trade_report2)
        order_snapshot3: oms.Order = copy.deepcopy(actions3[0].action_data.order_snapshot)

        self.assertEqual(order_snapshot1.order_status, oms.OrderStatus.ORDER_STATUS_PARTIALLY_FILLED)
        self.assertEqual(order_snapshot1.filled_qty, 15.0)

        self.assertEqual(order_snapshot2.order_status, oms.OrderStatus.ORDER_STATUS_PARTIALLY_FILLED)
        self.assertEqual(order_snapshot2.filled_qty, 15.0)

        self.assertEqual(order_snapshot3.order_status, oms.OrderStatus.ORDER_STATUS_FILLED)
        self.assertEqual(order_snapshot3.filled_qty, 20.0)
        self.assertNotEqual(order_snapshot3.filled_avg_price, .0)