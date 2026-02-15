

try:
    from .zk_client import TQClient, create_client
except ImportError:
    from zk_client import TQClient, create_client
from zk_datamodel import common

async def run():

    import os

    os.environ["TQ_ENV"] = "UAT"

    tq_client: TQClient = await create_client(
        nats_url="nats://localhost:4222",
        api_root_url="http://localhost:5013",
        #account_ids=[901, 225, 21002, 21003, 6001, 6002],
        account_ids=[901]
    )


    await tq_client.start()


    print(f"instance_id={tq_client.id_gen}")


    # balances = await tq_client.get_account_balance(account_id=901)
    # print(balances)
    #
    #
    # open_orders = await tq_client.get_open_orders(account_id=901)
    # print(open_orders)



    # data = tq_client.get_instrument_refdata("BTC-P/USDT@BINANCEDM")
    # print(data)
    #
    #
    # channel = await tq_client.get_rtmd_channel("BTC-P/USDT@BINANCEDM")
    # print(channel)
    #
    #
    # orderbook = await tq_client.get_orderbook("BTC-P/USDT@BINANCEDM")
    # print(orderbook)


    account_summary = await tq_client.get_account_summary(account_id=901)
    print(account_summary)


    account_margin_setting = await tq_client.get_instrument_margin_setting(account_id=901, instrument_id="BTC-P/USDT@BINANCEDM")
    print(account_margin_setting)


    resp = await tq_client.update_leverage(901, "BTC-P/USDT@BINANCEDM", 10)
    print(resp)


    id, is_error = await tq_client.request("tq.ods.rpc", "GenOrderId", common.DummyRequest())
    print(id)

    #print(tq_client.get_cached_refdata())

    inst_id = tq_client.get_instrument_id(
        account_id=901, base_asset="BTC", quote_asset="USDT",
        inst_type=common.InstrumentType.INST_TYPE_PERP)

    data1 = tq_client.get_instrument_refdata(inst_id)

    #await tq_client.load_refdata()


    #print(data1)

    await tq_client.stop()


if __name__ == "__main__":
    import asyncio
    asyncio.run(run())