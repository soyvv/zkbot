"""OANDA gateway adaptor.

Implements the Python venue adaptor contract for the OANDA v20 API.
Designed to be loaded by the Rust ``zk-gw-svc`` host via the ``zk-pyo3-bridge``.

Uses REST for commands/queries and the transaction stream for near-real-time events.
Operates in hybrid mode with query-after-action reconciliation.

Bridge constraints:
- All async methods run on one persistent Python event loop managed by ``zk-pyo3-bridge``.
- Do NOT call ``asyncio.run()`` inside this adaptor.
- Use an internal ``asyncio.Queue`` for events; ``next_event()`` awaits it.
"""

from __future__ import annotations

import asyncio
from collections import OrderedDict

from loguru import logger

from .client import OandaApiError, OandaRestClient
from .stream import OandaTransactionStream
from . import normalize as norm

_API_URLS = {
    "practice": "https://api-fxpractice.oanda.com",
    "live": "https://api-fxtrade.oanda.com",
}
_STREAM_URLS = {
    "practice": "https://stream-fxpractice.oanda.com",
    "live": "https://stream-fxtrade.oanda.com",
}

_QUERY_AFTER_ACTION_DELAY = 0.5
_RECONCILE_INTERVAL = 30.0
_MAX_SEEN_TXNS = 10_000


