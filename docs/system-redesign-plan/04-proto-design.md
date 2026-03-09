# ZKBot Protobuf Design

Package migration plan, new shared messages, and deprecations.

## 1. Package Migration

Current proto packages use mixed legacy naming (`tqrpc_*`, un-versioned). Target is explicit versioned packages.

| Current package | New package | Notes |
|---|---|---|
| `common` | `zk.common.v1` | |
| `oms` | `zk.oms.v1` | domain types |
| `tqrpc_oms` | `zk.oms.v1` | merged into domain package |
| `exch_gw` | `zk.exch_gw.v1` | gateway report types |
| `tqrpc_exch_gw` | `zk.gateway.v1` | gateway RPC service layer |
| `strategy` | `zk.strategy.v1` | |
| `rtmd` | `zk.rtmd.v1` | |
| `ods` / `tqrpc_ods` | **deprecated, removed** | no ODS service in new design |
| *(new)* | `zk.engine.v1` | engine lifecycle RPC |
| *(new)* | `zk.discovery.v1` | KV registration message |
| *(new)* | `zk.monitor.v1` | alert and notification types |
| *(new)* | `zk.pilot.v1` | pilot control/query RPC |

During transition, Rust module aliases in `zk-proto-rs` (current: `exch_gw`, `tqrpc_exch_gw`, `oms`, `tqrpc_oms`, `ods`, `tqrpc_ods`, `rtmd`, `strategy`, `common`) are re-exported under `zk::*` shims until full rename is complete.

## 2. `zk.common.v1` — Cross-cutting shared messages

```proto
syntax = "proto3";
package zk.common.v1;

message ErrorStatus {
  int32  code    = 1;
  string message = 2;
  string detail  = 3;
}

// Standard command acknowledgement — used by all command RPCs
message CommandAck {
  bool        success        = 1;
  ErrorStatus error          = 2;
  string      request_id     = 3;
  string      idempotency_key = 4;
}

// Lifecycle acknowledgement (strategy engine)
message LifecycleAck {
  bool        success    = 1;
  ErrorStatus error      = 2;
  string      request_id = 3;
}

// Attached to all command requests for tracing and audit
message AuditMeta {
  string source_id    = 1;  // strategy_id, client_id, or "pilot"
  string request_id   = 2;  // UUID
  string trace_id     = 3;  // distributed trace ID
  int64  timestamp_ms = 4;
}

message PaginationRequest {
  int32  page_size = 1;
  string cursor    = 2;
}

message PaginationResponse {
  string next_cursor  = 1;
  bool   has_more     = 2;
  int32  total_count  = 3;
}

message ServiceHealthResponse {
  string service_id  = 1;
  string status      = 2;  // OK | DEGRADED | DOWN
  string detail      = 3;
  int64  uptime_ms   = 4;
}

message ControlCommand {
  string command = 1;
  string target  = 2;
  map<string, string> params = 3;
}
```

## 3. `zk.discovery.v1` — Service registry

Used as the value payload in NATS KV entries (see [05-api-contracts.md](05-api-contracts.md)).

```proto
syntax = "proto3";
package zk.discovery.v1;

message TransportEndpoint {
  string protocol  = 1;   // grpc | nats | custom
  string address   = 2;   // host:port
  string authority = 3;   // DNS/service authority for TLS SNI
}

message ServiceRegistration {
  string            service_type   = 1;
  string            service_id     = 2;
  string            instance_id    = 3;
  TransportEndpoint endpoint       = 4;
  repeated int64    account_ids    = 5;
  string            venue          = 6;
  repeated string   capabilities   = 7;
  int64             lease_expiry_ms = 8;
  int64             updated_at_ms   = 9;
  map<string, string> attrs        = 10;
}
```

## 4. `zk.oms.v1` — OMS enhancements

Current Rust modules: `zk_proto_rs::oms`, `zk_proto_rs::tqrpc_oms`.

Additions in the new package:

- All command requests include `AuditMeta audit_meta` and `string idempotency_key`.
- All command responses use `CommandAck`.
- Paginated query APIs for orders and trades via `PaginationRequest` / `PaginationResponse`.
- Live push options:
  - Option A (preferred): NATS topic streaming only (`zk.oms.<oms_id>.order_update.<account_id>`).
  - Option B: optional `StreamOrderUpdates` server-streaming gRPC for direct subscribers.

