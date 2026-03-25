#!/usr/bin/env python3
"""Test BBO (tick) subscription via the RTMD subscription protocol.

Flow:
  1. Connect to NATS
  2. Write a subscription lease to KV bucket `zk-rtmd-subs-v1`
  3. Subscribe to the deterministic NATS subject `zk.rtmd.tick.<venue>.<instrument_exch>`
  4. Print decoded TickData protobuf messages as they arrive
  5. On exit, delete the lease (clean up)

Requires:
  - RTMD gateway running (`make rtmd-okx-demo-pilot` or `make rtmd-okx-demo`)
  - `nats-py` and `protobuf` packages

Usage:
  uv run tools/test_rtmd_bbo.py                          # BTC-USDT on OKX (default)
  uv run tools/test_rtmd_bbo.py --instrument ETH-USDT
  uv run tools/test_rtmd_bbo.py --venue simulator --instrument BTC-USDT
"""

import argparse
import asyncio
import json
import signal
import time

import nats
from nats.js.kv import KeyValue

# Inline protobuf decoding — avoids depending on generated pb2 files.
# TickData wire format (proto3, field numbers from rtmd.proto):
#   1: tick_type (varint)    2: instrument_code (string)
#   3: exchange (string)     4: original_timestamp (varint)
#   5: received_timestamp    7: volume (double)
#   8: latest_trade_price    9: latest_trade_qty
#  10: latest_trade_side    14: buy_price_levels   15: sell_price_levels
try:
    import sys, importlib, pathlib

    # Try to use the generated protobuf bindings if available.
    _gen_root = pathlib.Path(__file__).resolve().parent / ".trade_doctor_generated"
    if _gen_root.exists():
        sys.path.insert(0, str(_gen_root))
    from zk.rtmd.v1 import rtmd_pb2  # type: ignore[import-untyped]

    HAS_PROTO = True
except ImportError:
    HAS_PROTO = False


def decode_tick(data: bytes) -> dict:
    """Decode a TickData protobuf message into a readable dict."""
    if HAS_PROTO:
        tick = rtmd_pb2.TickData()
        tick.ParseFromString(data)
        bids = [{"px": l.price, "qty": l.qty} for l in tick.buy_price_levels]
        asks = [{"px": l.price, "qty": l.qty} for l in tick.sell_price_levels]
        return {
            "instrument": tick.instrument_code,
            "exchange": tick.exchange,
            "ts": tick.original_timestamp,
            "last_px": tick.latest_trade_price,
            "last_qty": tick.latest_trade_qty,
            "last_side": tick.latest_trade_side,
            "volume": tick.volume,
            "bid": bids[:1],
            "ask": asks[:1],
        }
    # Fallback: raw hex
    return {"raw_bytes": len(data), "hex_head": data[:40].hex()}


SUB_BUCKET = "zk-rtmd-subs-v1"
SUBSCRIBER_ID = "test_rtmd_bbo"
LEASE_TTL_S = 60


async def main(
    nats_url: str,
    venue: str,
    instrument: str,
    duration: int,
) -> None:
    nc = await nats.connect(nats_url)
    js = nc.jetstream()
    print(f"connected to NATS at {nats_url}")

    # ── 1. Ensure KV bucket exists ──────────────────────────────────────────
    try:
        kv: KeyValue = await js.key_value(SUB_BUCKET)
    except Exception:
        kv = await js.create_key_value(
            config=nats.js.kv.KeyValueConfig(
                bucket=SUB_BUCKET,
                ttl=LEASE_TTL_S * 3,
            )
        )
    print(f"KV bucket: {SUB_BUCKET}")

    # ── 2. Write subscription lease ─────────────────────────────────────────
    subscription_id = f"tick_{instrument.replace('-', '_').lower()}"
    kv_key = f"sub.venue.{venue}.{SUBSCRIBER_ID}.{subscription_id}"
    lease_value = json.dumps({
        "subscriber_id": SUBSCRIBER_ID,
        "scope": f"venue.{venue}",
        "instrument_id": instrument,
        "channel_type": "tick",
        "channel_param": None,
        "venue": venue,
        "instrument_exch": instrument,
        "lease_expiry_ms": int((time.time() + LEASE_TTL_S) * 1000),
    }).encode()

    await kv.put(kv_key, lease_value)
    print(f"lease written: {kv_key}")

    # ── 3. Subscribe to the deterministic NATS subject ──────────────────────
    nats_subject = f"zk.rtmd.tick.{venue}.{instrument}"
    print(f"subscribing to: {nats_subject}")
    print(f"waiting for ticks (max {duration}s)...\n")

    sub = await nc.subscribe(nats_subject)
    count = 0
    shutdown = asyncio.Event()

    def on_signal():
        shutdown.set()

    loop = asyncio.get_event_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(sig, on_signal)

    # ── 4. Refresh lease periodically + receive ticks ───────────────────────
    async def refresh_lease():
        while not shutdown.is_set():
            await asyncio.sleep(LEASE_TTL_S / 3)
            if shutdown.is_set():
                break
            refreshed = json.dumps({
                "subscriber_id": SUBSCRIBER_ID,
                "scope": f"venue.{venue}",
                "instrument_id": instrument,
                "channel_type": "tick",
                "channel_param": None,
                "venue": venue,
                "instrument_exch": instrument,
                "lease_expiry_ms": int((time.time() + LEASE_TTL_S) * 1000),
            }).encode()
            await kv.put(kv_key, refreshed)

    refresh_task = asyncio.create_task(refresh_lease())

    deadline = time.monotonic() + duration
    try:
        while time.monotonic() < deadline and not shutdown.is_set():
            try:
                msg = await asyncio.wait_for(sub.next_msg(), timeout=1.0)
            except asyncio.TimeoutError:
                continue
            count += 1
            tick = decode_tick(msg.data)
            ts_str = ""
            if "ts" in tick and tick["ts"]:
                ts_str = f"  ts={tick['ts']}"
            bid_str = f"  bid={tick['bid'][0]['px']}" if tick.get("bid") else ""
            ask_str = f"  ask={tick['ask'][0]['px']}" if tick.get("ask") else ""
            last_str = f"  last={tick.get('last_px', '?')}"
            print(f"[{count:4d}] {tick.get('instrument', '?')}{ts_str}{bid_str}{ask_str}{last_str}")
    finally:
        shutdown.set()
        refresh_task.cancel()

    # ── 5. Cleanup ──────────────────────────────────────────────────────────
    try:
        await kv.delete(kv_key)
        print(f"\nlease deleted: {kv_key}")
    except Exception as e:
        print(f"\nlease cleanup failed: {e}")

    await sub.unsubscribe()
    await nc.close()
    print(f"done — received {count} ticks")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Test RTMD BBO subscription")
    parser.add_argument("--nats-url", default="nats://localhost:4222")
    parser.add_argument("--venue", default="okx")
    parser.add_argument("--instrument", default="BTC-USDT")
    parser.add_argument("--duration", type=int, default=30, help="seconds to run")
    args = parser.parse_args()

    asyncio.run(main(args.nats_url, args.venue, args.instrument, args.duration))
