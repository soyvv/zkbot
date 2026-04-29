"""Live test harness for the IBKR gateway adaptor against a paper TWS / IB Gateway.

Skipped unless ZK_IBKR_HOST is set. Default connection = paper session.

Usage:
    source devops/scripts/load-ibkr-paper-env.sh
    uv run --project venue-integrations/ibkr pytest tests/test_live_harness.py -v -s

The order-path class (``TestOrderPath``) is additionally gated behind
``ZK_IBKR_ALLOW_ORDERS=1`` to prevent accidental writes in CI or dry runs.
"""

from __future__ import annotations

import asyncio
import os
import time

import pytest

from ibkr.gw import IbkrGatewayAdaptor

from zk.exch_gw.v1 import exch_gw_pb2 as gw_pb


IBKR_HOST = os.environ.get("ZK_IBKR_HOST", "")
IBKR_PORT = int(os.environ.get("ZK_IBKR_PORT", "0") or 0)
IBKR_CLIENT_ID_BASE = int(os.environ.get("ZK_IBKR_CLIENT_ID", "17") or 17)
IBKR_ACCOUNT_CODE = os.environ.get("ZK_IBKR_ACCOUNT_CODE", "")
IBKR_MODE = os.environ.get("ZK_IBKR_MODE", "paper")
IBKR_ALLOW_ORDERS = os.environ.get("ZK_IBKR_ALLOW_ORDERS", "") == "1"

pytestmark = pytest.mark.skipif(
    not IBKR_HOST or IBKR_PORT <= 0,
    reason="ZK_IBKR_HOST/ZK_IBKR_PORT not set",
)


# TWS holds a client_id occupied for several seconds after disconnect.
# Use a monotonic offset per fixture invocation so sequential tests don't clash.
# When run under pytest-xdist, also offset by worker index * 50 so workers
# can't collide on shared client-ids against the same TWS session.
_client_id_counter = 0
_XDIST_WORKER = os.environ.get("PYTEST_XDIST_WORKER", "")  # e.g. "gw0"
_WORKER_OFFSET = int(_XDIST_WORKER[2:]) * 50 if _XDIST_WORKER.startswith("gw") else 0


def _next_client_id() -> int:
    global _client_id_counter
    cid = IBKR_CLIENT_ID_BASE + _WORKER_OFFSET + _client_id_counter
    _client_id_counter += 1
    return cid


def _parse_report(event: dict) -> gw_pb.OrderReport:
    report = gw_pb.OrderReport()
    report.ParseFromString(event["payload_bytes"])
    return report


def _make_config(read_only: bool) -> dict:
    return {
        "mode": IBKR_MODE,
        "host": IBKR_HOST,
        "port": IBKR_PORT,
        "client_id": _next_client_id(),
        "account_code": IBKR_ACCOUNT_CODE,
        "read_only": read_only,
    }


async def _shutdown(a: IbkrGatewayAdaptor) -> None:
    try:
        await a._conn.disconnect()
    except Exception:
        pass
    # Give TWS a moment to release the client-id slot before the next fixture
    # in the same suite reconnects. Without this, reqOpenOrdersAsync and other
    # session-scoped requests can hang for ~5s on the next connect. The next
    # fixture also bumps client_id, but TWS treats reuse of the prior id as a
    # collision for several seconds — keep this sleep as belt-and-braces.
    await asyncio.sleep(2.0)


@pytest.fixture
async def adaptor_ro():
    a = IbkrGatewayAdaptor(_make_config(read_only=True))
    await a.connect()
    try:
        yield a
    finally:
        await _shutdown(a)


@pytest.fixture
async def adaptor_rw():
    a = IbkrGatewayAdaptor(_make_config(read_only=False))
    await a.connect()
    try:
        yield a
    finally:
        await _shutdown(a)


