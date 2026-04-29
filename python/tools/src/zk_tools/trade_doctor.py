#!/usr/bin/env python3
from __future__ import annotations

import argparse
import asyncio
import importlib
import os
import sys
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any

from loguru import logger


REGISTRY_BUCKET = "zk-svc-registry-v1"


def _normalize_addr(addr: str) -> str:
    return addr.removeprefix("http://").removeprefix("https://")


def _bool_env(name: str, default: bool = False) -> bool:
    value = os.getenv(name)
    if value is None:
        return default
    return value.lower() in {"1", "true", "yes", "on"}


def load_proto_modules() -> dict[str, Any]:
    modules = {
        "common_pb2": "zk.common.v1.common_pb2",
        "exch_gw_pb2": "zk.exch_gw.v1.exch_gw_pb2",
        "gateway_pb2": "zk.gateway.v1.gateway_service_pb2",
        "gateway_grpc": "zk.gateway.v1.gateway_service_pb2_grpc",
        "sim_admin_pb2": "zk.gateway.v1.gateway_simulator_admin_pb2",
        "sim_admin_grpc": "zk.gateway.v1.gateway_simulator_admin_pb2_grpc",
        "oms_pb2": "zk.oms.v1.oms_pb2",
        "oms_service_grpc": "zk.oms.v1.oms_service_pb2_grpc",
        "rtmd_pb2": "zk.rtmd.v1.rtmd_pb2",
        "discovery_pb2": "zk.discovery.v1.discovery_pb2",
    }
    return {key: importlib.import_module(path) for key, path in modules.items()}


PROTO = load_proto_modules()

import grpc  # noqa: E402
import nats  # noqa: E402


@dataclass
class StepReport:
    name: str
    ok: bool
    duration_ms: int
    detail: str


@dataclass
class ScenarioReport:
    scenario: str
    success: bool = True
    summary: str = ""
    steps: list[StepReport] = field(default_factory=list)
    notes: list[str] = field(default_factory=list)
    observations: dict[str, str] = field(default_factory=dict)

    def fail(self, message: str) -> None:
        self.success = False
        self.summary = message

    def note(self, message: str) -> None:
        self.notes.append(message)

    def observe(self, key: str, value: Any) -> None:
        self.observations[key] = str(value)


def now_ms() -> int:
    return int(time.time() * 1000)


def next_order_id() -> int:
    return int(time.time() * 1_000_000)


class TradeDoctorError(RuntimeError):
    pass


async def timed_step(report: ScenarioReport, name: str, coro):
    started = time.perf_counter()
    try:
        value, detail = await coro
        elapsed_ms = int((time.perf_counter() - started) * 1000)
        report.steps.append(StepReport(name=name, ok=True, duration_ms=elapsed_ms, detail=detail))
        logger.info("{} ok in {} ms: {}", name, elapsed_ms, detail)
        return value
    except Exception as exc:
        elapsed_ms = int((time.perf_counter() - started) * 1000)
        report.steps.append(
            StepReport(name=name, ok=False, duration_ms=elapsed_ms, detail=str(exc))
        )
        logger.exception("{} failed in {} ms", name, elapsed_ms)
        raise


async def connect_nats(url: str):
    nc = await nats.connect(url)
    logger.info("nats connected: {}", url)
    return nc


async def resolve_addr_from_kv(nc, key: str) -> str:
    js = nc.jetstream()
    kv = await js.key_value(REGISTRY_BUCKET)
    entry = await kv.get(key)
    if entry is None:
        raise TradeDoctorError(f"service registry entry not found: {key}")
    reg = PROTO["discovery_pb2"].ServiceRegistration()
    reg.ParseFromString(entry.value)
    if not reg.endpoint.address:
        raise TradeDoctorError(f"registry entry missing endpoint address: {key}")
    return reg.endpoint.address


async def resolve_gw_addr(nc, grpc_host: str, gw_id: str, override: str | None) -> str:
    return _normalize_addr(override) if override else await resolve_addr_from_kv(
        nc, f"svc.gw.{gw_id}"
    )


async def resolve_oms_addr(nc, grpc_host: str, oms_id: str, override: str | None) -> str:
    return _normalize_addr(override) if override else await resolve_addr_from_kv(
        nc, f"svc.oms.{oms_id}"
    )


