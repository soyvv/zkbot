# ZKBot API Contracts

gRPC service endpoints, REST notes, NATS topics, and NATS KV discovery contract.

## 1. NATS KV Discovery Contract

Bucket: `zk-svc-registry-v1`

> Note: dashes are required — NATS KV bucket names cannot contain dots. Earlier docs used `zk.svc.registry.v1`; the implementation and all plan files use the dash form.

Key format:
- `svc.gw.<gw_id>` — trading gateway
- `svc.oms.<oms_id>` — OMS instance
- `svc.engine.<strategy_key>` — strategy engine keyed by stable logical strategy identity
- `svc.mdgw.<logical_id>` — market data gateway
- `svc.refdata.<logical_id>` — refdata query service
- `svc.rec.<recorder_id>` — recorder instance

Value: protobuf-encoded `zk.discovery.v1.ServiceRegistration` (see [proto.md](proto.md)).

Example (JSON representation):
```json
{
  "service_type": "gw",
  "service_id": "gw_okx_1234",
  "instance_id": "pod-7f9c",
  "transport": {
    "protocol": "grpc",
    "address": "10.0.12.10:51051",
    "authority": "gw-okx.internal"
  },
  "account_ids": [1234],
  "venue": "OKX",
  "capabilities": [
    "place_order",
    "cancel_order",
    "batch_order",
    "query_balance",
    "query_order",
    "supports_streaming_order_events",
    "supports_streaming_trade_events",
    "supports_balance_query",
    "supports_position_query",
    "supports_trade_exactly_once_contract",
    "supports_order_at_least_once_contract"
  ],
  "lease_expiry_ms": 1760000000000,
  "updated_at_ms": 1759999995000
}
```

Heartbeat interval: 5s. Lease TTL: 20s. Consumers evict stale entries after TTL.

`trading_sdk` watches this bucket to resolve `oms_id → gRPC endpoint` — no ODS required.

## 2. gRPC Services

### 2.1 OMS Service — `zk.oms.v1.OMSService`

All command requests carry `AuditMeta audit_meta` and `string idempotency_key`.
All command responses use `CommandAck` (see [proto.md](proto.md)).

Idempotency options:
- Option A (preferred): OMS deduplicates commands on `(order_id, source_id, idempotency_key)` using in-memory + Redis short-window cache.
- Option B: OMS deduplicates in-memory only (lower complexity, weaker restart safety).

Current implementation note: OMS core does not fully implement request idempotency yet; this behavior must be added in the OMS service adapter layer.

**Commands:**

| RPC | Request | Response |
|---|---|---|
| `PlaceOrder` | `PlaceOrderRequest` | `CommandAck` |
| `BatchPlaceOrders` | `BatchPlaceOrdersRequest` | `CommandAck` |
| `CancelOrder` | `CancelOrderRequest` | `CommandAck` |
| `BatchCancelOrders` | `BatchCancelOrdersRequest` | `CommandAck` |
| `Panic` | `PanicRequest` | `CommandAck` |
| `ClearPanic` | `ClearPanicRequest` | `CommandAck` |

**Queries:**

| RPC | Request | Response | Notes |
|---|---|---|---|
| `QueryOpenOrders` | `QueryOpenOrdersRequest` | `QueryOpenOrdersResponse` | |
| `QueryOrder` | `QueryOrderRequest` | `QueryOrderResponse` | |
| `QueryTrades` | `QueryTradesRequest` | `QueryTradesResponse` | |
| `QueryPosition` | `QueryPositionRequest` | `PositionResponse` | Compatibility — reads from balance ledger; prefer `QueryBalances` |
| `QueryBalances` | `QueryBalancesRequest` | `QueryBalancesResponse` | Canonical balance query |
| `QueryHealth` | `QueryHealthRequest` | `ServiceHealthResponse` | |

**Streaming:**

Preferred runtime path is NATS topics (not gRPC server stream) for order update fanout.

