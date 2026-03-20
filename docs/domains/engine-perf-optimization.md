# Engine Performance Optimization

## Purpose

This document defines a concrete performance-oriented refactor plan for the Rust engine runtime and
service path.

The goal is to reduce per-event overhead, allocator pressure, and tail jitter in the current engine
hot path, with particular focus on:

- `zk-engine-rs` event batching and dispatch
- `zk-engine-svc` ingress / forwarding / dispatch path
- `zk-pyo3-rs` Python strategy adapter path
- data movement inherited from the original Python implementation

This note is based on the current code shape in:

- `rust/crates/zk-engine-rs/src/live_engine.rs`
- `rust/crates/zk-engine-rs/src/engine_event.rs`
- `rust/crates/zk-engine-rs/src/latency.rs`
- `rust/crates/zk-engine-svc/src/runtime.rs`
- `rust/crates/zk-engine-svc/src/subscriptions.rs`
- `rust/crates/zk-engine-svc/src/dispatcher.rs`
- `rust/crates/zk-pyo3-rs/src/py_strategy.rs`
- `rust/crates/zk-pyo3-rs/src/adapter.rs`

## Problem Statement

The engine was ported from a Python implementation where passing large event objects around often
meant moving references, not copying owned data.

In the current Rust implementation, several hot-path structures are owned values containing:

- protobuf-generated message structs
- `String`
- nested `HashMap`
- `Vec`
- per-callback Python conversion payloads

This is not inherently wrong, but it means the engine now pays real costs for:

- cloning event identity strings during tick coalescing
- cloning trigger metadata into per-order correlation payloads
- building full snapshot payloads on every batch
- spawning one async task per order/cancel dispatch
- serializing Rust protobuf messages into Python objects on every Python callback
- rebuilding Python-facing balance snapshots from Rust state

These costs are not catastrophic individually, but they sit on the per-event or per-action path and
therefore affect:

- tick-to-decision latency
- decision-to-submit latency
- p99 jitter during bursts
- allocator pressure
- CPU efficiency when Python strategies are enabled

## Current Hot Path Review

## Rust engine event loop

Current event-loop flow:

1. `zk-engine-svc` pushes `EventEnvelope` into bounded queues
2. `LiveEngine::run()` drains the channel into a fresh `Vec`
3. `coalesce_ticks()` builds a fresh `HashMap<(String, String), usize>`
4. `TriggerContext::from_envelope()` clones event identity strings into owned fields
5. strategy callback runs
6. if actions exist, latency samples and trigger context are cloned or materialized again
7. snapshot publication allocates a fresh `EngineSnapshot`

Main current costs:

- per-batch allocation of the temporary batch vector
- per-batch allocation of the coalescing map and keep-set
- per-event `String` cloning for `(instrument_code, exchange)` keys
- per-event `String` cloning again into `TriggerContext.instrument_code`
- per-batch cloning of recent latency samples into the read replica snapshot

## Engine service path

Current service flow:

1. subscriptions receive decoded events
2. ticks go directly to the engine queue
3. bars / OMS updates go through the grace queue
4. control commands go through the control queue
5. priority forwarder moves control/grace events into the engine queue
6. `TradingDispatcher` spawns a new task per order or cancel

Main current costs:

- extra queue hop for all reliable events
- task wakeups from the priority forwarder even under light load
- one `tokio::spawn` per order/cancel action
- repeated cloning of `execution_id` and trigger payload fields in dispatcher code

The queue topology is acceptable for correctness, but the service should avoid adding unnecessary
copies or task churn beyond that.

## PyO3 strategy adapter path

The Python path is currently the most allocation-heavy part of the engine.

Current callback shape:

1. acquire GIL
2. clone a `Py<ZkQuantAdapter>` handle
3. refresh adapter state from `StrategyContext`
4. convert event to Python object
5. call Python method
6. drain `Vec<SAction>`

Main current costs:

### 1. Protobuf serialization for OMS events

`rust_proto_to_py()` serializes every OMS protobuf to a new `Vec<u8>` and then reparses it in
Python via `FromString`.

This affects:

- `OrderUpdateEvent`
- `BalanceUpdateEvent`
- `PositionUpdateEvent`

This is the biggest clear performance smell in the current Python path.

### 2. Rebuilt balance snapshot maps

`ZkQuantAdapter::from_ctx()` and `refresh()` rebuild:

- `HashMap<i64, HashMap<String, ZkBalance>>`

whenever `balance_generation` changes.

That means each balance update causes:

- string cloning of every asset key
- fresh `HashMap` allocation and inserts
- fresh `ZkBalance` allocation/copy population

### 3. Python method lookup on every callback

The adapter repeatedly checks for method existence via `getattr(...)` in hot callbacks, especially:

- `on_orderupdate` vs `on_order_update`
- `on_scheduled_time` vs `on_timer`

That is small compared with protobuf serialization, but still unnecessary repeated Python-side work.

### 4. Missing cached converters beyond bars

`Kline` already has a cached Python class lookup, but OMS event conversion still goes through the
generic serialize-and-parse path every time.

## Decision Summary

### 1. Keep correctness-driven queue topology, but minimize extra work around it

The control / grace / event queue split in `zk-engine-svc` is acceptable for now because it encodes
real semantics:

- ticks are lossy
- control is high priority
- OMS / bars are reliable

Optimization should not collapse this correctness model. It should instead:

- reduce allocations inside the forward path
- avoid redundant task spawning
- keep queue payloads as compact as practical

### 2. Reduce owned string movement in the Rust hot path

The engine should avoid repeatedly cloning instrument identity strings in:

- tick coalescing
- trigger-context construction
- latency samples