class OandaGatewayAdaptor:
    """OANDA venue adaptor loaded by the Rust gateway host.

    Constructor receives ``config: dict`` from ``ZK_VENUE_CONFIG`` (validated
    against ``schemas/gw_config.schema.json`` by the Rust bridge).

    Config keys:
        environment: "practice" or "live"
        account_id: OANDA account ID string
        token: API bearer token (resolved from secret_ref by the host)
        api_base_url: (optional) override REST base URL
        stream_base_url: (optional) override stream base URL
        oms_account_id: (optional) integer OMS account_id for position facts
    """

    def __init__(self, config: dict) -> None:
        env = config.get("environment", "practice")
        self._oanda_account_id = config["account_id"]
        self._token = config.get("token") or config["secret_ref"]
        self._oms_account_id = int(config.get("oms_account_id", 0))
        self._api_base_url = config.get("api_base_url") or _API_URLS[env]
        self._stream_base_url = config.get("stream_base_url") or _STREAM_URLS[env]
        self._event_queue: asyncio.Queue = asyncio.Queue(maxsize=4096)
        self._client: OandaRestClient | None = None
        self._stream: OandaTransactionStream | None = None
        self._stream_task: asyncio.Task | None = None
        self._reconcile_task: asyncio.Task | None = None
        self._connected = False
        # Dedup order events emitted from both REST responses and the transaction
        # stream.  Keyed by OANDA transaction ID; bounded FIFO eviction.
        self._seen_txn_ids: OrderedDict[str, None] = OrderedDict()

    @property
    def connected(self) -> bool:
        return self._connected

    async def connect(self) -> None:
        """Establish connectivity: validate account, start transaction stream."""
        self._client = OandaRestClient(
            api_base_url=self._api_base_url,
            token=self._token,
            account_id=self._oanda_account_id,
        )
        summary = await self._client.get_account_summary()
        acct = summary.get("account", {})
        logger.info(
            f"OANDA connected: account={self._oanda_account_id} "
            f"currency={acct.get('currency', '?')} balance={acct.get('balance', '?')}"
        )
        self._stream = OandaTransactionStream(
            stream_base_url=self._stream_base_url,
            token=self._token,
            account_id=self._oanda_account_id,
            event_queue=self._event_queue,
            emit_order_event=self._emit_order_event,
        )
        self._stream_task = asyncio.create_task(self._stream.run(), name="oanda-txn-stream")
        # Wait for the stream to confirm its first connection before reporting ready.
        stream_ok = await self._stream.wait_connected(timeout=15.0)
        if not stream_ok:
            logger.warning("OANDA transaction stream did not connect within 15s")
            raise ConnectionError("OANDA transaction stream failed to connect")
        self._reconcile_task = asyncio.create_task(
            self._periodic_reconcile(), name="oanda-reconcile"
        )
        self._connected = True

    async def shutdown(self) -> None:
        if self._stream:
            await self._stream.stop()
        for task in (self._stream_task, self._reconcile_task):
            if task:
                task.cancel()
                try:
                    await task
                except asyncio.CancelledError:
                    pass
        if self._client:
            await self._client.close()
        self._connected = False
        logger.info("OANDA adaptor shut down")

    # ── commands ─────────────────────────────────────────────────────────────

    async def place_order(self, req: dict) -> dict:
        assert self._client is not None
        correlation_id = req.get("correlation_id", 0)
        instrument = req["instrument"]
        qty = req["qty"]
        buysell = req.get("buysell_type", 1)
        order_type = req.get("order_type", 0)
        price = req.get("price", 0.0)

        units = str(qty if buysell == 1 else -qty)
        order_body: dict = {
            "instrument": instrument,
            "units": units,
            "clientExtensions": {"id": str(correlation_id)},
        }
        if order_type == 1 and price > 0:
            order_body["type"] = "LIMIT"
            order_body["price"] = str(price)
            order_body["timeInForce"] = "GTC"
        else:
            order_body["type"] = "MARKET"
            order_body["timeInForce"] = "FOK"

        try:
            resp = await self._client.place_order(order_body)
        except OandaApiError as e:
            logger.warning(f"OANDA place_order failed: {e}")
            # Return unsuccessful ack only. The Rust host synthesizes a rejection
            # report from the failed ack; do NOT also enqueue a reject event here.
            return {"success": False, "exch_order_ref": None, "error_message": str(e)}

        exch_order_ref = None
        create_txn = resp.get("orderCreateTransaction")
        if create_txn:
            exch_order_ref = str(create_txn.get("id", ""))
            await self._emit_order_event(norm.order_create_event(create_txn), create_txn)

        fill_txn = resp.get("orderFillTransaction")
        if fill_txn:
            exch_order_ref = exch_order_ref or str(fill_txn.get("orderID", ""))
            await self._emit_order_event(norm.order_fill_event(fill_txn), fill_txn)

        cancel_txn = resp.get("orderCancelTransaction")
        if cancel_txn:
            await self._emit_order_event(norm.order_cancel_event(cancel_txn), cancel_txn)

        if create_txn and not fill_txn and not cancel_txn and exch_order_ref:
            asyncio.create_task(
                self._query_after_action(exch_order_ref), name=f"qaa-{exch_order_ref}"
            )
        return {"success": True, "exch_order_ref": exch_order_ref, "error_message": None}

    async def cancel_order(self, req: dict) -> dict:
        assert self._client is not None
        exch_order_ref = req.get("exch_order_ref", "")
        try:
            resp = await self._client.cancel_order(exch_order_ref)
        except OandaApiError as e:
            logger.warning(f"OANDA cancel_order failed: {e}")
            return {"success": False, "exch_order_ref": exch_order_ref, "error_message": str(e)}
        cancel_txn = resp.get("orderCancelTransaction")
        if cancel_txn:
            await self._emit_order_event(norm.order_cancel_event(cancel_txn), cancel_txn)
        return {"success": True, "exch_order_ref": exch_order_ref, "error_message": None}

    # ── queries ──────────────────────────────────────────────────────────────

    async def query_balance(self, req: dict) -> list[dict]:
        assert self._client is not None
        summary = await self._client.get_account_summary()
        return norm.normalize_balance(summary)

    async def query_order(self, req: dict) -> list[dict]:
        assert self._client is not None
        ref = req.get("exch_order_ref")
        order_id = req.get("order_id")

        if ref:
            resp = await self._client.get_order(ref)
            order = resp.get("order")
            return [norm.normalize_order(order)] if order else []

        if order_id:
            # OANDA has no native order_id lookup; scan pending + recent orders
            # by matching clientExtensions.id == str(order_id).
            resp = await self._client.get_pending_orders()
            for o in resp.get("orders", []):
                if norm.extract_client_order_id(o) == order_id:
                    return [norm.normalize_order(o)]

        return []

    async def query_open_orders(self, req: dict) -> list[dict]:
        assert self._client is not None
        resp = await self._client.get_pending_orders()
        results = norm.normalize_orders(resp)

        instrument = req.get("instrument")
        if instrument:
            results = [o for o in results if o.get("instrument") == instrument]

        limit = req.get("limit")
        if limit and limit > 0:
            results = results[:limit]

        return results

    async def query_trades(self, req: dict) -> list[dict]:
        assert self._client is not None
        limit = req.get("limit")
        resp = await self._client.get_trades(
            instrument=req.get("instrument"),
            count=limit,
            before_id=req.get("page_cursor"),
        )
        results = norm.normalize_trades(resp)

        # Client-side time filtering (OANDA /trades doesn't support time range natively)
        start_ts = req.get("start_ts")
        end_ts = req.get("end_ts")
        if start_ts:
            results = [t for t in results if t.get("timestamp", 0) >= start_ts]
        if end_ts:
            results = [t for t in results if t.get("timestamp", 0) <= end_ts]

        if limit and limit > 0:
            results = results[:limit]

        return results

    async def query_funding_fees(self, req: dict) -> list[dict]:
        """OANDA does not have explicit funding fees — return empty."""
        return []

    async def query_positions(self, req: dict) -> list[dict]:
        assert self._client is not None
        resp = await self._client.get_positions()
        return norm.normalize_positions(resp, self._oms_account_id)

    async def next_event(self) -> dict:
        return await self._event_queue.get()

    # ── internal ─────────────────────────────────────────────────────────────

    async def _emit_order_event(self, event: dict, txn: dict) -> None:
        """Emit an order event, deduplicating by OANDA transaction ID.

        Both REST command responses and the transaction stream call this so
        the same logical event is only delivered once to the downstream consumer.
        """
        txn_id = str(txn.get("id", ""))
        if not txn_id:
            await self._event_queue.put(event)
            return
        if txn_id in self._seen_txn_ids:
            logger.debug(f"dedup: skipping duplicate order event txn_id={txn_id}")
            return
        self._seen_txn_ids[txn_id] = None
        while len(self._seen_txn_ids) > _MAX_SEEN_TXNS:
            self._seen_txn_ids.popitem(last=False)
        await self._event_queue.put(event)

    async def _query_after_action(self, exch_order_ref: str) -> None:
        await asyncio.sleep(_QUERY_AFTER_ACTION_DELAY)
        try:
            assert self._client is not None
            resp = await self._client.get_order(exch_order_ref)
            order = resp.get("order")
            if order and order.get("state") in ("FILLED", "CANCELLED"):
                logger.debug(f"qaa: state={order['state']} for {exch_order_ref}")
                fact = norm.normalize_order(order)
                event = norm.order_state_event(
                    exch_order_ref=fact["exch_order_ref"],
                    order_id=fact["order_id"],
                    status=fact["status_enum"],
                    filled_qty=fact["filled_qty"],
                    unfilled_qty=fact["unfilled_qty"],
                    avg_price=fact["avg_price"],
                    instrument=fact.get("instrument", ""),
                )
                await self._event_queue.put(event)
        except Exception:
            logger.exception(f"query-after-action failed for {exch_order_ref}")

    async def _periodic_reconcile(self) -> None:
        while True:
            await asyncio.sleep(_RECONCILE_INTERVAL)
            try:
                if not self._client or not self._stream:
                    continue
                last_txn = self._stream.last_transaction_id
                if not last_txn:
                    continue
                resp = await self._client.get_account_changes(last_txn)
                changes = resp.get("changes", {})

                # Emit order-fill events for any fills missed by the stream
                for fill in changes.get("ordersFilled", []):
                    logger.info(f"reconcile: emitting missed fill for order {fill.get('id')}")
                    fact = norm.normalize_order(fill)
                    event = norm.order_state_event(
                        exch_order_ref=fact["exch_order_ref"],
                        order_id=fact["order_id"],
                        status=fact["status_enum"],
                        filled_qty=fact["filled_qty"],
                        unfilled_qty=fact["unfilled_qty"],
                        avg_price=fact["avg_price"],
                        instrument=fact.get("instrument", ""),
                    )
                    await self._event_queue.put(event)

                # Emit cancel events for orders cancelled while stream was down
                for cancelled in changes.get("ordersCancelled", []):
                    logger.info(f"reconcile: emitting missed cancel for order {cancelled.get('id')}")
                    fact = norm.normalize_order(cancelled)
                    event = norm.order_state_event(
                        exch_order_ref=fact["exch_order_ref"],
                        order_id=fact["order_id"],
                        status=fact["status_enum"],
                        filled_qty=fact["filled_qty"],
                        unfilled_qty=fact["unfilled_qty"],
                        avg_price=fact["avg_price"],
                        instrument=fact.get("instrument", ""),
                    )
                    await self._event_queue.put(event)

                # Emit balance snapshot from reconciled account state
                state = resp.get("state", {})
                if state and float(state.get("balance", 0)) > 0:
                    await self._event_queue.put({
                        "event_type": "balance",
                        "payload": [{
                            "asset": state.get("currency", "USD"),
                            "total_qty": float(state.get("balance", 0)),
                            "avail_qty": float(state.get("marginAvailable", 0)),
                            "frozen_qty": float(state.get("marginUsed", 0)),
                        }],
                    })

                # Update stream watermark if response provides it
                last_id = resp.get("lastTransactionID")
                if last_id:
                    self._stream._last_transaction_id = last_id
            except asyncio.CancelledError:
                raise
            except Exception:
                logger.exception("periodic reconciliation error")