async def connect_channel(addr: str) -> grpc.aio.Channel:
    addr = _normalize_addr(addr)
    channel = grpc.aio.insecure_channel(addr)
    await asyncio.wait_for(channel.channel_ready(), timeout=5.0)
    logger.info("grpc connected: {}", addr)
    return channel


def parse_message(msg_cls, payload: bytes):
    msg = msg_cls()
    msg.ParseFromString(payload)
    return msg


async def wait_for_matching_msg(subscription, decoder, matcher, timeout_ms: int, label: str):
    deadline = time.monotonic() + timeout_ms / 1000.0
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TradeDoctorError(f"timed out waiting for {label}")
        msg = await asyncio.wait_for(subscription.next_msg(), timeout=remaining)
        decoded = decoder(msg.data)
        if matcher(decoded):
            return decoded


async def wait_for_any_msg(subscription, decoder, timeout_ms: int, label: str):
    msg = await asyncio.wait_for(subscription.next_msg(), timeout=timeout_ms / 1000.0)
    return decoder(msg.data)


def gw_subject(kind: str, gw_id: str) -> str:
    return f"zk.gw.{gw_id}.{kind}"


def oms_order_subject(oms_id: str, account_id: int) -> str:
    return f"zk.oms.{oms_id}.order_update.{account_id}"


def oms_balance_subject(oms_id: str) -> str:
    return f"zk.oms.{oms_id}.balance_update"


def oms_position_subject(oms_id: str) -> str:
    return f"zk.oms.{oms_id}.position_update"


def oms_latency_subject(oms_id: str) -> str:
    return f"zk.oms.{oms_id}.metrics.latency"


def gateway_response_ok(resp) -> bool:
    return resp.status == PROTO["gateway_pb2"].GatewayResponse.GW_RESP_STATUS_SUCCESS


def oms_response_ok(resp) -> bool:
    return resp.status == PROTO["oms_pb2"].OMSResponse.OMS_RESP_STATUS_SUCCESS


def make_gw_place(args, order_id: int):
    req = PROTO["gateway_pb2"].SendOrderRequest()
    req.correlation_id = order_id
    req.exch_account_id = args.exch_account_id or str(args.account_id)
    req.instrument = args.instrument
    req.buysell_type = PROTO["common_pb2"].BS_BUY
    req.openclose_type = PROTO["common_pb2"].OC_OPEN
    req.order_type = PROTO["common_pb2"].ORDERTYPE_LIMIT
    req.scaled_price = args.price
    req.scaled_qty = args.qty
    req.leverage = 1.0
    req.timestamp = now_ms()
    return req


def make_oms_place(args, order_id: int):
    req = PROTO["oms_pb2"].PlaceOrderRequest()
    req.order_request.order_id = order_id
    req.order_request.account_id = args.account_id
    req.order_request.instrument_code = args.instrument
    req.order_request.buy_sell_type = PROTO["common_pb2"].BS_BUY
    req.order_request.open_close_type = PROTO["common_pb2"].OC_OPEN
    req.order_request.order_type = PROTO["common_pb2"].ORDERTYPE_LIMIT
    req.order_request.price = args.price
    req.order_request.qty = args.qty
    req.order_request.source_id = "trade_doctor_py"
    req.order_request.timestamp = now_ms()
    return req


def make_oms_cancel(order_id: int):
    req = PROTO["oms_pb2"].CancelOrderRequest()
    req.order_cancel_request.order_id = order_id
    req.order_cancel_request.timestamp = now_ms()
    return req


async def wait_for_latency_metric(subscription, order_id: int, timeout_ms: int):
    deadline = time.monotonic() + timeout_ms / 1000.0
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return None
        try:
            msg = await asyncio.wait_for(subscription.next_msg(), timeout=remaining)
        except asyncio.TimeoutError:
            return None
        batch = parse_message(PROTO["common_pb2"].LatencyMetricBatch, msg.data)
        for metric in batch.metrics:
            if metric.order_id == order_id:
                return dict(metric.tagged_timestamps)


