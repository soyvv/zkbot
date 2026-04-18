
import sys
from loguru import logger
from zk_proto_betterproto import rtmd, common, oms

import asyncio


from zk_client.tqclient import TQClient, create_client

async def test1():
    tq = await create_client(
        nats_url="nats://localhost:4222",
        account_ids=[901]
    )


    await tq.start()

    balances = await tq.get_account_balance(account_id=901)

    print(balances)


def _calc_qty(refdata: common.InstrumentRefData, price: float) -> float:
    # max(min_notial/price, min_qty)
    min_notional = refdata.min_notional
    min_qty = refdata.min_order_qty
    qty = max(min_notional / price, min_qty)

    # round to nearest step size
    step_size = refdata.qty_lot_size
    qty = round(qty / step_size) * step_size + step_size

    return qty

async def _wait_for_condition(condition, timeout_secs: int = 10) -> bool:
    _counter = 0
    while not condition():
        _counter += 1
        if _counter > timeout_secs:
            return True
        await asyncio.sleep(1)
    return False

async def RFB_test_order(account_id:int, instrument_id: str,
                         taker_order: bool=False, dry_run: bool = False):
    logger.info(f"creating TQClient for {account_id}...")
    tq = await create_client(
        nats_url="nats://localhost:4222",
        account_ids=[account_id],
        rtmd_channels=["tq.rtmd.binancedm.*"]
    )

    logger.info(f"Starting TQClient")
    await tq.start()

    balances = await tq.get_account_balance(account_id=account_id)
    logger.info(f"Account balances: {balances}")

    # check refdata for instrument
    refdata = tq.get_instrument_refdata(instrument_id)

    if refdata is None:
        logger.error(f"Refdata not found for instrument {instrument_id}")
        return
    else:
        logger.info(f"Refdata for instrument {instrument_id}: {refdata}")



    current_ticks: dict[str, rtmd.TickData] = {}
    orders: dict[int, oms.Order] = {}
    def on_rtmd_msg(msg: rtmd.TickData):
        if (instrument_id not in current_ticks and
                msg.instrument_code == refdata.instrument_id_exchange):
            logger.info(f"Received tick data: {msg.instrument_code}")
            current_ticks[instrument_id] = msg

    def on_order_update(msg: oms.OrderUpdateEvent):
        if instrument_id == msg .order_snapshot.instrument:
            logger.info(f"Received order update: {msg.to_json(indent=2)}")
            orders[msg.order_snapshot.order_id] = msg.order_snapshot

    await tq.subscribe_rtmd(on_rtmd_msg)
    await tq.subscribe_order_update(on_order_update)

    # wait tick tick data received
    logger.info(f"Waiting for tick data for {instrument_id}")
    await _wait_for_condition(lambda: instrument_id in current_ticks, timeout_secs=10)
    logger.info(f"Tick data received for {instrument_id}")

    if taker_order:
        buy_price = current_ticks[instrument_id] .sell_price_levels[0].price
    else:
        buy_price = current_ticks[instrument_id] .buy_price_levels[-1].price

    buy_qty = _calc_qty(refdata, buy_price)

    logger.info(f"Sending order for {buy_qty} at {buy_price}")
    if not dry_run:
        order_id = await tq.send_order(symbol=instrument_id,
                        side=common.BuySellType.BS_BUY,
                        qty=buy_qty, price=buy_price, account_id=account_id)
    else:
        order_id = None
        logger.info("Dry run, not sending order.")

    if not dry_run:
        # ensure booked
        timeout = await _wait_for_condition(lambda: order_id in orders, timeout_secs=10)
        if timeout:
            logger.error(f"Timeout waiting for order to be booked.")
            return

        if orders[order_id].order_status == oms.OrderStatus.ORDER_STATUS_BOOKED:
            logger.info("checking open orders...")
            open_orders = await tq.get_open_orders(account_id=account_id)
            if open_orders and len(open_orders) > 0:
                logger.info(f"Open orders: {open_orders}")
            else:
                logger.warning("No open orders.")

            logger.info("canceling order...")
            await tq.cancel(order_id, account_id=account_id)

        # wait till order reached terminal state
        timeout = await _wait_for_condition(lambda: orders[order_id].order_status in [
            oms.OrderStatus.ORDER_STATUS_CANCELLED,
            oms.OrderStatus.ORDER_STATUS_FILLED,
            oms.OrderStatus.ORDER_STATUS_REJECTED
        ], timeout_secs=10)
        if timeout:
            logger.error(f"Timeout waiting for order to reach terminal state.")
            return
        else:
            logger.info(f"Order reached terminal state: {orders[order_id].order_status}")

    logger.info("Test completed.")


# async def test2():
#     await RFB_test(901, "OMNI-P/USDT@BINANCEDM", dry_run=False)


if __name__ == "__main__":
    #asyncio.run(test1())
    asyncio.run(RFB_test_order(901, "OMNI-P/USDT@BINANCEDM",
                               taker_order=True, dry_run=False))