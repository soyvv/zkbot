# Performance Observability

## Purpose

This document defines the benchmark-oriented and production-safe performance observability model for:

- `zk-engine-svc`
- `zk-oms-svc`
- simulator-backed gateway smoke tests
- a Python observer or similar external collector

The immediate use case is a smoke-test setup:

- one engine service running a smoke-test market-making strategy
- OMS service
- simulator gateway
- an observer process subscribing to performance topics and writing reports

The longer-term goal is to keep the same observability model useful in production, not just in ad
hoc benchmarks.

## Scope

This note covers:

- what latency data engine and OMS should collect
- how that data should be buffered and published
- how external collection should work
- what control-plane hooks should exist for deterministic test runs
- how to keep observability overhead bounded

This note does not define:

- the benchmark harness itself
- dashboard implementation details
- the exact internal latency tracker data structures

## Goals

The observability model should support two main questions:

1. what is the decision-to-trade latency
2. how is that latency broken down across:
   - engine queue/wait
   - strategy compute
   - engine dispatch/submit
   - transport / RPC
   - OMS internal handling
   - gateway / simulator path if available

It should also support:

- low-overhead continuous collection in production
- explicit flush-and-observe behavior in smoke tests
- stable correlation of one trade decision across engine and OMS

## Core Design

The model should use three layers:

### 1. Local in-process collection

Each service should collect latency records locally on the hot path into an internal bounded buffer.

Rules:

- collection must be cheap
- hot path should only append compact samples or update counters
- no synchronous NATS publish on the hot path

### 2. Periodic batch publication

Each service should periodically publish accumulated performance data to NATS.

Rules:

- publication happens off the hot path
- publish on a configurable interval
- publish batches, not one message per event
- batches should include service identity and time window metadata

### 3. Explicit flush control

Each service should expose a control-plane RPC such as `FlushPerfData`.

Purpose:

- force buffered performance data to be published immediately
- support deterministic end-of-test collection in smoke tests
- reduce uncertainty about buffered-but-not-yet-published data

## Why periodic publish plus flush is preferred

This is better than direct synchronous publishing because:

- it decouples observability from the hot loop
- it keeps per-event overhead bounded
- it works in production without turning telemetry into the bottleneck
- it still gives smoke tests a deterministic collection boundary

## Recommended Service Behavior

## Engine

The engine should collect:

- event ingress timestamp
- queue enqueue timestamp
- strategy dispatch timestamp
- strategy decision timestamp
- OMS submit timestamp

It should derive:

- ingress to queue
- queue wait
- strategy compute
- engine-local decision to submit
- engine-local trigger to submit

It should also emit:

- tick drop/coalesce counters
- grace queue / control queue counters where applicable
- current queue-depth style gauges

## OMS

OMS should collect:

- OMS handler entry
- validation accepted
- internal queueing / worker dispatch points if relevant
- gateway send start and completion
- gateway report receipt
- order update publish

It should derive:

- OMS through latency
- RPC / transport segment
- gateway round-trip if available
- report publication latency

## Gateway / Simulator

If available, gateway or simulator should publish:

- request receive
- booking / matching / report emit
- venue/simulator processing segment

This is optional for the first engine+OMS benchmark, but the schema should not prevent it.

## Correlation Model

The engine and OMS must share a common correlation payload.

The order path should carry forward:

- `execution_id`
- `strategy_key`
- `trigger_event_type`
- `instrument_code`
- `trigger_source_ts`
- `trigger_recv_ts`
- `trigger_dispatch_ts`
- `trigger_decision_ts`
- `decision_seq`
- `client_order_id`

OMS should preserve these fields and append its own stage timestamps rather than reconstruct the
earlier path indirectly.

This is the core enabler for offline or observer-side full-path reconstruction.

## Data Model

Recommended message families:

- `PerfMetricBatch`
- `PerfFlushRequest`
- `PerfFlushResponse`

The exact protobuf names can be adjusted, but the logical split should remain:

- periodic data batch
- explicit flush command
- explicit flush acknowledgement

## Batch structure

Each published batch should contain:

- `service_type`
- `service_id`
- `execution_id` if applicable
- `window_start_ts`
- `window_end_ts`
- `sample_count`
- counters / gauges snapshot
- repeated compact latency samples

Samples should be compact and typed.

Recommended sample categories:

- `engine_order_decision_sample`
- `oms_order_path_sample`
- `gateway_order_path_sample`

For production, services may publish:

- aggregate-only mode
- sampled-detail mode
- full-detail mode for smoke tests