Streaming options:
- Option A (preferred): NATS-only streaming (`zk.oms.<oms_id>.order_update...`).
- Option B: keep optional `StreamOrderUpdates` gRPC for single-client direct subscriptions.

**Admin:**

| RPC | Request | Response |
|---|---|---|
| `ReloadConfig` | `ReloadConfigRequest` | `CommandAck` |

> **ODS removed**: `zk.ods.v1.ODSService` does not exist in the new design.
> Routing discovery, refdata, and order-ID generation move to `trading_sdk` + NATS KV + PostgreSQL.

### 2.2 Gateway Service — `zk.gateway.v1.GatewayService`

OMS calls gateways directly. `trading_sdk` does not call gateways.

**Order execution:**

| RPC | Request | Response |
|---|---|---|
| `PlaceOrder` | `GwPlaceOrderRequest` | `GwCommandAck` |
| `BatchPlaceOrders` | `GwBatchPlaceOrdersRequest` | `GwCommandAck` |
| `CancelOrder` | `GwCancelOrderRequest` | `GwCommandAck` |
| `BatchCancelOrders` | `GwBatchCancelOrdersRequest` | `GwCommandAck` |

Gateway identity-linkage note:

- `client_order_id` is generated upstream
- the gateway must emit at least one early acknowledgment or event carrying both `client_order_id`
  and the venue-native order id once both are known
- OMS uses that linkage to persist the durable correlation

**Queries** (mirrors current Python `GWSender.QueryOrderDetails` / `QueryAccountSummary`):

| RPC | Request | Response |
|---|---|---|
| `QueryBalance` | `GwQueryBalanceRequest` | `GwQueryBalanceResponse` |
| `QueryOrder` | `GwQueryOrderRequest` | `GwQueryOrderResponse` |
| `QueryTrades` | `GwQueryTradesRequest` | `GwQueryTradesResponse` |
| `QueryAccountSetting` | `GwQueryAccountSettingRequest` | `GwQueryAccountSettingResponse` |
| `UpdateAccountSetting` | `GwUpdateAccountSettingRequest` | `GwUpdateAccountSettingResponse` |

### 2.2A Market Data Gateway Service — `zk.rtmd.v1.RtmdService`

RTMD streaming remains NATS-first. The gRPC API is for current snapshots and bounded recent
history.

**Current snapshot queries:**

| RPC | Request | Response |
|---|---|---|
| `QueryCurrentTick` | `QueryCurrentTickRequest` | `QueryCurrentTickResponse` |
| `QueryCurrentOrderBook` | `QueryCurrentOrderBookRequest` | `QueryCurrentOrderBookResponse` |
| `QueryCurrentFundingRate` | `QueryCurrentFundingRateRequest` | `QueryCurrentFundingRateResponse` |

**Bounded recent-history queries:**

| RPC | Request | Response |
|---|---|---|
| `QueryKlines` | `QueryKlinesRequest` | `QueryKlinesResponse` |

Registration/discovery note:

- RTMD gateway registration should advertise the query gRPC endpoint through the generic transport field
- supported query families should be advertised in `capabilities` / registration metadata

### 2.2B Refdata Service — `zk.refdata.v1.RefdataService`

The refdata service is the canonical runtime query surface for instrument reference data.

Progressive disclosure note:

- refdata queries should support compact and richer disclosure levels from the same canonical source
- Pilot/operator flows may request richer disclosure than hot-path SDK lookups
- AI-oriented readers may request expanded metadata, but bounded structured responses should remain
  the default

**Queries:**

| RPC | Request | Response |
|---|---|---|
| `QueryInstrumentRefdata` | `QueryInstrumentRefdataRequest` | `QueryInstrumentRefdataResponse` |
| `QueryInstrumentByVenueSymbol` | `QueryInstrumentByVenueSymbolRequest` | `QueryInstrumentByVenueSymbolResponse` |
| `ListInstruments` | `ListInstrumentsRequest` | `ListInstrumentsResponse` |
| `QueryRefdataWatermark` | `QueryRefdataWatermarkRequest` | `QueryRefdataWatermarkResponse` |
| `QueryMarketStatus` | `QueryMarketStatusRequest` | `QueryMarketStatusResponse` |
| `QueryMarketCalendar` | `QueryMarketCalendarRequest` | `QueryMarketCalendarResponse` |

