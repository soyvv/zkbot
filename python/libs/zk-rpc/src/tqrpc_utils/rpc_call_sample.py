import datetime
import traceback

import nats
import asyncio


async def main():
    nc = await nats.connect("nats://localhost:4222")

    async def call_f(method: str, arg: str):
        try:
            headers = {"rpc_method": method}
            resp = await nc.request(subject="test1.test", payload=arg.encode(), headers=headers, timeout=5)
            print(resp.data.decode())
            return resp
        except:
            print(f"error processing {method}({arg})")
            traceback.print_exc()


    async def on_msg(msg):
        print(datetime.datetime.now())
        print(msg)

    await nc.subscribe("resp.test1.test", cb=on_msg)



    tasks = []
    for i in range(1):
        tasks.append(asyncio.create_task(call_f("Func1", f"f{i}")))

    # for i in range(1):
    #     tasks.append(asyncio.create_task(call_f("Func2", f"g{i}")))

    err_res = await asyncio.create_task(call_f("Func3", "3"))
    print("error result:", err_res)

    await asyncio.gather(*tasks)
    await asyncio.sleep(100)

if __name__ == "__main__":
    asyncio.run(main())