async def run_gw_smoke(args) -> ScenarioReport:
    report = ScenarioReport("gw-smoke")
    nc = await timed_step(report, "connect_nats", _step_connect_nats(args))
    gw_addr = await timed_step(report, "resolve_gateway", _step_resolve_gw(args, nc))
    report.observe("gw_addr", gw_addr)
    channel = await timed_step(report, "connect_gateway_grpc", _step_connect_grpc(gw_addr))

    stub = PROTO["gateway_grpc"].GatewayServiceStub(channel)
    report_sub = await nc.subscribe(gw_subject("report", args.gw_id))
    balance_sub = await nc.subscribe(gw_subject("balance", args.gw_id))
    system_sub = await nc.subscribe(gw_subject("system", args.gw_id))

    await _optional_step(report, "health_check", _gw_health_check(stub))
    await _optional_step(report, "query_balance", _gw_query_balance(stub))

    order_id = next_order_id()
    place_started = now_ms()
    place_resp = await timed_step(report, "place_order", _gw_place(stub, args, order_id))
    report.observe("gw_ack_ts_ms", place_resp.timestamp)

    order_report = await timed_step(
        report,
        "wait_order_report",
        _wait_gw_order_report(report_sub, order_id, args.timeout_ms),
    )
    report.observe("gw_order_report_latency_ms", now_ms() - place_started)
    report.observe("gw_exch_order_ref", order_report.exch_order_ref)

    await _optional_step(report, "optional_balance_push", _wait_any_gw_balance(balance_sub))
    await _optional_step(report, "optional_system_event", _wait_any_gw_system(system_sub))

    report.summary = "gateway smoke completed"
    return report


async def run_oms_smoke(args) -> ScenarioReport:
    report = ScenarioReport("oms-smoke")
    nc = await timed_step(report, "connect_nats", _step_connect_nats(args))
    oms_addr = await timed_step(report, "resolve_oms", _step_resolve_oms(args, nc))
    report.observe("oms_addr", oms_addr)
    channel = await timed_step(report, "connect_oms_grpc", _step_connect_grpc(oms_addr))

    stub = PROTO["oms_service_grpc"].OMSServiceStub(channel)
    order_sub = await nc.subscribe(oms_order_subject(args.oms_id, args.account_id))
    balance_sub = await nc.subscribe(oms_balance_subject(args.oms_id))
    position_sub = await nc.subscribe(oms_position_subject(args.oms_id))
    latency_sub = await nc.subscribe(oms_latency_subject(args.oms_id))

    await _optional_step(report, "health_check", _oms_health_check(stub))
    await _optional_step(report, "query_balances", _oms_query_balances(stub, args.account_id))
    await _optional_step(report, "query_positions", _oms_query_positions(stub, args.account_id))

    open_before = await _query_open_orders_best_effort(stub, args.account_id)
    report.observe("open_orders_before", len(open_before.orders))

    order_id = next_order_id()
    t0 = now_ms()
    place_resp = await timed_step(report, "place_order", _oms_place(stub, args, order_id))
    report.observe("oms_ack_ts_ms", place_resp.timestamp)
    report.observe("client_place_ts_ms", t0)

    booked = await timed_step(
        report,
        "wait_order_update",
        _wait_oms_order_update(order_sub, order_id, None, args.timeout_ms),
    )
    report.observe("oms_first_update_latency_ms", now_ms() - t0)

    status = (
        booked.order_snapshot.order_status if booked.HasField("order_snapshot") else None
    )
    if status in {
        PROTO["oms_pb2"].ORDER_STATUS_PENDING,
        PROTO["oms_pb2"].ORDER_STATUS_BOOKED,
        PROTO["oms_pb2"].ORDER_STATUS_PARTIALLY_FILLED,
    }:
        await _optional_step(report, "cancel_order", _oms_cancel(stub, order_id))
        await _optional_step(
            report,
            "wait_cancelled_update",
            _wait_oms_order_update(
                order_sub,
                order_id,
                PROTO["oms_pb2"].ORDER_STATUS_CANCELLED,
                args.timeout_ms,
            ),
        )
    else:
        report.note("cancel step skipped because first OMS update was already terminal")

    await _optional_step(report, "optional_balance_push", _wait_any_oms_balance(balance_sub))
    await _optional_step(report, "optional_position_push", _wait_any_oms_position(position_sub))
    await _optional_step(
        report, "optional_latency_metric", _wait_any_latency_metric(latency_sub, order_id)
    )

    report.summary = "OMS smoke completed"
    return report