Registration/discovery note:

- refdata service registration should advertise the gRPC endpoint through the generic transport field
- supported query families should be advertised in `capabilities`

### 2.3 Strategy Engine Service — `zk.engine.v1.EngineService`

| RPC | Request | Response |
|---|---|---|
| `StartStrategy` | `StartStrategyRequest` | `LifecycleAck` |
| `StopStrategy` | `StopStrategyRequest` | `LifecycleAck` |
| `PauseStrategy` | `PauseStrategyRequest` | `LifecycleAck` |
| `ResumeStrategy` | `ResumeStrategyRequest` | `LifecycleAck` |
| `SendStrategyCommand` | `SendStrategyCommandRequest` | `LifecycleAck` |
| `QueryStrategyState` | `QueryStrategyStateRequest` | `QueryStrategyStateResponse` |
| `QueryRuntimeMetrics` | `QueryRuntimeMetricsRequest` | `QueryRuntimeMetricsResponse` |

### 2.4 Pilot Control API

Pilot is a REST-first control-plane service in the current design.

Contract note:

- operator/admin flows such as manual trading, topology/config management, strategy lifecycle, risk
  operations, and refdata admin should be exposed through REST
- bootstrap registration remains a separate NATS request/reply contract
- Pilot may aggregate or proxy data from OMS, refdata, and discovery, but it is not the runtime
  liveness source of truth

The concrete REST surface is documented in
[Pilot Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/pilot_service.md).

Pilot/refdata note:

- Pilot may aggregate or proxy refdata queries for operator convenience
- the dedicated refdata service remains the canonical runtime query service for SDK/service lookup

Risk API design note:
- Option A (preferred): split risk control/query details into a dedicated `risk-design.md`.
- Option B: keep risk APIs embedded under Pilot/OMS API docs.

### 2.5 Pilot Bootstrap (NATS Request/Reply)

Bootstrap registration is NATS request/reply, so instances only need `ZK_NATS_URL` to bootstrap.

| Subject | Request | Response |
|---|---|---|
| `zk.bootstrap.register` | `BootstrapRegisterRequest` | `BootstrapRegisterResponse` |
| `zk.bootstrap.reissue` | `BootstrapReissueRequest` | `BootstrapRegisterResponse` |
| `zk.bootstrap.deregister` | `BootstrapDeregisterRequest` | `CommandAck` |
| `zk.bootstrap.sessions.query` | `BootstrapSessionsQueryRequest` | `BootstrapSessionsQueryResponse` |

`zk.bootstrap.register` request fields:
- `token`
- `logical_id`
- `instance_type`
- `runtime_info`

`zk.bootstrap.register` response fields:
- `owner_session_id`
- `kv_key`
- `lock_key`
- `lease_ttl_ms`
- `server_time_ms`
- `status` / `error`

Implementation note:

- the current implementation returns ownership/session metadata and does not depend on a
  scoped runtime credential
- stricter per-key runtime credentials remain a possible later hardening step

## 3. NATS Topics

### 3.1 OMS events

Order update topic options:
- Option A (preferred): include both OMS and account in topic for selective subscriptions.
- Option B: include OMS only and filter by payload fields client-side.

| Topic | Payload | Notes |
|---|---|---|
| `zk.oms.<oms_id>.order_update.<account_id>` | `oms.OrderUpdateEvent` | fan-out to strategies, recorder, monitor |
| `zk.oms.<oms_id>.balance_update` | `oms.BalanceUpdateEvent` | **current impl**: no asset suffix; design calls for `.<asset>` per-asset suffix (deferred) |
| `zk.oms.<oms_id>.position_update.<instrument>` | `oms.PositionUpdateEvent` | per-instrument for derivatives |
| `zk.oms.<oms_id>.system` | `oms.OMSSystemEvent` | panic, reload, restart events |
| `zk.oms.<oms_id>.metrics.latency` | `common.LatencyMetricBatch` | **implemented** — latency segment batch published every `ZK_METRICS_INTERVAL_SECS` (default 10s) |

