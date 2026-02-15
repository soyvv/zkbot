import functools
import logging
import threading
import traceback
import dataclasses
from concurrent.futures import ThreadPoolExecutor

import betterproto
from dataclasses import dataclass
from typing import Callable, Optional
import inspect
import nats
import asyncio
import signal, os, sys
from contextlib import suppress

import json

from loguru import logger


@dataclass
class RPCCallError:
    message: str = None
    method: str = None

    def from_json(json_str: str):
        return RPCCallError(**json.loads(json_str))

def create_runner(shutdown_cb: Callable):
    async def shutdown(signal_, loop):
        print(f"Received {signal_.name} signal. Shutting down...")
        if shutdown_cb:
            await shutdown_cb()
        tasks = [t for t in asyncio.all_tasks() if t is not asyncio.current_task()]

        for task in tasks:
            task.cancel()

        with suppress(asyncio.CancelledError):
            await asyncio.gather(*tasks, return_exceptions=True)

        loop.stop()

    loop = asyncio.get_event_loop()

    # Register signal handlers for graceful shutdown
    signals = (signal.SIGTERM, signal.SIGINT)
    for sig in signals:
        loop.add_signal_handler(sig, lambda s=sig: asyncio.create_task(shutdown(s, loop)))

    return loop

@dataclass
class ServiceDescriptor:
    service_namespace: str = None
    service_name: str = None
    rpc_endpoint: str = None
    rpc_methods: list[str] = None
    publishing_topics: list[str] = None
    subscribing_topics: list[str] = None

@dataclass
class RpcMethodDescriptor:
    method_name: str = None
    method: Callable = None
    concurrency: int = 1
    is_async: bool = False
    run_in_executor: bool = True

class TQRpcService:
    def __init__(self, nc_url: str, rpc_endpoint: str, nc_task_queue_name:str=None):
        self.nc_url = nc_url
        self.nc: nats.NATS = None
        self.rpc_subject = rpc_endpoint
        self.nc_task_queue_name = nc_task_queue_name # if set, rpc requests will be load-balanced on this queue.
        self.rpc_subscribers: dict[str, RpcMethodDescriptor] = {}
        #self.rpc_concurrency: dict[str, int] = {}
        self.subscribers: dict[str, Callable] = {}
        self.startup_method: Callable = None
        self.stop_method: Callable = None
        self.task_methods: list[Callable] = []

        self.loop = asyncio.get_event_loop()

        self.q = asyncio.Queue()
        self.rpc_queues: dict[str, asyncio.Queue] = {}

        self.rpc_tasks = []

        #self.lifecycle_hooks: dict[str, Callable] = {} # lifecycle: started/stopped

    async def start(self):
        # connect
        self.nc = await nats.connect(self.nc_url)

        # rpc subscribers
        if self.nc_task_queue_name:
            await self.nc.subscribe(subject=self.rpc_subject,
                                    queue=self.nc_task_queue_name,
                                    cb=self.handle_rpc_request)
        else:
            await self.nc.subscribe(subject=self.rpc_subject,
                                    cb=self.handle_rpc_request)

        # other topic subscribers
        for topic, cb in self.subscribers:
            await self.nc.subscribe(subject=topic, cb=cb)

        # async startup method
        if self.startup_method:
            if inspect.iscoroutinefunction(self.startup_method):
                await self.startup_method()
            else:
                self.startup_method()

    async def stop(self):
        if self.stop_method:
            if inspect.iscoroutinefunction(self.stop_method):
                await self.stop_method()
            else:
                self.stop_method()
        await self.nc.close()

    def get_async_tasks(self):
        tasks = [self.publish_task()]
        if self.task_methods:
            for task_method in self.task_methods:
                tasks.append(task_method())
        tasks.extend(self.rpc_tasks)
        return tasks

    async def handle_rpc_request(self, msg):
        method = msg.headers["rpc_method"]
        if method is None or method not in self.rpc_queues:
            raise Exception(f"rpc method {method} not found")
        q = self.rpc_queues.get(method)

        await q.put(msg)


    async def publish_task(self):
        while True:
            (topic, data) = await self.q.get()
            try:
                if isinstance(data, str):
                    await self.nc.publish(subject=topic, payload=bytes(data, encoding='utf-8'))
                elif isinstance(data, RPCCallError):
                    headers = {"error": "1"}
                    await self.nc.publish(subject=topic, payload=bytes(json.dumps(dataclasses.asdict(data)), encoding='utf-8'), headers=headers)
                else:
                    await self.nc.publish(subject=topic, payload=bytes(data))
            except:
                traceback.print_exc()
            self.q.task_done()

    async def rpc_task(self, rpc_queue: asyncio.Queue,  rpc_method:str, id: int, executor: ThreadPoolExecutor=None):
        rmd = self.rpc_subscribers.get(rpc_method, None)
        if rmd is None:
            raise Exception(f"rpc method {rpc_method} not found")

        logger.info(f"creating rpc task for {rpc_method}")
        if executor:
            logger.info(f"using executor {executor} with max_workers={executor._max_workers}")
        while True:
            msg = await rpc_queue.get()

            try:
                print(f"rpc task {rpc_method}_{id} got message {msg}")
                if not rmd.is_async:
                    if executor:
                        asyncio.get_running_loop().run_in_executor(executor, rmd.method, msg)
                    else:
                        rmd.method(msg)
                else:
                    # async method
                    asyncio.create_task(rmd.method(msg))
                    #await rmd.method(msg)
            except:
                traceback.print_exc()

            rpc_queue.task_done()
        logger.info(f"rpc task for {rpc_method} stopped")

    def generate_rpc_tasks(self):
        tasks = []
        for method_name, rmd in self.rpc_subscribers.items():
            concurrency = rmd.concurrency
            q = asyncio.Queue()
            self.rpc_queues[method_name] = q
            if not rmd.is_async and rmd.run_in_executor:
                executor = ThreadPoolExecutor(max_workers=concurrency)
                task = self.rpc_task(rpc_queue=q, rpc_method=method_name, id=0, executor=executor)
                tasks.append(task)
            elif rmd.is_async:
                for i in range(concurrency):
                    task = self.rpc_task(rpc_queue=q, rpc_method=method_name, id=i, executor=None)
                    tasks.append(task)

        self.rpc_tasks = tasks


    def enqueue_and_publish(self, data, topic: str):
        asyncio.run_coroutine_threadsafe(self.q.put((topic, data)), self.loop)