async def run_sim_full_cycle(args) -> ScenarioReport:
    report = ScenarioReport("sim-full-cycle")
    nc = await timed_step(report, "connect_nats", _step_connect_nats(args))
    gw_addr = await timed_step(report, "resolve_gateway", _step_resolve_gw(args, nc))
    oms_addr = await timed_step(report, "resolve_oms", _step_resolve_oms(args, nc))
    admin_addr = _normalize_addr(args.gw_admin_addr or f"{args.grpc_host}:51052")
    report.observe("gw_addr", gw_addr)
    report.observe("oms_addr", oms_addr)
    report.observe("gw_admin_addr", admin_addr)

    oms_channel = await timed_step(report, "connect_oms_grpc", _step_connect_grpc(oms_addr))
    admin_channel = await timed_step(
        report, "connect_gw_admin_grpc", _step_connect_grpc(admin_addr)
    )

    oms_stub = PROTO["oms_service_grpc"].OMSServiceStub(oms_channel)
    admin_stub = PROTO["sim_admin_grpc"].GatewaySimulatorAdminServiceStub(admin_channel)
    order_sub = await nc.subscribe(oms_order_subject(args.oms_id, args.account_id))
    balance_sub = await nc.subscribe(oms_balance_subject(args.oms_id))

    order_id = next_order_id()
    await timed_step(report, "place_order_via_oms", _oms_place(oms_stub, args, order_id))
    await timed_step(
        report,
        "wait_booked_update",
        _wait_oms_order_update(order_sub, order_id, None, args.timeout_ms),
    )
    await timed_step(report, "force_match", _sim_force_match(admin_stub, args, order_id))
    await timed_step(
        report,
        "wait_filled_update",
        _wait_oms_order_update(
            order_sub,
            order_id,
            PROTO["oms_pb2"].ORDER_STATUS_FILLED,
            args.timeout_ms,
        ),
    )
    await _optional_step(report, "optional_balance_push", _wait_any_oms_balance(balance_sub))

    report.summary = "sim full cycle completed"
    return report


def _step_connect_nats(args):
    async def inner():
        nc = await connect_nats(args.nats_url)
        return nc, f"connected to {args.nats_url}"

    return inner()


def _step_resolve_gw(args, nc):
    async def inner():
        addr = await resolve_gw_addr(nc, args.grpc_host, args.gw_id, args.gw_addr)
        return addr, f"resolved {args.gw_id} to {addr}"

    return inner()


def _step_resolve_oms(args, nc):
    async def inner():
        addr = await resolve_oms_addr(nc, args.grpc_host, args.oms_id, args.oms_addr)
        return addr, f"resolved {args.oms_id} to {addr}"

    return inner()


def _step_connect_grpc(addr: str):
    async def inner():
        channel = await connect_channel(addr)
        return channel, f"gRPC channel established to {addr}"

    return inner()


def _gw_health_check(stub):
    async def inner():
        resp = await stub.HealthCheck(PROTO["common_pb2"].DummyRequest())
        return None, f"status={resp.status}"

    return inner()


def _gw_query_balance(stub):
    async def inner():
        resp = await stub.QueryAccountBalance(PROTO["gateway_pb2"].QueryAccountRequest())
        count = len(resp.balance_update.balances) if resp.HasField("balance_update") else 0
        return None, f"returned {count} balance entries"

    return inner()


def _gw_place(stub, args, order_id: int):
    async def inner():
        resp = await stub.PlaceOrder(make_gw_place(args, order_id))
        if not gateway_response_ok(resp):
            raise TradeDoctorError(f"gateway place_order returned failure: {resp.message}")
        return resp, f"place_order ack timestamp={resp.timestamp}"

    return inner()


def _wait_gw_order_report(subscription, order_id: int, timeout_ms: int):
    async def inner():
        event = await wait_for_matching_msg(
            subscription,
            lambda b: parse_message(PROTO["exch_gw_pb2"].OrderReport, b),
            lambda x: x.order_id == order_id,
            timeout_ms,
            f"gateway OrderReport order_id={order_id}",
        )
        return event, f"received order report exch_order_ref={event.exch_order_ref}"

    return inner()


def _wait_any_gw_balance(subscription):
    async def inner():
        try:
            update = await wait_for_any_msg(
                subscription,
                lambda b: parse_message(PROTO["exch_gw_pb2"].BalanceUpdate, b),
                1_000,
                "gateway balance update",
            )
            return None, f"received balance push with {len(update.balances)} entries"
        except Exception:
            return None, "no balance push observed within 1s"

    return inner()


