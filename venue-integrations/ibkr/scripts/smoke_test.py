"""Smoke test: connect to a real TWS/IB Gateway, test GW + RTMD + Refdata."""

import asyncio
import sys
import time

import ib_async


async def test_connection(host: str, port: int, client_id: int = 99) -> ib_async.IB:
    """Phase 1: Basic connection."""
    print(f"\n{'='*60}")
    print(f"Phase 1: Connecting to {host}:{port} (clientId={client_id})")
    print(f"{'='*60}")

    ib = ib_async.IB()
    await ib.connectAsync(host, port, clientId=client_id, readonly=True)
    print(f"  Connected: {ib.isConnected()}")

    accounts = ib.managedAccounts()
    print(f"  Accounts: {accounts}")

    return ib


async def test_rtmd(ib: ib_async.IB, symbol: str = "AAPL") -> None:
    """Phase 2: Subscribe to tick data, print a few updates."""
    print(f"\n{'='*60}")
    print(f"Phase 2: RTMD — subscribing to {symbol} ticks")
    print(f"{'='*60}")

    # Use delayed data (type 3) to avoid needing live market data subscription
    ib.reqMarketDataType(3)
    print("  Using delayed market data (type 3)")

    contract = ib_async.Stock(symbol, "SMART", "USD")
    qualified = await ib.qualifyContractsAsync(contract)
    if not qualified:
        print(f"  FAIL: could not qualify {symbol}")
        return
    contract = qualified[0]
    print(f"  Qualified: conId={contract.conId} exchange={contract.exchange}")

    ticker = ib.reqMktData(contract)
    print("  reqMktData sent, waiting for ticks...")

    ticks_received = 0
    last_printed = None
    deadline = time.monotonic() + 15  # wait up to 15s for delayed data
    while time.monotonic() < deadline and ticks_received < 5:
        await asyncio.sleep(1.0)
        bid = getattr(ticker, "bid", None)
        ask = getattr(ticker, "ask", None)
        last = getattr(ticker, "last", None)
        vol = getattr(ticker, "volume", None)
        state = (bid, ask, last, vol)
        # Print when any field changes
        if state != last_printed and any(
            v is not None and v == v and v != 0 for v in state
        ):
            ticks_received += 1
            print(f"  Tick #{ticks_received}: bid={bid} ask={ask} last={last} vol={vol}")
            last_printed = state

    if ticks_received == 0:
        print(
            f"  No ticks in 15s. Current state: bid={ticker.bid} ask={ticker.ask} "
            f"last={ticker.last} vol={ticker.volume}"
        )

    ib.cancelMktData(contract)
    print(f"  Cancelled market data for {symbol}")


async def test_refdata(ib: ib_async.IB) -> None:
    """Phase 3: Qualify contracts and print refdata fields."""
    print(f"\n{'='*60}")
    print("Phase 3: Refdata — qualifying contracts")
    print(f"{'='*60}")

    specs = [
        ("AAPL", "STK", "SMART", "USD"),
        ("MSFT", "STK", "SMART", "USD"),
        ("ES", "FUT", "CME", "USD"),
    ]

    for symbol, sec_type, exchange, currency in specs:
        if sec_type == "STK":
            contract = ib_async.Stock(symbol, exchange, currency)
        elif sec_type == "FUT":
            contract = ib_async.Future(symbol, exchange=exchange, currency=currency)
            contract.includeExpired = False
        else:
            continue

        try:
            qualified = await ib.qualifyContractsAsync(contract)
            if not qualified:
                print(f"  {symbol} ({sec_type}): qualification returned empty")
                continue
            contract = qualified[0]

            details_list = await ib.reqContractDetailsAsync(contract)
            if not details_list:
                print(f"  {symbol} ({sec_type}): no contract details")
                continue

            d = details_list[0]
            c = d.contract
            print(f"\n  {symbol} ({sec_type}):")
            print(f"    conId:          {c.conId}")
            print(f"    symbol:         {c.symbol}")
            print(f"    secType:        {c.secType}")
            print(f"    exchange:       {c.exchange}")
            print(f"    primaryExch:    {c.primaryExchange}")
            print(f"    currency:       {c.currency}")
            print(f"    minTick:        {d.minTick}")
            print(f"    multiplier:     {d.contract.multiplier}")
            print(f"    longName:       {d.longName}")
            print(f"    timeZoneId:     {d.timeZoneId}")
            lh = d.liquidHours or ""
            print(f"    liquidHours:    {lh[:80]}{'...' if len(lh) > 80 else ''}")
            th = d.tradingHours or ""
            print(f"    tradingHours:   {th[:80]}{'...' if len(th) > 80 else ''}")

        except Exception as exc:
            print(f"  {symbol} ({sec_type}): ERROR — {exc}")


async def main() -> None:
    host = sys.argv[1] if len(sys.argv) > 1 else "192.168.1.136"
    port = int(sys.argv[2]) if len(sys.argv) > 2 else 4004

    ib = await test_connection(host, port)
    try:
        await test_rtmd(ib)
        await test_refdata(ib)
    finally:
        ib.disconnect()
        print(f"\n{'='*60}")
        print("Disconnected. Smoke test complete.")
        print(f"{'='*60}")


if __name__ == "__main__":
    asyncio.run(main())
