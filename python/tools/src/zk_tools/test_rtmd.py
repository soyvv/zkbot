#!/usr/bin/env python3
"""Test RTMD subscription for tick (BBO), kline, and orderbook channels.

Flow:
  1. Connect to NATS
  2. Write a subscription lease to KV bucket `zk-rtmd-subs-v1`
  3. Subscribe to the deterministic NATS subject for the chosen channel
  4. Print decoded protobuf messages as they arrive
  5. On exit, delete the lease (clean up)

NATS subject patterns (venue is uppercased):
  tick:      zk.rtmd.tick.<VENUE>.<instrument>
  kline:     zk.rtmd.kline.<VENUE>.<instrument>.<interval>
  orderbook: zk.rtmd.orderbook.<VENUE>.<instrument>

Requires:
  - RTMD gateway running for the target venue
  - `nats-py` and `protobuf` packages

Usage:
  uv run tools/test_rtmd.py --venue oanda --instrument EUR_USD --channel tick
  uv run tools/test_rtmd.py --venue oanda --instrument EUR_USD --channel kline --interval 1m
  uv run tools/test_rtmd.py --venue okx --instrument BTC-USDT --channel orderbook
  uv run tools/test_rtmd.py --venue okx --instrument BTC-USDT  # defaults to tick
"""

import argparse
import asyncio
import json
import signal
import time

import nats
from nats.js.kv import KeyValue

try:
    from zk.rtmd.v1 import rtmd_pb2  # type: ignore[import-untyped]

    HAS_PROTO = True
except ImportError:
    HAS_PROTO = False


# ── Decoders ────────────────────────────────────────────────────────────────