class RpcServiceBuilder:

    def __init__(self, service_namespace: str,
                 service_name: str,
                 rpc_endpoint: str,
                 nc_url="nats://localhost:4222",
                 nc_task_queue_name:str=None):
        service_desc = ServiceDescriptor()
        service_desc.service_namespace = service_namespace
        service_desc.service_name = service_name

        self.sub_table: dict[str, RpcMethodDescriptor] = {}
        self.start_method: Callable = None # startup method
        self.task_methods: list[Callable] = [] # long running task
        self.stop_method: Callable = None # stop method

        self.sub_cbs: list[tuple[str, Callable]] = []
        #self.nc = nats.NATS("")

        self.svc = TQRpcService(nc_url=nc_url, rpc_endpoint=rpc_endpoint, nc_task_queue_name=nc_task_queue_name)


    def register_rpc_method(self, method: Callable, concurrency=1) -> "RpcServiceBuilder":
        sig = inspect.signature(method)

        # todo: validate the signature: 1) one param one return; 2) must be with annotated types

        params = sig.parameters
        param_type = list(sig.parameters.items())[0][1].annotation
        ret_type = sig.return_annotation

        async def handle_req_async(msg):
            raw_data = msg.data  # bytes
            reply = msg.reply
            req = param_type()

            output = None
            try:
                if param_type == str:
                    input = raw_data.decode('utf-8')
                elif issubclass(param_type, betterproto.Message):
                    input = req.parse(raw_data)
                else:
                    raise Exception("input type not supported")
                output: ret_type = await method(input)
            except:
                err_msg = traceback.format_exc()
                logger.error("error in handling rpc request: " + method_name)
                logger.error(err_msg)
                output = RPCCallError(
                    message=err_msg,
                    method=method_name
                )
                # todo: reorg the error handling
            if output is not None:
                self.svc.enqueue_and_publish(topic=reply, data=output)

        def handle_req(msg):
            raw_data = msg.data  # bytes
            reply = msg.reply
            req = param_type()

            output = None
            try:
                if param_type == str:
                    input = raw_data.decode('utf-8')
                elif issubclass(param_type, betterproto.Message):
                    input = req.parse(raw_data)
                else:
                    raise Exception("input type not supported")
                output: ret_type = method(input)
            except:
                err_msg = traceback.format_exc()
                logger.error("error in handling rpc request: " + method_name)
                logger.error(err_msg)
                output = RPCCallError(
                    message=err_msg,
                    method=method_name
                )
                # todo: reorg the error handling
            # if not isinstance(output, ret_type):
            #     raise Exception("return type not match")
            if output is not None:
                self.svc.enqueue_and_publish(topic=reply, data=output)

        method_name = method.__name__
        # convert to camael case
        rpc_name = "".join([w.capitalize() for w in method_name.split("_")])
        rmd = RpcMethodDescriptor()
        rmd.method_name =rpc_name
        rmd.is_async = inspect.iscoroutinefunction(method)
        rmd.method = handle_req if not rmd.is_async else handle_req_async
        rmd.concurrency = concurrency
        rmd.run_in_executor = not rmd.is_async # for now, assume all sync methods are blocking functions
        self.sub_table[rpc_name] = rmd

        return self


    def register_as_publisher(self, public_topic: str,
                              publisher_register_method: Callable[[Callable[[any], None]], None]):
        def _publish(data):
            self.svc.enqueue_and_publish(data=data, topic=public_topic)

        publisher_register_method(_publish)

        return self

    def register_as_subscriber(self, sub_topic: str, sub_cb: Callable[[any], None], decoder: Callable[[bytes], any] = None):
        async def _sub_from_topic(msg):
            raw_data = msg.data
            try:
                if decoder:
                    user_data = decoder(raw_data)
                else:
                    user_data = raw_data
                sub_cb(user_data)
            except Exception as e:
                traceback.print_exc()

        self.sub_cbs.append((sub_topic, _sub_from_topic))

        return self


    def register_start_method(self, startup_method: Callable):
        self.start_method = startup_method
        return self

    def register_stop_method(self, stop_method: Callable):
        self.stop_method = stop_method
        return self

    def register_task_method(self, task_method: Callable):
        if inspect.iscoroutinefunction(task_method):
            self.task_methods.append(task_method)
        else:
            raise Exception("task method must be async function")
        return self

    def build(self) -> TQRpcService:
        self.svc.rpc_subscribers = self.sub_table
        self.svc.subscribers = self.sub_cbs
        self.svc.startup_method = self.start_method
        self.svc.stop_method = self.stop_method
        self.svc.task_methods = self.task_methods

        self.svc.generate_rpc_tasks()

        return self.svc

