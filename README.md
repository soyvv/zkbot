# zkbot

zkbot is a polyglot trading platform for building, running, and operating automated trading systems.
The project is organized around a control plane for configuration and lifecycle management, and a
data plane for strategy execution, order management, venue connectivity, market data, recording, and
reconciliation.

The long-term goal is to provide a stable foundation where strategies can move between simulation,
backtesting, paper trading, and live deployment without changing the core business semantics. The
system is designed to support both isolated service deployments and lower-latency collocated
deployments while keeping the same contracts between components.

## Code Structure

- `protos/`  
  Contract-first protobuf schemas. Versioned `zk/*/v1` packages are the primary API surface, with
  generated bindings consumed by Rust, Python, and Java code.

- `rust/`  
  Runtime and hot-path components. This includes OMS domain logic and service hosts, gateway and
  RTMD hosts, strategy runtime crates, backtesting, recorder components, infrastructure helpers, and
  generated protobuf bindings.

- `python/`  
  Python libraries, tools, generated protobuf packages, and active Python services. Current active
  service code includes the refdata service; legacy Python snapshots remain under `python/legacy/`
  for reference.

- `java/`  
  Pilot service and Java protobuf module. Pilot is the main control-plane implementation for
  topology, bootstrap, configuration, schema registry, and operator-facing APIs.

- `venue-integrations/`  
  Venue modules such as OANDA, OKX, IBKR, and simulator support. Each venue is described by a
  manifest and may provide trading gateway, RTMD, and refdata capabilities.

- `service-manifests/`  
  Service-kind manifest and schema metadata used by Pilot for config validation and operator UI
  workflows.

- `scripts/`  
  Repository-level automation, including protobuf generation and dependency-contract checks.

## Architecture Choices

- **Contract-first APIs**  
  Protobuf is the shared contract layer. gRPC is used for command/query flows, while NATS is used
  for event fanout, async workflows, and runtime discovery.

- **Control plane vs data plane**  
  Pilot owns desired configuration, topology, bootstrap authorization, and operator workflows. OMS,
  gateways, RTMD, engines, refdata, recorder, and monitor services own runtime behavior.

- **Rust for hot paths**  
  Latency-sensitive and stateful runtime components are Rust-first. Core domain logic is kept
  deterministic and transport-agnostic where possible.

- **Python for integration and tooling**  
  Python is used for venue adaptors where it is pragmatic, refdata ingestion, tooling, analytics,
  and optional embedded strategy workflows.

- **Java for Pilot**  
  The current Pilot backend is Java/Spring, with Gradle-managed protobuf bindings and relational
  control-plane storage.

- **Manifest-driven configuration**  
  Service and venue config shapes are defined by bundled manifests and schemas. Pilot treats these
  as the authoritative contract for validation, UI rendering, and change impact.

- **Lease-based service discovery**  
  Runtime services register live endpoints in NATS KV. Consumers discover services dynamically and
  react to lease expiry or topology changes.

- **Explicit data ownership**  
  PostgreSQL stores authoritative relational state and config. Redis is used for fast operational
  state and read replicas. NATS carries live events and leases. Event/document storage is reserved
  for append-oriented audit and replay use cases.

- **Flexible deployment shapes**  
  The architecture supports three-layer service deployment, two-layer collocation, and all-in-one
  runtime composition. The deployment shape changes process boundaries, not the domain contracts.

## Project Dev Progress

Done:

- [x] Polyglot monorepo structure with shared protobuf contracts across Rust, Python, and Java.
- [x] Rust OMS domain library and OMS service host.
- [x] Rust gateway, RTMD, strategy runtime, backtest, recorder, and SDK crate foundations.
- [x] Java Pilot control plane for topology, bootstrap, config management, schema registry, and operator APIs.
- [x] Python refdata service with lifecycle-aware instrument refresh and gRPC query surface.
- [x] Manifest-based venue integration model with OANDA, OKX, IBKR, and simulator coverage.
- [x] Generated protobuf bindings for active runtime languages.
- [x] Service and venue manifest model for config validation and capability discovery.

In progress / todo:

- [ ] Continue migrating remaining hot-path runtime behavior to Rust-first services.
- [ ] Complete production orchestration for engine and strategy lifecycle management.
- [ ] Broaden runtime config introspection and drift handling across all managed services.
- [ ] Finish service-side migration to the versioned `zk.*.v1` protobuf contracts where legacy aliases remain.
- [ ] Expand recorder, reconciliation, and monitoring workflows around durable event capture.
- [ ] Harden multi-venue refdata, market-data, and gateway flows for larger instrument universes.
- [ ] Keep strategy SDK ergonomics aligned across backtest, simulation, paper, and live modes.
