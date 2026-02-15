import asyncio
import threading
import time
import concurrent.futures

import tqrpc_utils.rpc_utils as rpc_utils

import logging
logging.basicConfig(format="%(levelname)s:%(filename)s:%(lineno)d:%(asctime)s:%(message)s", level=logging.DEBUG)

logger = logging.getLogger(__name__)

class rpc_sample_service1:

    def start(self):
        print("start")
        return

    def func1(self, param1: str) -> str:
        print("func1 called")
        time.sleep(1)
        return f"f1: hello, {param1}"


    def func2(self, param2: str) -> str:
        time.sleep(1)
        return f"f2: hello, {param2}"


    def func3(self, param3: str) -> str:
        raise Exception("error")

    def stop(self):
        print("stop")
        return


class rpc_sample_service2:

    def start(self):
        print("start")
        return

    async def func1(self, param1: str) -> str:
        print("func1 called")
        await asyncio.sleep(1)
        return f"f1: hello, {param1}"


    async def func2(self, param2: str) -> str:
        await asyncio.sleep(1)
        return f"f2: hello, {param2}"




    async def _call_to_ex(self):
        raise Exception("error")

    async def func3(self, param3: str) -> str:
        task = asyncio.create_task(self._call_to_ex())
        await asyncio.gather(task)
        return "func3 done"

    def stop(self):
        print("stop")
        return


class rpc_sample_service3:

    def __init__(self):
        self.executor = concurrent.futures.ThreadPoolExecutor(max_workers=5)
        self.publisher = None

    def start(self):
        print("start")
        return


    def blocking_func(self, param: str, i: int):
        logger.info('blocking_func called: ' + str(i))
        time.sleep(5)

        ret_item = f"blocking_func: hello, {param} + {i}"
        logger.info(f"blocking func return: {i}")
        self.publisher(ret_item)
        logger.info(f"published: {i}")


    def func1(self, param1: str) -> str:
        logger.info("func1 called")
        for ii in range(5):
            #asyncio.get_event_loop().run_in_executor(self.exeutor, self.blocking_func, param1)
            self.executor.submit(self.blocking_func, param1, ii)
        return f"f1: hello, {param1}"

    async def func2(self, param2: str) -> str:
        await asyncio.sleep(1)
        return f"f2: hello, {param2}"


    def register_publisher(self, publisher):
        self.publisher = publisher

    def stop(self):
        print("stop")
        return



def main1():
    rpc_handler = rpc_sample_service1()
    rpc_server = rpc_utils.RpcServiceBuilder(
        nc_url="nats://localhost:4222",
        service_name="test1",
        service_namespace="test",
        rpc_endpoint="test1.test")\
        .register_rpc_method(rpc_handler.func1, concurrency=5) \
        .register_rpc_method(rpc_handler.func2, concurrency=5) \
        .register_rpc_method(rpc_handler.func3, concurrency=5) \
        .register_start_method(rpc_handler.start) \
        .build()


    runner = rpc_utils.create_runner(rpc_server.stop)
    runner.run_until_complete(rpc_server.start())

    tasks = rpc_server.get_async_tasks()
    for task in tasks:
        #runner.run_in_executor(executor=None, func=task)
        runner.create_task(task)

    runner.run_until_complete(asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED))
    print("done")


def main2():
    rpc_handler = rpc_sample_service2()
    rpc_server = rpc_utils.RpcServiceBuilder(
        nc_url="nats://localhost:4222",
        service_name="test1",
        service_namespace="test",
        rpc_endpoint="test1.test")\
        .register_rpc_method(rpc_handler.func1, concurrency=5) \
        .register_rpc_method(rpc_handler.func2, concurrency=5) \
        .register_rpc_method(rpc_handler.func3, concurrency=5) \
        .register_start_method(rpc_handler.start) \
        .build()


    runner = rpc_utils.create_runner(rpc_server.stop)
    runner.run_until_complete(rpc_server.start())

    tasks = rpc_server.get_async_tasks()
    for task in tasks:
        #runner.run_in_executor(executor=None, func=task)
        runner.create_task(task)

    runner.run_until_complete(asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED))
    print("done")



def main3():
    rpc_handler = rpc_sample_service3()
    rpc_server = rpc_utils.RpcServiceBuilder(
        nc_url="nats://localhost:4222",
        service_name="test1",
        service_namespace="test",
        rpc_endpoint="test1.test")\
        .register_rpc_method(rpc_handler.func1, concurrency=5) \
        .register_rpc_method(rpc_handler.func2, concurrency=5) \
        .register_start_method(rpc_handler.start) \
        .register_as_publisher("resp.test1.test", rpc_handler.register_publisher)  \
        .build()


    runner = rpc_utils.create_runner(rpc_server.stop)
    runner.run_until_complete(rpc_server.start())

    tasks = rpc_server.get_async_tasks()
    for task in tasks:
        #runner.run_in_executor(executor=None, func=task)
        runner.create_task(task)

    runner.run_until_complete(asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED))

    print("done")

if __name__ == "__main__":
    main2()

