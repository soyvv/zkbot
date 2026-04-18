import copy
import unittest
import time

from zk_proto_betterproto import common
from zk_oms.tests import oms_testutils
from zk_oms.tests.gw_test_utils import GwMessageHelper
import zk_proto_betterproto.exch_gw as gw
import zk_proto_betterproto.oms as oms
import zk_oms.core.models as oms_models

class NonTQOrderReports(unittest.TestCase):

    def setUp(self) -> None:
        self.oms_core = oms_testutils.get_nontq_test_omscore()
        self.gw_msg_helper = GwMessageHelper(gw_key="GW1", exch_account_id="TEST1")



    def tearDown(self) -> None:
        pass


    def test_non_tq_trade_marked_by_gw(self):

        ts = int(time.time() * 1000)

        exch_order_ref = "test_order_1"

        booked_report = self.gw_msg_helper.generate_orderstate_report(
            exch_order_ref=exch_order_ref,
            order_id=None,
            state=gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_BOOKED,
            filled_qty=.0,
            unfilled_qty=20.0,
            qty=20.0,
            ts=ts + 3
        )
        booked_report.order_source_type = gw.OrderSourceType.ORDER_SOURCE_NON_TQ


        actions = self.oms_core.process_order_report(booked_report)

        self.assertEqual(2, len(actions))

        order_snapshot: oms.Order = copy.deepcopy(actions[0].action_data.order_snapshot)
        self.assertEqual(order_snapshot.order_status, oms.OrderStatus.ORDER_STATUS_BOOKED)
        self.assertIsNotNone(order_snapshot.exch_order_ref)



    def test_non_tq_trade_marked_by_gw2(self):

        ts = int(time.time() * 1000)

        exch_order_ref = "test_order_2"

        filled_report = self.gw_msg_helper.generate_orderstate_report(
            exch_order_ref=exch_order_ref,
            order_id=None,
            state=gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_FILLED,
            filled_qty=20.0,
            unfilled_qty=.0,
            qty=20.0,
            ts=ts + 3
        )
        filled_report.order_source_type = gw.OrderSourceType.ORDER_SOURCE_NON_TQ

        trade_report = self.gw_msg_helper.generate_trade_report(
            filled_price=1625.0, filled_qty=20.0, exch_order_ref=exch_order_ref, ts=ts + 4, gw_order_report=filled_report)

        actions = self.oms_core.process_order_report(trade_report)

        self.assertEqual(2, len(actions))

        order_snapshot: oms.Order = copy.deepcopy(actions[0].action_data.order_snapshot)
        self.assertEqual(order_snapshot.order_status, oms.OrderStatus.ORDER_STATUS_FILLED)
        self.assertIsNotNone(order_snapshot.exch_order_ref)
        self.assertEqual(20.0, order_snapshot.filled_qty)
        self.assertIsNotNone(order_snapshot.order_id)


    def test_non_tq_trade_not_marked_by_gw(self):
        ts = int(time.time() * 1000)

        exch_order_ref = "test_order_3"

        filled_report = self.gw_msg_helper.generate_orderstate_report(
            exch_order_ref=exch_order_ref,
            order_id=None,
            state=gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_FILLED,
            filled_qty=20.0,
            unfilled_qty=.0,
            qty=20.0,
            ts=ts + 3
        )
        filled_report.order_source_type = gw.OrderSourceType.ORDER_SOURCE_UNKNOWN

        trade_report = self.gw_msg_helper.generate_trade_report(
            filled_price=1625.0, filled_qty=20.0, exch_order_ref=exch_order_ref, ts=ts + 4,
            gw_order_report=filled_report)

        actions = self.oms_core.process_order_report(trade_report)

        self.assertEqual(0, len(actions))

        # second report with non-TQ source marked
        booked_report = self.gw_msg_helper.generate_orderstate_report(
            exch_order_ref=exch_order_ref,
            order_id=None,
            state=gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_BOOKED,
            filled_qty=.0,
            unfilled_qty=20.0,
            qty=20.0,
            ts=ts + 3
        )
        booked_report.order_source_type = gw.OrderSourceType.ORDER_SOURCE_NON_TQ

        actions = self.oms_core.process_order_report(booked_report)

        self.assertEqual(4, len(actions))

        order_snapshot: oms.Order = copy.deepcopy(actions[2].action_data.order_snapshot)
        self.assertEqual(order_snapshot.order_status, oms.OrderStatus.ORDER_STATUS_FILLED)
        self.assertIsNotNone(order_snapshot.exch_order_ref)
        self.assertEqual(20.0, order_snapshot.filled_qty)
        self.assertIsNotNone(order_snapshot.order_id)



    def test_cleanup_stale_non_tq_order(self):
        ts = int(time.time() * 1000)

        exch_order_ref = "test_order_4"

        filled_report = self.gw_msg_helper.generate_orderstate_report(
            exch_order_ref=exch_order_ref,
            order_id=None,
            state=gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_FILLED,
            filled_qty=20.0,
            unfilled_qty=.0,
            qty=20.0,
            ts=ts + 3
        )
        filled_report.order_source_type = gw.OrderSourceType.ORDER_SOURCE_UNKNOWN

        actions = self.oms_core.process_order_report(filled_report)

        self.assertEqual(0, len(actions))

        booked_report = self.gw_msg_helper.generate_orderstate_report(
            exch_order_ref=exch_order_ref,
            order_id=None,
            state=gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_BOOKED,
            filled_qty=.0,
            unfilled_qty=20.0,
            qty=20.0,
            ts=ts + 4
        )
        booked_report.order_source_type = gw.OrderSourceType.ORDER_SOURCE_UNKNOWN

        actions = self.oms_core.process_order_report(booked_report)

        self.assertEqual(0, len(actions))

        ts += 60 * 1000 * 11 # 11 minutes later
        actions = self.oms_core.process_cleanup(ts)

        self.assertEqual(4, len(actions))

        order_snapshot: oms.Order = copy.deepcopy(actions[2].action_data.order_snapshot)
        self.assertEqual(order_snapshot.order_status, oms.OrderStatus.ORDER_STATUS_FILLED)
        self.assertIsNotNone(order_snapshot.exch_order_ref)
        self.assertEqual(20.0, order_snapshot.filled_qty)
        self.assertIsNotNone(order_snapshot.order_id)

    def test_cancel_non_tq_order(self):
        ts = int(time.time() * 1000)

        exch_order_ref = "test_order_5"

        booked_report = self.gw_msg_helper.generate_orderstate_report(
            exch_order_ref=exch_order_ref,
            order_id=None,
            state=gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_BOOKED,
            filled_qty=.0,
            unfilled_qty=20.0,
            qty=20.0,
            ts=ts + 3
        )
        booked_report.order_source_type = gw.OrderSourceType.ORDER_SOURCE_NON_TQ

        booked_report.order_details = gw.OrderInfo(
            instrument="ETH",
            buy_sell_type=common.BuySellType.BS_BUY
        )

        actions = self.oms_core.process_order_report(booked_report)

        self.assertEqual(2, len(actions))

        order_snapshot: oms.Order = copy.deepcopy(actions[0].action_data.order_snapshot)
        self.assertEqual(order_snapshot.order_status, oms.OrderStatus.ORDER_STATUS_BOOKED)
        self.assertIsNotNone(order_snapshot.exch_order_ref)


        cancel_request = oms_testutils.generate_oms_cancel_request(order_snapshot.order_id)
        actions = self.oms_core.process_cancel(cancel_request)

        oms_testutils.assert_actions_has_type(actions, oms_models.OMSActionType.SEND_CANCEL_TO_GW)