Current enum variant names (NO type-name prefix) are preserved in `zk.oms.v1`:
- `OrderStatus`: `UNSPECIFIED`, `PENDING`, `BOOKED`, `PARTIALLY_FILLED`, `FILLED`, `CANCELLED`, `REJECTED`
- `ExecType`: `UNSPECIFIED`, `CANCEL`, `PLACING_ORDER`

## 5. `zk.exch_gw.v1` — Exchange gateway report types

Current Rust modules: `zk_proto_rs::exch_gw`.

No structural changes; package rename only. Current enum variants kept:
- `ExchangeOrderStatus`: `UNSPECIFIED`, `BOOKED`, `PARTIAL_FILLED`, `FILLED`, `CANCELLED`, `EXCH_REJECTED`, `EXPIRED`
- `ExchExecType`: `UNSPECIFIED`, `REJECTED`, `CANCEL_REJECT`, `REPLACED`, `RESTATED`
- `OrderReportType`: `UNSPECIFIED`, `TRADE`, `STATE`, `FEE`, `LINKAGE`, `EXEC`
- `OrderSourceType`: `UNSPECIFIED`, `TQ`, `NON_TQ`, `UNKNOWN` → rename tracked in `docs/rust-port-plan/07-open-questions-and-non-port-changes.md`

## 6. `zk.strategy.v1` — Strategy enhancements

Additions:
```proto
enum StrategyLifecycleState {
  LIFECYCLE_UNSPECIFIED = 0;
  INITIALIZING          = 1;
  RUNNING               = 2;
  PAUSED                = 3;
  STOPPED               = 4;
  ERROR                 = 5;
}

enum StrategyRuntimeType {
  RUNTIME_UNSPECIFIED = 0;
  RUST                = 1;
  PY_EMBEDDED         = 2;
  PY_WORKER           = 3;
}

enum LogLevel {
  LOG_UNSPECIFIED = 0;
  TRACE           = 1;
  DEBUG           = 2;
  INFO            = 3;
  WARN            = 4;
  ERROR           = 5;
}

message StrategyLogEvent {
  string   strategy_id  = 1;
  string   execution_id = 2;
  LogLevel level        = 3;
  string   message      = 4;
  int64    timestamp_ms = 5;
  map<string, string> meta = 6;
}

message StrategyNotification {
  string severity   = 1;  // INFO | WARN | CRITICAL
  string channel    = 2;  // slack_channel, pagerduty_key, etc.
  string message    = 3;
  map<string, string> payload = 4;
}
```

## 7. `zk.monitor.v1` — Monitor and alert types

```proto
syntax = "proto3";
package zk.monitor.v1;

message NotificationEvent {
  string severity      = 1;
  string source        = 2;
  string channel       = 3;
  string message       = 4;
  int64  timestamp_ms  = 5;
  map<string, string> tags = 6;
}

message ReconResult {
  string recon_type    = 1;  // ORDER | BALANCE | POSITION | TRADE
  int64  account_id    = 2;
  string oms_id        = 3;
  int32  mismatch_count = 4;
  string detail        = 5;
  int64  run_at_ms     = 6;
}
```

## 8. Additional payload types used by topic contracts

```proto
syntax = "proto3";
package zk.rtmd.v1;

message OrderBook {
  string instrument_code = 1;
  string exchange = 2;
  int64  timestamp_ms = 3;
  repeated PriceLevel buy_levels = 4;
  repeated PriceLevel sell_levels = 5;
}
```

```proto
syntax = "proto3";
package zk.common.v1;

message PostTradeEvent {
  int64  order_id = 1;
  int64  account_id = 2;
  string oms_id = 3;
  string gw_id = 4;
  int64  timestamp_ms = 5;
  map<string, string> meta = 6;
}
```

## 9. Deprecation and Cleanup

Remove in v2 contracts:
- `ods` and `tqrpc_ods` proto packages entirely — no ODS service in new design
- All references to `tq.ods.rpc` NATS subject
- Typo field names: `accound_id` (found in existing protos) → `account_id`
- Duplicated config message families split across `rpc-ods.proto` and `rpc-oms.proto`
- Mixed optional-by-convention fields — mark all optional proto3 scalars explicitly with `optional` keyword

Pending renames (tracked separately in `docs/rust-port-plan/07-open-questions-and-non-port-changes.md`):
- `OrderSourceType::TQ` / `NonTq` → `INTERNAL` / `EXTERNAL`
- `tqrpc_*` prefix removal from all RPC package names
- Rust naming: `handle_non_tq_orders` → `handle_external_orders`, `nontq_order_dict` → `external_order_dict` (already done in Rust port; Python side to follow)