## Publication Topics

Suggested topic family:

- `perf.engine.<engine_id>`
- `perf.oms.<oms_id>`
- `perf.gateway.<gw_id>`

Optional aggregate observer output topic:

- `perf.observer.report.<run_id>`

The observer should subscribe to the raw service topics, not require direct process access.

## Control-Plane Flush

Each service should support an explicit flush RPC:

- `FlushPerfData`

Semantics:

- publish all currently buffered performance samples as soon as practical
- return an acknowledgement after the publish task has accepted the flush request
- optionally include the flush sequence id or publish watermark

This does not require the RPC handler itself to publish synchronously. It only needs to guarantee
that the flush was accepted and processed.

## Configuration Model

Performance collection and publication should be configurable separately.

Recommended controls:

- `perf_collection_enabled`
- `perf_publish_enabled`
- `perf_publish_interval_ms`
- `perf_detail_mode`
- `perf_sampling_rate`
- `perf_buffer_capacity`

Recommended `perf_detail_mode` values:

- `OFF`
- `AGGREGATE_ONLY`
- `SAMPLED`
- `FULL`

Rules:

- if collection is off, publish must also be effectively off
- if collection is on but publish is off, the service may still answer `FlushPerfData` with local
  summaries or reject flush as unsupported by config
- production default should usually be:
  - collection on
  - publish on
  - sampled or aggregate mode
- smoke test default should usually be:
  - collection on
  - publish on
  - full mode

## Recommended Production Practice

Do not treat benchmark observability as a separate one-off subsystem.

Recommended production posture:

- always keep aggregate latency collection available
- keep periodic publication enabled in bounded form
- use sampled detail by default
- reserve full-detail mode for controlled test windows or incident diagnosis

This avoids the common failure mode where:

- benchmark mode has useful data
- production mode has almost none
- the two systems drift apart over time

## Observer Design

The observer can be a Python process in the first implementation.

Responsibilities:

- subscribe to raw perf topics from engine and OMS
- correlate records by `client_order_id` / decision context
- compute end-to-end latency breakdown
- emit CSV, parquet, JSON, or summary reports
- optionally trigger `FlushPerfData` at benchmark phase boundaries

The observer should remain out-of-process.

Reason:

- easier to iterate quickly
- lower coupling to the services
- works for both smoke tests and production captures

## Recommended Smoke-Test Flow

1. start simulator gateway
2. start OMS with perf collection/publish enabled
3. start engine with smoke-test MM strategy and perf collection/publish enabled
4. start Python observer subscribed to perf topics
5. run the scenario for a fixed window
6. call `FlushPerfData` on engine and OMS
7. wait for observer to receive the flushed batches
8. produce a benchmark report

## Key Gauges To Report

At minimum, the benchmark report should include:

- decision-to-submit latency
- trigger-to-submit latency
- engine queue wait
- strategy compute
- OMS through latency
- OMS RPC/transport segment
- first order update latency
- end-to-end trigger-to-first-order-update latency

If gateway timestamps are available, also include:

- gateway processing latency
- OMS-to-gateway-to-OMS round trip

## Recommended Derived Views

For each benchmark run, report:

- p50
- p90
- p99
- p99.9 if sample count supports it
- max
- sample count

And report breakdown views by:

- service
- symbol
- strategy
- event type

Avoid high-cardinality labels in the in-service metrics system. The observer can perform high-cardinality
analysis offline using the detailed samples.

## Guardrails

Performance observability must not become the new bottleneck.

Rules:

- no synchronous publishing on the engine or OMS hot path
- no unbounded in-memory sample retention
- all detailed buffers must be bounded
- sample dropping due to observability overload should be counted and reported
- the flush RPC must not block the hot path while waiting for NATS round trips

## Initial Implementation Recommendation

First implementation should be:

- engine local collector + periodic NATS batch publish
- OMS local collector + periodic NATS batch publish
- `FlushPerfData` RPC on both services
- Python observer consuming the raw perf topics
- smoke-test MM strategy used to generate realistic decision traffic

That is sufficient to establish:

- decision-to-trade latency
- engine vs OMS breakdown
- benchmark repeatability
- a production-compatible observability path

## Related Docs

- [Engine Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/engine_service.md)
- [OMS Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/oms_service.md)
- [Engine Performance Optimization](/Users/zzk/workspace/zklab/zkbot/docs/domains/engine-perf-optimization.md)
- [Gateway and OMS Latency Benchmark](/Users/zzk/workspace/zklab/zkbot/docs/domains/gateway_latency_benchmark.md)
