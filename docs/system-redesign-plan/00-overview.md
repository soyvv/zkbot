# ZKBot Detailed Technical Design — Index

This directory contains the end-state technical design for the ZKBot system redesign.
Each file covers one concern area.

## Files

| File | Contents |
|---|---|
| [01-idea.md](01-idea.md) | Scope, objectives, and tech-stack selection |
| [02-target-architecture.md](02-target-architecture.md) | Architecture principles, planes, service model, logical diagram |
| [03-service-design.md](03-service-design.md) | Per-service responsibilities, startup contracts, domain reuse |
| [04-proto-design.md](04-proto-design.md) | Protobuf package migration, new messages, deprecations |
| [05-api-contracts.md](05-api-contracts.md) | gRPC endpoints, NATS topics, NATS KV discovery contract |
| [06-data-layer.md](06-data-layer.md) | PostgreSQL schema, MongoDB event store schema |
| [07-sdk-design.md](07-sdk-design.md) | `trading_sdk` replacing `TQClient` + ODS |
| [08-rust-crate-architecture.md](08-rust-crate-architecture.md) | Rust crate graph, responsibilities, new components |
| [09-ops-design.md](09-ops-design.md) | Secrets/security, reliability, SLOs, observability |
| [11-registry-security-design.md](11-registry-security-design.md) | NATS KV registration security: ACL, Vault creds, registrar HA |
| [12-topology-token-registration-design.md](12-topology-token-registration-design.md) | Pilot-managed topology and token-gated instance registration |

## Key Design Decisions

**ODS is fully deprecated.**
The current `ods_server.py` (NATS `tq.ods.rpc`) is a central singleton and single point of failure.
Its responsibilities are redistributed:

| ODS responsibility | New owner |
|---|---|
| account → OMS routing | NATS KV registry + `trading_sdk` |
| instrument refdata | PostgreSQL (`cfg.instrument_refdata`) via Pilot REST or direct read |
| RTMD channel lookup | deterministic topic naming — no query needed |
| order ID generation | OMS-side (Snowflake) or `trading_sdk` local generator |
| account setting query/update | `GatewayService` RPC via OMS |
| OMS config query | PostgreSQL + Pilot gRPC |

**Rust domain cores stay infra-agnostic.**
`zk-oms-rs`, `zk-engine-rs`, `zk-strategy-sdk-rs`, `zk-backtest-rs` have no transport dependencies.
All infra (gRPC, NATS, Redis, DB, Vault) lives in `zk-infra-rs` and thin service binaries.

**`trading_sdk` is the only external dependency for strategies and integrations.**
It handles discovery (NATS KV), OMS gRPC channel pooling, retries, and NATS subscriptions transparently.
