# ZKBot Operational Design

Secrets and security, reliability, SLOs, and observability.

## 1. Secrets and Security

### 1.1 Scope

Vault stores **only exchange account credentials** — API keys, API secrets, and private keys used by
Trading Gateway instances to authenticate with exchanges.

No other services (OMS, Engine, Pilot, Recorder, Monitor) use Vault. Their configs (DB connection
strings, NATS URLs, etc.) are supplied via environment variables or config files using whatever
deployment mechanism is in use (k8s secrets, env files, etc.).

### 1.2 Vault secret paths

```
kv/trading/gw/<account_id>/api_key
kv/trading/gw/<account_id>/api_secret
kv/trading/gw/<account_id>/private_key    # for signing-based venues (e.g. Hyperliquid)
```

One Vault access role per venue (or per gateway instance), scoped to only the `account_id`s it serves:
- `gw-role-okx` → can read `kv/trading/gw/<okx_account_id>/*`
- `gw-role-hyperliquid` → can read `kv/trading/gw/<hl_account_id>/*`

### 1.3 Gateway secret lifecycle

1. Gateway starts and authenticates to Vault (Kubernetes service account or AppRole)
2. Reads account credentials for its bound `account_id` from the path above
3. Caches credentials in-memory; re-reads on lease expiry or on `zk.control.reload.<gw_id>` NATS event
4. Uses credentials to establish and maintain the exchange session

Vault path versioning supports credential rotation without gateway restart.

### 1.4 Transport security

- TLS for gRPC internal traffic
- NATS TLS for event plane
- No mTLS required for internal services in the initial deployment; can be added incrementally if needed

## 2. Reliability

### 2.1 gRPC reconnect

- exponential backoff: base 100ms, cap 10s, +/- 20% jitter
- circuit breaker per OMS/gateway gRPC channel: open after 5 consecutive failures
- half-open probe after 30s; close on first successful probe

### 2.2 NATS consumer idempotency

- all NATS consumers use at-least-once delivery
- idempotency enforced client-side on `correlation_id` (order_id or execution_id) with a 5-minute dedup window
- OMS command dedup: `(order_id, source_id, idempotency_key)` checked in-memory on receipt

### 2.3 NATS KV lease expiry

- gateway and OMS entries have 20s lease TTL with 5s heartbeat
- on TTL expiry, consumers treat endpoint as stale and stop routing to it
- `trading_sdk` picks up new endpoint on next KV watch event (convergence < 3s)

### 2.4 OMS state recovery

On OMS restart, state recovery is hybrid:
1. warm-load open orders and positions from Redis cache
2. query open orders from each bound gateway (`GatewayService.QueryOrder`)
3. query balances and positions from each gateway (`GatewayService.QueryBalance`)
4. replay any gateway reports that arrived during restart window (durable NATS stream)
5. publish reconciliation summary as OMS system event

Redis role:
- non-critical read replica for UI/Pilot
- warm-start cache for OMS
- short retention cache for terminal orders

Authoritative source remains exchange/gateway reconciliation output.

### 2.5 Gateway reconnect

On gateway restart or reconnect:
- gateway re-registers `svc.gw.<gw_id>` in NATS KV
- gateway publishes `zk.gw.<gw_id>.system` event with `GW_EVENT_STARTED`
- OMS receives event and triggers full order and balance resync for affected accounts
  (mirrors current Python `GW_EVENT_STARTED` handler in `oms_server.py`)

### 2.6 Strategy engine failure

- `zk-engine-svc` subscribes to OMS order_update events on durable NATS stream
- on engine restart, resumes from last acknowledged offset
- OMS is unaffected; no order state is lost on engine crash

## 3. SLO Targets

| Metric | Target |
|---|---|
| OMS command ack p99 (in-cluster, excluding venue latency) | < 20ms |
| Engine event-to-action internal p99 | < 10ms |
| Discovery convergence after instance change | < 3s |
| Recorder ingest lag p99 | < 2s |
| Balance/position resync window after GW reconnect | < 30s |
| OMS startup and registration | < 10s |

## 4. Deployment Modes

All three modes preserve identical protobuf contracts and domain semantics.

### 4.1 Three-layer (default)

```
Strategy Engine  →(gRPC)→  OMS  →(gRPC)→  Trading Gateway
     ↕                     ↕
   NATS                  NATS
```

Best for: low/medium frequency strategies, portfolio rebalancing, kline-based CTA.
Isolation: separate processes, separate failure domains.

### 4.2 Two-layer

```
Strategy Engine  →(in-process or local gRPC)→  OMS+GW plugin
```

OMS embeds gateway as a plugin (in-process call, no network hop for execution).
Best for: latency-sensitive market making, arbitrage.

### 4.3 All-in-one

```
[ Strategy + OMS + Gateway ] ← single process
```

`zk-engine-rs` dispatcher calls `OmsCore::process_message` directly (no gRPC).
OmsCore calls gateway execution directly (no gRPC).
Best for: strict latency budgets (tick-to-trade path).

## 5. Observability

### 5.1 Distributed tracing

- `trace_id` propagated via gRPC metadata (`x-trace-id`) and NATS message headers
- correlation IDs carried on all events: `strategy_id`, `execution_id`, `order_id`, `account_id`, `oms_id`, `gw_id`
- OpenTelemetry SDK initialized via `zk-infra-rs::tracing`

### 5.2 Metrics (Prometheus-compatible)

OMS:
- `oms_command_latency_ms` histogram (labels: `oms_id`, `command`) — p50/p95/p99
- `oms_order_total` counter (labels: `oms_id`, `account_id`, `status`)
- `oms_rejection_total` counter (labels: `oms_id`, `account_id`, `reason`)
- `oms_pending_order_cache_size` gauge

Engine:
- `engine_event_latency_ms` histogram (labels: `engine_id`, `event_type`)
- `engine_action_total` counter (labels: `engine_id`, `action_type`)
- `engine_queue_depth` gauge

Gateway:
- `gw_order_latency_ms` histogram (labels: `gw_id`, `venue`)
- `gw_reconnect_total` counter (labels: `gw_id`)
- `gw_report_lag_ms` histogram

Recorder:
- `recorder_ingest_lag_ms` histogram
- `recon_mismatch_total` counter (labels: `recon_type`, `account_id`)

Discovery:
- `kv_churn_total` counter (labels: `service_type`)
- `endpoint_refresh_total` counter
- `stale_eviction_total` counter (labels: `service_type`)

### 5.3 Structured logs

- JSON format with mandatory fields: `timestamp`, `level`, `service_id`, `trace_id`, and correlation IDs
- Log levels: `ERROR`, `WARN`, `INFO`, `DEBUG`, `TRACE`
- Strategy logs published to `zk.strategy.<strategy_id>.log` NATS topic AND written to MongoDB `strategy_log_event`
- OMS system events published to `zk.oms.<oms_id>.system` NATS topic AND written to MongoDB `oms_system_event`

### 5.4 Health endpoints

Each gRPC service exposes `QueryHealth(QueryHealthRequest) returns (ServiceHealthResponse)`.
`ServiceHealthResponse.status` values: `OK`, `DEGRADED`, `DOWN`.

Pilot aggregates health from all discovered services and exposes them via REST and `QueryServiceTopology` gRPC.