def _wait_any_gw_system(subscription):
    async def inner():
        try:
            event = await wait_for_any_msg(
                subscription,
                lambda b: parse_message(PROTO["exch_gw_pb2"].GatewaySystemEvent, b),
                500,
                "gateway system event",
            )
            return None, f"system event type={event.event_type}"
        except Exception:
            return None, "no system event observed after subscription"

    return inner()


def _oms_health_check(stub):
    async def inner():
        resp = await stub.HealthCheck(PROTO["common_pb2"].DummyRequest())
        return None, f"status={resp.status}"

    return inner()


def _oms_query_balances(stub, account_id: int):
    async def inner():
        req = PROTO["oms_pb2"].QueryBalancesRequest(account_id=account_id, query_gw=False)
        resp = await stub.QueryBalances(req)
        return None, f"returned {len(resp.balances)} balances"

    return inner()


def _oms_query_positions(stub, account_id: int):
    async def inner():
        req = PROTO["oms_pb2"].QueryPositionRequest(account_id=account_id, query_gw=False)
        resp = await stub.QueryPosition(req)
        return None, f"returned {len(resp.positions)} positions"

    return inner()


async def _query_open_orders_best_effort(stub, account_id: int):
    req = PROTO["oms_pb2"].QueryOpenOrderRequest(account_id=account_id, query_gw=False)
    try:
        return await stub.QueryOpenOrders(req)
    except Exception:
        return PROTO["oms_pb2"].OrderDetailResponse()


def _oms_place(stub, args, order_id: int):
    async def inner():
        resp = await stub.PlaceOrder(make_oms_place(args, order_id))
        if not oms_response_ok(resp):
            raise TradeDoctorError(f"OMS place_order returned failure: {resp.message}")
        return resp, "OMS accepted place_order"

    return inner()


def _oms_cancel(stub, order_id: int):
    async def inner():
        resp = await stub.CancelOrder(make_oms_cancel(order_id))
        if not oms_response_ok(resp):
            raise TradeDoctorError(f"OMS cancel_order returned failure: {resp.message}")
        return None, "OMS accepted cancel_order"

    return inner()


def _wait_oms_order_update(subscription, order_id: int, expected_status: int | None, timeout_ms: int):
    async def inner():
        event = await wait_for_matching_msg(
            subscription,
            lambda b: parse_message(PROTO["oms_pb2"].OrderUpdateEvent, b),
            lambda x: (
                x.order_id == order_id
                and (
                    expected_status is None
                    or (x.HasField("order_snapshot") and x.order_snapshot.order_status == expected_status)
                )
            ),
            timeout_ms,
            f"OMS OrderUpdateEvent order_id={order_id}",
        )
        status_name = (
            PROTO["oms_pb2"].OrderStatus.Name(event.order_snapshot.order_status)
            if event.HasField("order_snapshot")
            else "UNKNOWN"
        )
        return event, f"received OMS order update status={status_name}"

    return inner()


def _wait_any_oms_balance(subscription):
    async def inner():
        try:
            update = await wait_for_any_msg(
                subscription,
                lambda b: parse_message(PROTO["oms_pb2"].BalanceUpdateEvent, b),
                1_000,
                "OMS balance update",
            )
            return None, f"received OMS balance push with {len(update.balance_snapshots)} balances"
        except Exception:
            return None, "no OMS balance push observed within 1s"

    return inner()


def _wait_any_oms_position(subscription):
    async def inner():
        try:
            update = await wait_for_any_msg(
                subscription,
                lambda b: parse_message(PROTO["oms_pb2"].PositionUpdateEvent, b),
                1_000,
                "OMS position update",
            )
            return None, f"received OMS position push with {len(update.position_snapshots)} positions"
        except Exception:
            return None, "no OMS position push observed within 1s"

    return inner()


def _wait_any_latency_metric(subscription, order_id: int):
    async def inner():
        tags = await wait_for_latency_metric(subscription, order_id, 12_000)
        if tags is None:
            return None, "no OMS latency metric batch observed for this order"
        return None, f"received OMS latency metric tags={len(tags)}"

    return inner()