Balance uses `<asset>` (not `<instrument>`) because spot/margin balances are denominated per asset (USDT, BTC, ETH etc.), not per trading pair. Position uses `<instrument>` for derivatives where position is per contract.

Migration from current Python topics:

| Old | New |
|---|---|
| `tq.oms.service.{oms_id}.order_update` | `zk.oms.<oms_id>.order_update.<account_id>` |
| `tq.oms.service.{oms_id}.balance_update.{symbol}` | `zk.oms.<oms_id>.balance_update.<asset>` |

### 3.2 Gateway events

| Topic | Payload | Notes |
|---|---|---|
| `zk.gw.<gw_id>.report` | `exch_gw.OrderReport` | normalized order/trade report stream; OMS subscribes per bound gateway |
| `zk.gw.<gw_id>.balance` | `exch_gw.BalanceUpdate` | balance snapshots |
| `zk.gw.<gw_id>.position` | `exch_gw.PositionUpdate` | position snapshots when venue supports explicit position stream |
| `zk.gw.<gw_id>.system` | `exch_gw.GwSystemEvent` | startup, reconnect, errors |

Gateway publication semantics:

- trade/fill facts carried by the gateway report stream must be exactly once
- order lifecycle events are at least once
- if a balance or position update is published after an order/trade update, it must reflect that change
- if a position transitions to zero, the gateway must publish that explicit zero-state transition even
  when the venue omits it from the push stream

If a later contract revision splits order and trade into separate topics, these semantics still
apply by event family.

### 3.3 Strategy events

| Topic | Payload |
|---|---|
| `zk.strategy.<strategy_id>.log` | `strategy.StrategyLogEvent` |
| `zk.strategy.<strategy_id>.order` | `strategy.StrategyOrderEvent` |
| `zk.strategy.<strategy_id>.cancel` | `strategy.StrategyCancelEvent` |
| `zk.strategy.<strategy_id>.lifecycle` | `strategy.StrategyLifecycleNotifyEvent` |
| `zk.strategy.<strategy_id>.signal` | `rtmd.RealtimeSignal` |
| `zk.strategy.<strategy_id>.notify` | `strategy.StrategyNotification` |

### 3.4 RTMD topics

Topic names are deterministic. No ODS or service query is needed to construct them.
The runtime interest protocol is defined separately in [rtmd_subscription_protocol.md](rtmd_subscription_protocol.md).

| Topic | Payload |
|---|---|
| `zk.rtmd.tick.<venue>.<instrument_exch>` | `rtmd.TickData` |
| `zk.rtmd.kline.<venue>.<instrument_exch>.<interval>` | `rtmd.Kline` (interval: `1m`, `5m`, `1h`, etc.) |
| `zk.rtmd.funding.<venue>.<instrument_exch>` | `rtmd.FundingRate` |
| `zk.rtmd.orderbook.<venue>.<instrument_exch>` | `rtmd.OrderBook` |

### 3.5 Control topics

| Topic | Payload | Notes |
|---|---|---|
| `zk.control.reload.<service_id>` | `common.ControlCommand` | targeted reload for a specific instance |
| `zk.control.refdata.updated` | `common.ControlCommand` | broadcast on refdata/subscription change |
| `zk.control.mdgw.<venue>.reload` | `common.ControlCommand` | trigger MDGW subscription reload |
| `zk.notify.<channel>` | `monitor.NotificationEvent` | risk/ops alerts |

Note: `zk.posttrade` (legacy `tq.posttrade`) is removed. Post-trade cleanup tasks (Redis eviction of terminal orders, DB write of settled state) are handled by the **Recorder** service directly from `oms.OrderUpdateEvent` when order status is terminal (`FILLED`, `CANCELLED`, `REJECTED`). No separate post-trade topic is needed.
