import unittest

from zk_oms.tests import oms_testutils
from zk_oms.tests.gw_test_utils import GwMessageHelper
import zk_proto_betterproto.exch_gw as gw
import zk_proto_betterproto.oms as oms

class OMSOutOfOrderReportTest(unittest.TestCase):

    def setUp(self) -> None:
        self.oms_core = oms_testutils.get_default_test_omscore()
        self.gw_msg_helper = GwMessageHelper(gw_key="GW1", exch_account_id="TEST1")

    def tearDown(self) -> None:
        pass


    def test_place_order(self):
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



        partial_fill_report = self.gw_msg_helper.generate_orderstate_report(
            exch_order_ref=exch_order_ref,
            state=gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_PARTIAL_FILLED,
            filled_qty=0.1,
            unfilled_qty=.1,
            qty=order_req.qty,
            ts=order_req.timestamp + 3
        )

        trade_report = self.gw_msg_helper.generate_trade_report(
            filled_price=1620.0,
            filled_qty=0.1,
            exch_order_ref=exch_order_ref,
            ts=order_req.timestamp + 2,
            gw_order_report=partial_fill_report
        )

        state_report2 = self.gw_msg_helper.generate_orderstate_report(
            exch_order_ref=exch_order_ref,
            state=gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_CANCELLED,
            filled_qty=0.1,
            unfilled_qty=.1,
            qty=order_req.qty,
            ts=order_req.timestamp + 3
        )

        # reports come in with different order

        actions_2 = self.oms_core.process_order_report(linkage_report)
        print(actions_2)

        actions_3 = self.oms_core.process_cancel(cancel_req)
        self.assertEqual(1, len(actions_3))

        # cancel report comes in before trade report
        actions_4 = self.oms_core.process_order_report(state_report2)
        print(actions_4)

        actions_5 = self.oms_core.process_order_report(trade_report)
        print(actions_5)
        #self.assertEqual(len(actions_5), 0)


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
        self.oms_core.process_order_report(trade_report1)
        self.oms_core.process_order_report(trade_report2)
        actions1 = self.oms_core.process_order_report(trade_report3)


        self.assertEqual(actions1[0].action_data.order_snapshot.order_status,
                         oms.OrderStatus.ORDER_STATUS_FILLED)
        self.assertNotEqual(actions1[0].action_data.order_snapshot.filled_avg_price, .0)

        self.oms_core.process_order_report(partial_fill_report)
        actions2 = self.oms_core.process_order_report(filled_report)

        self.assertEqual(actions2[0].action_data.order_snapshot.order_status,
                         oms.OrderStatus.ORDER_STATUS_FILLED)


        

        