def _sim_force_match(stub, args, order_id: int):
    async def inner():
        req = PROTO["sim_admin_pb2"].ForceMatchRequest(
            order_id=order_id,
            fill_mode=PROTO["sim_admin_pb2"].FILL_MODE_FULL,
            fill_qty=0.0,
            fill_price=args.price,
            publish_balance_update=True,
            publish_position_update=True,
        )
        resp = await stub.ForceMatch(req)
        if not resp.accepted:
            raise TradeDoctorError(f"ForceMatch rejected: {resp.message}")
        return resp, (
            f"accepted matched_order_id={resp.matched_order_id} "
            f"generated_trade_count={resp.generated_trade_count}"
        )

    return inner()


async def _optional_step(report: ScenarioReport, name: str, coro):
    try:
        await timed_step(report, name, coro)
    except Exception as exc:
        report.note(f"{name}: {exc}")


def configure_logging(level: str, json_logs: bool) -> None:
    logger.remove()
    if json_logs:
        logger.add(sys.stderr, level=level.upper(), serialize=True, backtrace=True, diagnose=False)
    else:
        logger.add(
            sys.stderr,
            level=level.upper(),
            backtrace=True,
            diagnose=False,
            format=(
                "<green>{time:YYYY-MM-DD HH:mm:ss.SSS}</green> "
                "<level>{level: <8}</level> "
                "<cyan>{name}</cyan>:<cyan>{function}</cyan>:<cyan>{line}</cyan> "
                "- <level>{message}</level>"
            ),
        )


def print_report(report: ScenarioReport) -> None:
    status = "PASS" if report.success else "FAIL"
    logger.info("scenario={} status={} summary={}", report.scenario, status, report.summary)
    for step in report.steps:
        line = f"[{'ok' if step.ok else 'xx'}] {step.name} {step.duration_ms} ms :: {step.detail}"
        if step.ok:
            logger.info(line)
        else:
            logger.error(line)
    for key, value in sorted(report.observations.items()):
        logger.info("obs {}={}", key, value)
    for note in report.notes:
        logger.warning("note {}", note)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Smoke-test gateway and OMS services.")
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument("--nats-url", default="nats://127.0.0.1:4222")
    common.add_argument("--grpc-host", default="127.0.0.1")
    common.add_argument("--account-id", type=int, default=9001)
    common.add_argument("--exch-account-id")
    common.add_argument("--instrument", default="BTCUSDT_SIM")
    common.add_argument("--price", type=float, default=50_000.0)
    common.add_argument("--qty", type=float, default=0.001)
    common.add_argument("--timeout-ms", type=int, default=5_000)
    common.add_argument("--json-out")
    common.add_argument("--log-level", default=os.getenv("TRADE_DOCTOR_LOG_LEVEL", "INFO"))
    common.add_argument(
        "--json-logs", action="store_true", default=_bool_env("TRADE_DOCTOR_JSON_LOGS")
    )

    sub = parser.add_subparsers(dest="command", required=True)

    gw = sub.add_parser("gw-smoke", parents=[common])
    gw.add_argument("--gw-id", required=True)
    gw.add_argument("--gw-addr")

    oms = sub.add_parser("oms-smoke", parents=[common])
    oms.add_argument("--oms-id", required=True)
    oms.add_argument("--oms-addr")

    sim = sub.add_parser("sim-full-cycle", parents=[common])
    sim.add_argument("--gw-id", required=True)
    sim.add_argument("--oms-id", required=True)
    sim.add_argument("--gw-addr")
    sim.add_argument("--oms-addr")
    sim.add_argument("--gw-admin-addr")

    return parser


async def async_main(args) -> int:
    try:
        if args.command == "gw-smoke":
            report = await run_gw_smoke(args)
        elif args.command == "oms-smoke":
            report = await run_oms_smoke(args)
        elif args.command == "sim-full-cycle":
            report = await run_sim_full_cycle(args)
        else:
            raise TradeDoctorError(f"unknown command: {args.command}")
    except Exception as exc:
        report = ScenarioReport(args.command, success=False, summary=str(exc))
        logger.exception("scenario failed")

    print_report(report)
    if args.json_out:
        Path(args.json_out).write_text(json.dumps(asdict(report), indent=2))
        logger.info("wrote report to {}", args.json_out)
    return 0 if report.success else 1


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()
    configure_logging(args.log_level, args.json_logs)
    return asyncio.run(async_main(args))


if __name__ == "__main__":
    raise SystemExit(main())
