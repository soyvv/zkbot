import unittest

from zk_oms.tests import oms_testutils
from zk_oms.tests.gw_test_utils import GwMessageHelper
import zk_datamodel.exch_gw as gw
import zk_datamodel.oms as oms
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



    def test_spot_balacne_calc_place_cancel(self):
        order_req1 = oms_testutils.generate_oms_order_request(
            account=101,
            symbol='ETH/USD@EX2',
            side='buy',
            qty=0.2,
            price=1620.0
        )

        order_req2 = oms_testutils.generate_oms_order_request(
            account=101,
            symbol='ETH/USD@EX2',
            side='sell',
            qty=0.2,
            price=1625.0
        )

        initial_balances = self.oms_core.get_account_balance(account_id=101, use_exch_data=False)

        init_balance1 = initial_balances[0].position_state.avail_qty
        init_balance2 = initial_balances[1].position_state.avail_qty

        actions_1_1 = self.oms_core.process_order(order_req1)
        print(actions_1_1)

        assert_that(actions_1_1).is_length(3)
        oms_testutils.assert_actions_has_type(actions_1_1, oms_models.OMSActionType.SEND_ORDER_TO_GW)
        oms_testutils.assert_actions_has_type(actions_1_1, oms_models.OMSActionType.UPDATE_BALANCE)
        oms_testutils.assert_actions_has_type(actions_1_1, oms_models.OMSActionType.PERSIST_ORDER)

        actions_1_2 = self.oms_core.process_order(order_req2)
        print(actions_1_2)
        oms_testutils.assert_actions_has_type(actions_1_2, oms_models.OMSActionType.SEND_ORDER_TO_GW)
        oms_testutils.assert_actions_has_type(actions_1_2, oms_models.OMSActionType.UPDATE_BALANCE)
        oms_testutils.assert_actions_has_type(actions_1_2, oms_models.OMSActionType.PERSIST_ORDER)

        position_updates: oms.PositionUpdateEvent = oms_testutils .retrieve_positionupdate_event(actions_1_1)
        assert_that(position_updates.position_snapshots).is_length(2)



        # generate reports
        exch_order_ref1 = "test_order_1"
        linkage_report1 = self.gw_msg_helper.generate_linkage_report(
            exch_order_ref=exch_order_ref1,
            client_order_id=order_req1.order_id,
            ts=order_req1.timestamp + 1
        )

        cancel_report1 = self.gw_msg_helper.generate_orderstate_report(
            exch_order_ref=exch_order_ref1,
            state=gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_CANCELLED,
            filled_qty=.0,
            unfilled_qty=.2,
            qty=order_req1.qty,
            ts=order_req1.timestamp + 3
        )

        exch_order_ref2 = "test_order_2"
        linkage_report2 = self.gw_msg_helper.generate_linkage_report(
            exch_order_ref=exch_order_ref2,
            client_order_id=order_req2.order_id,
            ts=order_req2.timestamp + 1
        )

        cancel_req1 = oms_testutils.generate_oms_cancel_request(order_id=order_req1.order_id)
        cancel_req2 = oms_testutils.generate_oms_cancel_request(order_id=order_req2.order_id)

        cancel_report2 = self.gw_msg_helper.generate_orderstate_report(
            exch_order_ref=exch_order_ref2,
            state=gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_CANCELLED,
            filled_qty=.0,
            unfilled_qty=.2,
            qty=order_req2.qty,
            ts=order_req2.timestamp + 3
        )


        # send linkage reports
        actions_2_1 = self.oms_core.process_order_report(linkage_report1)
        print(actions_2_1)
        actions_2_2 = self.oms_core.process_order_report(linkage_report2)
        print(actions_2_2)

        # send cancel requests
        actions_3_1 = self.oms_core.process_cancel(cancel_req1)
        print(actions_3_1)
        actions_3_2 = self.oms_core.process_cancel(cancel_req2)
        print(actions_3_2)

        # send cancel reports
        actions_4_1 = self.oms_core.process_order_report(cancel_report1)
        print(actions_4_1)
        actions_4_2 = self.oms_core.process_order_report(cancel_report2)
        print(actions_4_2)

        # send cancel reports again
        actions_5_1 = self.oms_core.process_order_report(cancel_report1)
        print(actions_4_1)
        actions_5_2 = self.oms_core.process_order_report(cancel_report2)
        print(actions_4_2)



        final_balances = self.oms_core.get_account_balance(account_id=101, use_exch_data=False)
        print(final_balances)

        final_balance1 = final_balances[0].position_state.avail_qty
        final_balance2 = final_balances[1].position_state.avail_qty

        assert_that(final_balance1).is_equal_to(init_balance1)
        assert_that(final_balance2).is_equal_to(init_balance2)


    def test_spot_balance_with_trade(self):
        order_req1 = oms_testutils.generate_oms_order_request(
            account=101,
            symbol='ETH/USD@EX2',
            side='buy',
            qty=0.2,
            price=1620.0
        )

        order_req2 = oms_testutils.generate_oms_order_request(
            account=101,
            symbol='ETH/USD@EX2',
            side='sell',
            qty=0.2,
            price=1625.0
        )

        actions_1_1 = self.oms_core.process_order(order_req1)
        print(actions_1_1)

        actions_1_2 = self.oms_core.process_order(order_req2)
        print(actions_1_2)

        # generate reports
        exch_order_ref1 = "test_order_3"
        linkage_report1 = self.gw_msg_helper.generate_linkage_report(
            exch_order_ref=exch_order_ref1,
            client_order_id=order_req1.order_id,
            ts=order_req1.timestamp + 1
        )

        trade_report1 = self.gw_msg_helper.generate_trade_report(
            filled_price=1620.0,
            filled_qty=0.2,
            exch_order_ref=exch_order_ref1,
            ts=order_req1.timestamp + 2
        )

        filled_state_report1 = self.gw_msg_helper.generate_orderstate_report(
            exch_order_ref=exch_order_ref1,
            order_id=order_req1.order_id,
            state=gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_FILLED,
            filled_qty=0.2,
            unfilled_qty=.0,
            qty=0.2
        )

        exch_order_ref2 = "test_order_4"
        linkage_report2 = self.gw_msg_helper.generate_linkage_report(
            exch_order_ref=exch_order_ref2,
            client_order_id=order_req2.order_id,
            ts=order_req2.timestamp + 1
        )

        trade_report2 = self.gw_msg_helper.generate_trade_report(
            filled_price=1625.0,
            filled_qty=0.2,
            exch_order_ref=exch_order_ref2,
            ts=order_req1.timestamp + 2
        )

        filled_state_report2 = self.gw_msg_helper.generate_orderstate_report(
            exch_order_ref=exch_order_ref2,
            order_id=order_req2.order_id,
            state=gw.ExchangeOrderStatus.EXCH_ORDER_STATUS_FILLED,
            filled_qty=0.2,
            unfilled_qty=.0,
            qty=0.2,
            gw_order_report=trade_report2
        )


        # send linkage reports
        actions_2_1 = self.oms_core.process_order_report(linkage_report1)
        assert_that(oms_testutils.retrieve_orderupdate_event(actions_2_1)).is_not_none()
        actions_2_2 = self.oms_core.process_order_report(linkage_report2)
        print(oms_testutils.retrieve_orderupdate_event(actions_2_2))


        # send trade reports
        actions_3_1 = self.oms_core.process_order_report(trade_report1)
        print(actions_3_1)
        actions_3_2 = self.oms_core.process_order_report(filled_state_report1)
        print(actions_3_2)
        actions_3_3 = self.oms_core.process_order_report(filled_state_report2)
        print(actions_3_3)

        # duplicate state report for order 1
        actions_3_4 = self.oms_core.process_order_report(filled_state_report1)
        print(actions_3_4)


        final_balances = self.oms_core.get_account_balance(account_id=101, use_exch_data=False)

        print(final_balances)



    def test_spot_balance_with_partialfill_and_cancel(self):
        pass



    def test_spot_balance_with_rejection(self):
        pass