def decode_tick(data: bytes) -> dict:
    """Decode a TickData protobuf message into a readable dict."""
    if HAS_PROTO:
        tick = rtmd_pb2.TickData()
        tick.ParseFromString(data)
        bids = [{"px": l.price, "qty": l.qty} for l in tick.buy_price_levels]
        asks = [{"px": l.price, "qty": l.qty} for l in tick.sell_price_levels]
        return {
            "type": "tick",
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
    return {"type": "tick", "raw_bytes": len(data), "hex_head": data[:40].hex()}


def decode_kline(data: bytes) -> dict:
    """Decode a Kline protobuf message into a readable dict."""
    if HAS_PROTO:
        k = rtmd_pb2.Kline()
        k.ParseFromString(data)
        return {
            "type": "kline",
            "symbol": k.symbol,
            "source": k.source,
            "ts": k.timestamp,
            "ts_end": k.kline_end_timestamp,
            "O": k.open,
            "H": k.high,
            "L": k.low,
            "C": k.close,
            "V": k.volume,
            "kline_type": k.kline_type,
        }
    return {"type": "kline", "raw_bytes": len(data), "hex_head": data[:40].hex()}


def decode_orderbook(data: bytes) -> dict:
    """Decode an OrderBook protobuf message into a readable dict."""
    if HAS_PROTO:
        ob = rtmd_pb2.OrderBook()
        ob.ParseFromString(data)
        bids = [{"px": l.price, "qty": l.qty} for l in ob.buy_levels]
        asks = [{"px": l.price, "qty": l.qty} for l in ob.sell_levels]
        return {
            "type": "orderbook",
            "instrument": ob.instrument_code,
            "exchange": ob.exchange,
            "ts_ms": ob.timestamp_ms,
            "bids": bids[:5],
            "asks": asks[:5],
        }
    return {"type": "orderbook", "raw_bytes": len(data), "hex_head": data[:40].hex()}


DECODERS = {
    "tick": decode_tick,
    "kline": decode_kline,
    "orderbook": decode_orderbook,
}


def format_msg(d: dict) -> str:
    """Format a decoded message dict into a one-line summary."""
    mtype = d.get("type", "?")
    if mtype == "tick":
        ts_str = f"  ts={d['ts']}" if d.get("ts") else ""
        bid_str = f"  bid={d['bid'][0]['px']}" if d.get("bid") else ""
        ask_str = f"  ask={d['ask'][0]['px']}" if d.get("ask") else ""
        last_str = f"  last={d.get('last_px', '?')}"
        return f"{d.get('instrument', '?')}{ts_str}{bid_str}{ask_str}{last_str}"
    elif mtype == "kline":
        return (
            f"{d.get('symbol', '?')}  ts={d.get('ts', '?')}"
            f"  O={d.get('O')} H={d.get('H')} L={d.get('L')} C={d.get('C')}"
            f"  V={d.get('V')}"
        )
    elif mtype == "orderbook":
        best_bid = d["bids"][0]["px"] if d.get("bids") else "?"
        best_ask = d["asks"][0]["px"] if d.get("asks") else "?"
        depth = max(len(d.get("bids", [])), len(d.get("asks", [])))
        return (
            f"{d.get('instrument', '?')}  ts={d.get('ts_ms', '?')}"
            f"  bid={best_bid}  ask={best_ask}  depth={depth}"
        )
    return str(d)


# ── NATS subject builders (mirrors Rust canonical_venue + subject module) ───

def build_nats_subject(
    channel: str, venue: str, instrument: str, interval: str | None = None,
) -> str:
    v = venue.upper()
    if channel == "tick":
        return f"zk.rtmd.tick.{v}.{instrument}"
    elif channel == "kline":
        return f"zk.rtmd.kline.{v}.{instrument}.{interval or '1m'}"
    elif channel == "orderbook":
        return f"zk.rtmd.orderbook.{v}.{instrument}"
    raise ValueError(f"unknown channel: {channel}")


# ── Subscription lease helpers ──────────────────────────────────────────────

SUB_BUCKET = "zk-rtmd-subs-v1"
SUBSCRIBER_ID = "test_rtmd"
LEASE_TTL_S = 60


def make_subscription_id(
    channel: str, instrument: str, interval: str | None = None,
) -> str:
    inst = instrument.replace("-", "_").replace("/", "_").lower()
    if channel == "kline":
        return f"kline_{interval or '1m'}_{inst}"
    return f"{channel}_{inst}"


def make_lease(
    venue: str, instrument: str, channel: str, interval: str | None, expiry_ms: int,
) -> bytes:
    return json.dumps({
        "subscriber_id": SUBSCRIBER_ID,
        "scope": f"venue.{venue}",
        "instrument_id": instrument,
        "channel_type": channel,
        "channel_param": interval if channel == "kline" else None,
        "venue": venue,
        "instrument_exch": instrument,
        "lease_expiry_ms": expiry_ms,
    }).encode()


# ── Main ────────────────────────────────────────────────────────────────────

async def main(
    nats_url: str,
    venue: str,
    instrument: str,
    channel: str,
    interval: str | None,
    duration: int,
) -> None:
    nc = await nats.connect(nats_url)
    js = nc.jetstream()
    print(f"connected to NATS at {nats_url}")

    # 1. Ensure KV bucket exists
    try:
        kv: KeyValue = await js.key_value(SUB_BUCKET)
    except Exception:
        kv = await js.create_key_value(
            config=nats.js.kv.KeyValueConfig(bucket=SUB_BUCKET, ttl=LEASE_TTL_S * 3)
        )
    print(f"KV bucket: {SUB_BUCKET}")

    # 2. Write subscription lease
    subscription_id = make_subscription_id(channel, instrument, interval)
    kv_key = f"sub.venue.{venue}.{SUBSCRIBER_ID}.{subscription_id}"
    expiry_ms = int((time.time() + LEASE_TTL_S) * 1000)
    await kv.put(kv_key, make_lease(venue, instrument, channel, interval, expiry_ms))
    print(f"lease written: {kv_key}")

    # 3. Subscribe to the deterministic NATS subject
    subject = build_nats_subject(channel, venue, instrument, interval)
    print(f"subscribing to: {subject}")
    label = channel if channel != "kline" else f"kline/{interval or '1m'}"
    print(f"waiting for {label} messages (max {duration}s)...\n")

    sub = await nc.subscribe(subject)
    count = 0
    shutdown = asyncio.Event()
    decoder = DECODERS[channel]

    def on_signal():
        shutdown.set()

    loop = asyncio.get_event_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(sig, on_signal)

    # 4. Refresh lease periodically + receive messages
    async def refresh_lease():
        while not shutdown.is_set():
            await asyncio.sleep(LEASE_TTL_S / 3)
            if shutdown.is_set():
                break
            exp = int((time.time() + LEASE_TTL_S) * 1000)
            await kv.put(kv_key, make_lease(venue, instrument, channel, interval, exp))

    refresh_task = asyncio.create_task(refresh_lease())

    deadline = time.monotonic() + duration
    try:
        while time.monotonic() < deadline and not shutdown.is_set():
            try:
                msg = await asyncio.wait_for(sub.next_msg(), timeout=1.0)
            except asyncio.TimeoutError:
                continue
            count += 1
            decoded = decoder(msg.data)
            print(f"[{count:4d}] {format_msg(decoded)}")
    finally:
        shutdown.set()
        refresh_task.cancel()

    # 5. Cleanup
    try:
        await kv.delete(kv_key)
        print(f"\nlease deleted: {kv_key}")
    except Exception as e:
        print(f"\nlease cleanup failed: {e}")

    await sub.unsubscribe()
    await nc.close()
    print(f"done — received {count} {channel} messages")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Test RTMD subscription (tick/kline/orderbook)")
    parser.add_argument("--nats-url", default="nats://localhost:4222")
    parser.add_argument("--venue", default="okx")
    parser.add_argument("--instrument", default="BTC-USDT")
    parser.add_argument("--channel", choices=["tick", "kline", "orderbook"], default="tick")
    parser.add_argument("--interval", default="1m", help="kline interval (1m,5m,15m,30m,1h,4h,1d)")
    parser.add_argument("--duration", type=int, default=30, help="seconds to run")
    args = parser.parse_args()

    asyncio.run(
        main(args.nats_url, args.venue, args.instrument, args.channel, args.interval, args.duration)
    )
