
import unittest

import betterproto

from zk_oms.tests import oms_testutils
from zk_oms.tests.gw_test_utils import GwMessageHelper
import zk_proto_betterproto.exch_gw as gw
import zk_proto_betterproto.oms as oms

class OMSInferredTradeTest(unittest.TestCase):

    def setUp(self) -> None:
        self.oms_core = oms_testutils.get_default_test_omscore()
        self.gw_msg_helper = GwMessageHelper(gw_key="GW1", exch_account_id="TEST1")

    def tearDown(self) -> None:
        pass




    def test_orderupdate_before_trades(self):
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

        trade_report1 = self.gw_msg_helper.generate_trade_report(
            filled_price=1620.0,
            filled_qty=5.0,
            exch_order_ref=exch_order_ref,
            ts=order_req.timestamp + 2
        )

        trade_report2 = self.gw_msg_helper.generate_trade_report(
            filled_price=1620.0,
            filled_qty=11.0,
            exch_order_ref=exch_order_ref,
            ts=order_req.timestamp + 2
        )

        trade_report3 = self.gw_msg_helper.generate_trade_report(
            filled_price=1620.0,
            filled_qty=4.0,
            exch_order_ref=exch_order_ref,
            ts=order_req.timestamp + 2
        )

        partial_fill_report = self.gw_msg_helper.generate_orderstate_report(
            exch_order_ref=exch_order_ref,
            state=gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_PARTIAL_FILLED,
            filled_qty=16.0,
            unfilled_qty=4.0,

            qty=order_req.qty,
            ts=order_req.timestamp + 3
        )

        filled_report = self.gw_msg_helper.generate_orderstate_report(
            exch_order_ref=exch_order_ref,
            state=gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_FILLED,
            filled_qty=20.0,
            unfilled_qty=.0,
            qty=order_req.qty,
            ts=order_req.timestamp + 3
        )

        self.oms_core.process_order_report(linkage_report)

        actions1 = self.oms_core.process_order_report(partial_fill_report)
        actions2 = self.oms_core.process_order_report(filled_report)

        self.oms_core.process_order_report(trade_report1)
        self.oms_core.process_order_report(trade_report2)
        actions3 = self.oms_core.process_order_report(trade_report3)


        print("actions1:" + str(actions1))
        print("actions2:" + str(actions2))

        self.assertIsNotNone(actions1[0].action_data.order_inferred_trade)
        self.assertTrue(betterproto.serialized_on_wire(actions1[0].action_data.order_inferred_trade))
        self.assertTrue(betterproto.serialized_on_wire(actions2[0].action_data.order_inferred_trade))
        self.assertFalse(betterproto.serialized_on_wire(actions3[0].action_data.order_inferred_trade))


    def test_trades_before_orderupdate(self):
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

        trade_report1 = self.gw_msg_helper.generate_trade_report(
            filled_price=1620.0,
            filled_qty=5.0,
            exch_order_ref=exch_order_ref,
            ts=order_req.timestamp + 2
        )

        trade_report2 = self.gw_msg_helper.generate_trade_report(
            filled_price=1620.0,
            filled_qty=11.0,
            exch_order_ref=exch_order_ref,
            ts=order_req.timestamp + 2
        )

        trade_report3 = self.gw_msg_helper.generate_trade_report(
            filled_price=1620.0,
            filled_qty=4.0,
            exch_order_ref=exch_order_ref,
            ts=order_req.timestamp + 2
        )

        partial_fill_report = self.gw_msg_helper.generate_orderstate_report(
            exch_order_ref=exch_order_ref,
            state=gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_PARTIAL_FILLED,
            filled_qty=16.0,
            unfilled_qty=4.0,

            qty=order_req.qty,
            ts=order_req.timestamp + 3
        )

        filled_report = self.gw_msg_helper.generate_orderstate_report(
            exch_order_ref=exch_order_ref,
            state=gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_FILLED,
            filled_qty=20.0,
            unfilled_qty=.0,
            qty=order_req.qty,
            ts=order_req.timestamp + 3
        )

        self.oms_core.process_order_report(linkage_report)



        actions1 = self.oms_core.process_order_report(trade_report1)
        oue = oms_testutils.retrieve_orderupdate_event(actions1)
        self.assertTrue(betterproto.serialized_on_wire(oue.order_inferred_trade))
        self.assertEqual(oue.order_snapshot.order_status, oms.OrderStatus.ORDER_STATUS_PARTIALLY_FILLED)

        actions2 = self.oms_core.process_order_report(trade_report2)
        oue = oms_testutils.retrieve_orderupdate_event(actions2)
        self.assertTrue(betterproto.serialized_on_wire(oue.order_inferred_trade))
        self.assertEqual(oue.order_snapshot.order_status, oms.OrderStatus.ORDER_STATUS_PARTIALLY_FILLED)

        actions3 = self.oms_core.process_order_report(trade_report3)
        oue = oms_testutils.retrieve_orderupdate_event(actions3)
        self.assertTrue(betterproto.serialized_on_wire(oue.order_inferred_trade))
        self.assertEqual(oue.order_snapshot.order_status, oms.OrderStatus.ORDER_STATUS_FILLED)

        actions4 = self.oms_core.process_order_report(partial_fill_report)
        oue = oms_testutils.retrieve_orderupdate_event(actions4)
        self.assertFalse(betterproto.serialized_on_wire(oue.order_inferred_trade))
        self.assertEqual(oue.order_snapshot.order_status, oms.OrderStatus.ORDER_STATUS_FILLED)

        actions5 = self.oms_core.process_order_report(filled_report)
        oue = oms_testutils.retrieve_orderupdate_event(actions5)
        self.assertFalse(betterproto.serialized_on_wire(oue.order_inferred_trade))
        self.assertEqual(oue.order_snapshot.order_status, oms.OrderStatus.ORDER_STATUS_FILLED)


    def test_missed_trades(self):
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

        trade_report1 = self.gw_msg_helper.generate_trade_report(
            filled_price=1620.0,
            filled_qty=5.0,
            exch_order_ref=exch_order_ref,
            ts=order_req.timestamp + 2
        )

        partial_fill_report = self.gw_msg_helper.generate_orderstate_report(
            exch_order_ref=exch_order_ref,
            state=gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_PARTIAL_FILLED,
            filled_qty=16.0,
            unfilled_qty=4.0,

            qty=order_req.qty,
            ts=order_req.timestamp + 3
        )

        filled_report = self.gw_msg_helper.generate_orderstate_report(
            exch_order_ref=exch_order_ref,
            state=gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_FILLED,
            filled_qty=20.0,
            unfilled_qty=.0,
            qty=order_req.qty,
            ts=order_req.timestamp + 3
        )

        self.oms_core.process_order_report(linkage_report)

        actions1 = self.oms_core.process_order_report(trade_report1)
        oue = oms_testutils.retrieve_orderupdate_event(actions1)
        self.assertTrue(betterproto.serialized_on_wire(oue.order_inferred_trade))
        self.assertEqual(oue.order_snapshot.order_status, oms.OrderStatus.ORDER_STATUS_PARTIALLY_FILLED)
        self.assertEqual(oue.order_snapshot.filled_qty, 5.0)
        self.assertEqual(oue.order_inferred_trade.filled_qty, 5.0)


        actions4 = self.oms_core.process_order_report(partial_fill_report)
        oue = oms_testutils.retrieve_orderupdate_event(actions4)
        self.assertTrue(betterproto.serialized_on_wire(oue.order_inferred_trade))
        self.assertEqual(oue.order_snapshot.order_status, oms.OrderStatus.ORDER_STATUS_PARTIALLY_FILLED)
        self.assertEqual(oue.order_snapshot.filled_qty, 16.0)
        self.assertEqual(oue.order_inferred_trade.filled_qty, 11.0)

        actions5 = self.oms_core.process_order_report(filled_report)
        oue = oms_testutils.retrieve_orderupdate_event(actions5)
        self.assertTrue(betterproto.serialized_on_wire(oue.order_inferred_trade))
        self.assertEqual(oue.order_snapshot.order_status, oms.OrderStatus.ORDER_STATUS_FILLED)
        self.assertEqual(oue.order_snapshot.filled_qty, 20.0)
        self.assertEqual(oue.order_inferred_trade.filled_qty, 4.0)