This does not require an immediate global intern table, but the path should move toward:

- borrowed lookups during batch processing
- compact per-event references where possible
- delayed materialization of owned strings only at publish / query boundaries

### 3. Treat the Python adapter as a separate optimization domain

The PyO3 adapter has materially different costs than the Rust-native path.

The optimization strategy should be:

- keep Rust-native path simple and cheap
- optimize Python path explicitly rather than forcing both through the same abstraction

### 4. Prefer cached or direct field mapping over serialize-and-parse bridges

The Python adapter should not keep using protobuf bytes as the default interop path for OMS events if
the engine expects Python strategies to run in live mode with meaningful throughput.

## Target Refactor Areas

## Area 1: Tick coalescing data movement

Current issue:

- `coalesce_ticks()` allocates a `HashMap<(String, String), usize>` and clones both strings for each
  tick candidate

Target:

- avoid allocating owned `(String, String)` pairs in the first pass
- use borrowed keys or a more compact event-side symbol identity

Practical options:

- use `HashMap<(&str, &str), usize>` inside the local batch pass
- introduce an optional compact instrument key at ingress time for coalescing only

Recommendation:

- start with borrowed `&str` coalescing keys; it is the lowest-friction improvement

## Area 2: TriggerContext size and cloning

Current issue:

- `TriggerContext` owns `instrument_code`, `execution_id`, and `strategy_key`
- this gets cloned again into the proto `TriggerContext` at order dispatch

Target:

- keep full owned trigger payload only where it crosses the service boundary
- keep internal hot-path trigger metadata compact

Recommendation:

- split internal trigger metadata from publishable/proto trigger metadata
- internal form should prefer borrowed or shared references for stable strings
- proto materialization should happen only inside the dispatcher boundary

## Area 3: Snapshot publication churn

Current issue:

- `publish_snapshot()` allocates a fresh `EngineSnapshot` every batch
- `recent_latency_samples()` clones the ring-buffer contents into a fresh `Vec`

This is not the primary hot-path cost, but it is avoidable background churn.

Recommendation:

- separate “hot stats” from “debug samples”
- publish debug samples less frequently than every batch
- consider a two-tier snapshot:
  - fast snapshot per batch with counters only
  - slower snapshot with copied sample history on a timer

## Area 4: Dispatcher task spawning

Current issue:

- `TradingDispatcher` performs one `tokio::spawn` per place/cancel action

This is simple and correct, but it adds scheduler overhead on the per-order path.

Recommendation:

- if order-rate expectations remain low, keep it for now
- if the engine is expected to emit larger bursts, move to a dedicated dispatcher worker task with a
  bounded action queue

This should be treated as a second-wave optimization after more obvious copy/clone issues are fixed.

## Area 5: PyO3 OMS event conversion

Current issue:

- OMS events are converted via `prost::Message::encode_to_vec()` followed by Python `FromString`

Target:

- eliminate the bytes round-trip for live callbacks

Recommendation:

- add direct Rust-to-Python object builders for:
  - `OrderUpdateEvent`
  - `BalanceUpdateEvent`
  - `PositionUpdateEvent`
- follow the same model already used for `Kline`

This is the single highest-value optimization in the current Python adapter.

## Area 6: Py adapter state snapshot rebuilding

Current issue:

- balance snapshots are rebuilt as nested hash maps whenever balance generation changes

Target:

- preserve the adapter object, but make balance refresh incremental or cheaper

Recommendation:

- keep the generation guard
- replace full nested-map rebuild with per-account update where practical
- consider a Python-native cached mapping updated in place instead of rebuilding a Rust-side map that
  is then rewrapped

## Area 7: Python callback method resolution

Current issue:

- callback aliases are checked dynamically on each invocation

Recommendation:

- resolve callback method objects once during adapter initialization
- store the resolved callable or `None`
- call the cached method directly in hot callbacks

This is a smaller optimization, but low risk and easy to reason about.

## Proposed Execution Order

1. optimize PyO3 OMS event conversion to remove serialize-and-parse bridging
2. reduce tick coalescing string allocation by using borrowed keys
3. split internal trigger metadata from boundary/proto trigger payload materialization
4. reduce snapshot sample-copy frequency
5. move dispatcher from per-action `tokio::spawn` to a worker queue if measurements justify it
6. optimize adapter balance refresh and cached Python method resolution

## Non-Goals

This design does not require:

- changing the external gRPC or protobuf contracts
- removing the current queue topology in `zk-engine-svc`
- eliminating Python support
- replacing all `String` fields in one pass
- introducing unsafe code or custom allocators

## Suggested Benchmarks

The refactor should be validated with focused measurements rather than intuition.

Recommended measurements:

- Rust-native tick-only path:
  - ticks/sec through `LiveEngine::run()`
  - p50/p99 decision latency with noop strategy
- Rust-native tick-to-order path:
  - p50/p99 tick-to-submit latency
  - allocation counts if available
- Python path:
  - p50/p99 `on_bar` latency
  - p50/p99 `on_order_update` latency
  - cost before/after removing protobuf bytes round-trip
- service path:
  - action burst cost with current per-action `tokio::spawn`
  - control/grace forwarder overhead under mixed load

## Immediate Actionable Findings

The current code has four concrete performance smells worth addressing:

1. `coalesce_ticks()` clones instrument/exchange strings into a per-batch map
2. `TriggerContext` owns and reclones stable identity strings on the hot path
3. `PyStrategyAdapter` converts OMS events through protobuf bytes on every callback
4. `ZkQuantAdapter` rebuilds nested balance snapshot maps on balance-generation changes

Of these, the PyO3 OMS event conversion is the clearest first optimization target.