class TestLiveHarness:
    @pytest.mark.asyncio
    async def test_connect(self, adaptor_ro):
        """Handshake completed; managed accounts visible."""
        assert adaptor_ro._conn.state == "live"
        assert adaptor_ro._conn._next_order_id is not None
        print(f"\nConnected: next_order_id={adaptor_ro._conn._next_order_id}")

    @pytest.mark.asyncio
    async def test_query_balance(self, adaptor_ro):
        """query_balance returns one row per cash balance."""
        # Give reqAccountUpdates a moment to stream initial values.
        await asyncio.sleep(2.0)
        facts = await adaptor_ro.query_balance({})
        print(f"\nBalances ({len(facts)}): {facts}")
        # On paper the account always has USD cash.
        assets = {f.get("asset") for f in facts}
        assert assets, "expected at least one cash balance row"

    @pytest.mark.asyncio
    async def test_query_positions(self, adaptor_ro):
        """query_positions returns a possibly-empty list."""
        await asyncio.sleep(2.0)
        facts = await adaptor_ro.query_positions({})
        print(f"\nPositions ({len(facts)}): {facts}")

    @pytest.mark.asyncio
    async def test_query_open_orders(self, adaptor_ro):
        facts = await adaptor_ro.query_open_orders({})
        print(f"\nOpen orders ({len(facts)}): {facts}")

    @pytest.mark.asyncio
    async def test_query_trades(self, adaptor_ro):
        facts = await adaptor_ro.query_trades({})
        print(f"\nTrades ({len(facts)}): {facts}")


@pytest.mark.skipif(
    not IBKR_ALLOW_ORDERS,
    reason="ZK_IBKR_ALLOW_ORDERS=1 required to run order-path tests",
)
class TestOrderPath:
    """Exercises place → booked → cancel → cancelled on a non-marketable limit.

    Uses AAPL at a price far enough below the last trade to stay unfilled
    during RTH, and at any price outside RTH. Delayed market data (type=3)
    lets the session run without live entitlements.
    """

    @pytest.mark.asyncio
    async def test_limit_place_then_cancel(self, adaptor_rw):
        correlation_id = int(time.time() * 1000) % 2**31
        ack = await adaptor_rw.place_order({
            "correlation_id": correlation_id,
            "instrument": "AAPL-USD-STK-SMART",
            "qty": 1,
            "buysell_type": 1,  # Buy
            "order_type": 1,    # Limit
            "price": 1.00,      # Intentionally far below market
        })
        print(f"\nPlace ack: {ack}")
        assert ack["success"] is True
        exch_ref = ack["exch_order_ref"]

        # Drain events until we see BOOKED (state or trade report).
        booked = False
        deadline = time.time() + 10.0
        while time.time() < deadline and not booked:
            try:
                event = await asyncio.wait_for(adaptor_rw.next_event(), timeout=2.0)
            except asyncio.TimeoutError:
                break
            if event["event_type"] != "order_report":
                continue
            report = _parse_report(event)
            for entry in report.order_report_entries:
                if entry.report_type == gw_pb.ORDER_REP_TYPE_STATE:
                    status = entry.order_state_report.exch_order_status
                    if status == gw_pb.EXCH_ORDER_STATUS_BOOKED:
                        booked = True
                        break
        assert booked, "did not see BOOKED order state after place_order"

        cancel_ack = await adaptor_rw.cancel_order({
            "order_id": correlation_id,
            "exch_order_ref": exch_ref,
        })
        print(f"Cancel ack: {cancel_ack}")
        assert cancel_ack["success"] is True

        cancelled = False
        deadline = time.time() + 10.0
        while time.time() < deadline and not cancelled:
            try:
                event = await asyncio.wait_for(adaptor_rw.next_event(), timeout=2.0)
            except asyncio.TimeoutError:
                break
            if event["event_type"] != "order_report":
                continue
            report = _parse_report(event)
            for entry in report.order_report_entries:
                if entry.report_type == gw_pb.ORDER_REP_TYPE_STATE:
                    status = entry.order_state_report.exch_order_status
                    if status == gw_pb.EXCH_ORDER_STATUS_CANCELLED:
                        cancelled = True
                        break
        assert cancelled, "did not see CANCELLED order state after cancel_order"
