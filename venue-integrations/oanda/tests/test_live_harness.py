"""Live test harness for the OANDA adaptor against the demo account.

Skipped unless ZK_OANDA_TOKEN is set. Runs against OANDA practice environment.

Usage:
    source devops/scripts/load-oanda-token.sh
    uv run --project venue-integrations/oanda pytest tests/test_live_harness.py -v -s
"""

from __future__ import annotations

import asyncio
import os
import time

import pytest

from oanda.gw import OandaGatewayAdaptor

from zk.exch_gw.v1 import exch_gw_pb2 as gw_pb

OANDA_TOKEN = os.environ.get("ZK_OANDA_TOKEN", "")
OANDA_ACCOUNT_ID = os.environ.get("ZK_OANDA_ACCOUNT_ID", "101-003-26138765-001")

pytestmark = pytest.mark.skipif(not OANDA_TOKEN, reason="ZK_OANDA_TOKEN not set")


def _parse_report(event: dict) -> gw_pb.OrderReport:
    report = gw_pb.OrderReport()
    report.ParseFromString(event["payload_bytes"])
    return report


@pytest.fixture
async def adaptor():
    """Create and connect a live adaptor, shut down after test."""
    a = OandaGatewayAdaptor({
        "environment": "practice",
        "account_id": OANDA_ACCOUNT_ID,
        "token": OANDA_TOKEN,
    })
    await a.connect()
    yield a
    await a.shutdown()


class TestLiveHarness:
    @pytest.mark.asyncio
    async def test_connect_and_query_balance(self, adaptor):
        """Connect and query account balance."""
        facts = await adaptor.query_balance({})
        assert len(facts) == 1
        assert len(facts[0]["asset"]) == 3  # ISO currency code
        assert facts[0]["total_qty"] > 0
        print(f"\nBalance: {facts[0]}")

    @pytest.mark.asyncio
    async def test_query_positions(self, adaptor):
        """Query current positions."""
        facts = await adaptor.query_positions({})
        print(f"\nPositions ({len(facts)}): {facts}")

    @pytest.mark.asyncio
    async def test_query_open_orders(self, adaptor):
        """Query pending orders."""
        facts = await adaptor.query_open_orders({})
        print(f"\nOpen orders ({len(facts)}): {facts}")

    @pytest.mark.asyncio
    async def test_limit_order_lifecycle(self, adaptor):
        """Place a limit order far from market, query it, cancel it.

        Uses EUR_USD with a price 10% below current to ensure no fill.
        """
        # Get current pricing via a balance query (just to verify connectivity)
        balance = await adaptor.query_balance({})
        assert len(balance) == 1

        # Place a limit buy far from market
        correlation_id = int(time.time() * 1000) % 2**31
        ack = await adaptor.place_order({
            "correlation_id": correlation_id,
            "instrument": "EUR_USD",
            "qty": 1,
            "buysell_type": 1,  # Buy
            "order_type": 1,    # Limit
            "price": 0.5000,    # Far below market
        })
        print(f"\nPlace order ack: {ack}")
        assert ack["success"] is True
        assert ack["exch_order_ref"] is not None
        exch_ref = ack["exch_order_ref"]

        # Drain the queued event(s) from place_order
        events = []
        deadline = time.time() + 2.0
        while time.time() < deadline:
            try:
                event = await asyncio.wait_for(adaptor.next_event(), timeout=0.5)
                events.append(event)
            except asyncio.TimeoutError:
                break
        print(f"Events after place: {len(events)}")
        for e in events:
            if e["event_type"] == "order_report":
                report = _parse_report(e)
                print(f"  OrderReport: ref={report.exch_order_ref} entries={len(report.order_report_entries)}")

        # Query the order
        order_facts = await adaptor.query_order({"exch_order_ref": exch_ref})
        assert len(order_facts) == 1
        assert order_facts[0]["status"] == "booked"
        print(f"Order query: {order_facts[0]}")

        # Cancel the order
        cancel_ack = await adaptor.cancel_order({"exch_order_ref": exch_ref})
        print(f"Cancel ack: {cancel_ack}")
        assert cancel_ack["success"] is True

        # Drain cancel event
        cancel_events = []
        deadline = time.time() + 2.0
        while time.time() < deadline:
            try:
                event = await asyncio.wait_for(adaptor.next_event(), timeout=0.5)
                cancel_events.append(event)
            except asyncio.TimeoutError:
                break
        print(f"Events after cancel: {len(cancel_events)}")
        for e in cancel_events:
            if e["event_type"] == "order_report":
                report = _parse_report(e)
                for entry in report.order_report_entries:
                    if entry.HasField("order_state_report"):
                        assert entry.order_state_report.exch_order_status == gw_pb.EXCH_ORDER_STATUS_CANCELLED
                        print("  Confirmed: order cancelled via protobuf")

        # Verify order no longer in open orders
        open_orders = await adaptor.query_open_orders({})
        refs = [o["exch_order_ref"] for o in open_orders]
        assert exch_ref not in refs
        print("Lifecycle complete: place → query → cancel → verify")
