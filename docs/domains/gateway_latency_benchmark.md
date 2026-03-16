# Gateway and OMS Latency Benchmark

## Scope

This note defines a more realistic latency benchmark model for gateway, OMS, and
related order-path services.

The main change is:

- benchmark at controlled target QPS
- report latency percentiles under sustained load
- separate realistic load modes from raw max-throughput tests

This note is intended to guide benchmark harness design for:

- `zk-gw-svc`
- `zk-oms-svc`
- simulator-backed gateway testing
- OMS-facing order-path load tests

## Goals

The benchmark should answer:

- what latency distribution do we get at `100/s`, `1k/s`, `10k/s`, etc.
- where does the system knee appear
- how much tail-latency growth appears before hard saturation
- whether the system can sustain a target QPS without backlog explosion or errors

The benchmark should not be limited to:

- maximum unconstrained throughput
- one-client closed-loop request/response timing

## Core Benchmark Model

### Open-loop load generation

Use open-loop scheduling:

- send requests according to a target arrival rate
- do not wait for prior responses before scheduling the next request

Reason:

- this better reflects real trading load
- this avoids hiding queueing behind client-side backpressure

### Target-QPS sweep

Recommended first sweep:

- `100/s`
- `1k/s`
- `5k/s`
- `10k/s`

Recommended expanded sweep:

- `50/s`
- `100/s`
- `250/s`
- `500/s`
- `1k/s`
- `2k/s`
- `5k/s`
- `10k/s`

Each point should use:

- warmup: `15s` to `30s`
- measurement window: `60s` to `300s`

### Arrival pattern

Acceptable first pass:

- fixed-interval pacing

Better later:

- Poisson arrival process around the target mean QPS

Reason:

- fixed intervals are simpler and deterministic
- Poisson arrivals expose queueing and burst sensitivity more realistically

## Workload Modes

Benchmarks should be split by workload mode.

Recommended initial modes:

- `place_only`
- `place_and_fill`
- `place_and_cancel`
- `mixed`

Suggested `mixed` profile:

- `70% place`
- `20% cancel`
- `10% query`

For simulator-backed runs, also vary:

- matching active
- matching paused
- immediate match policy
- FCFS policy

Reason:

- command-path latency and post-trade publication pressure are different bottlenecks

## Timestamp Model

Use explicit timestamp gauges so that benchmark reports and service metrics refer
to the same points in the path.

Recommended canonical timestamps:

- `t0`: scheduled send time in the benchmark harness
- `t1`: actual client send start time
- `t2`: service request accepted at gRPC handler entry
- `t3`: command accepted by service business logic
- `t4`: downstream publish or persistence handoff completed
- `t5`: RPC ack returned to client
- `t6`: first downstream event observed by benchmark subscriber
- `t7`: optional final downstream event observed for the flow

Interpretation by path:

- gateway place path:
  - `t2`: gateway handler entry
  - `t3`: gateway command accepted
  - `t4`: gateway report publication queued or emitted
  - `t6`: first `OrderReport` observed by the benchmark
- OMS place path:
  - benchmark-level generic model:
    - `t2`: OMS handler entry
    - `t3`: OMS request validated and accepted
    - `t4`: OMS publish-to-event-bus or persistence handoff completed
    - `t6`: first `OrderUpdateEvent` observed by the benchmark
    - `t7`: optional `LatencyMetricBatch` or later lifecycle event observed
  - OMS service internal metric model from `zk-oms-svc`:
    - `t0`: `OrderRequest.timestamp × 1e6`
    - `t1`: OMS gRPC handler entry
    - `t3`: immediately before `gw_pool.send_order()`
    - `t3r`: immediately after `send_order()` returns
    - `t4`: gateway handler entry
    - `t5`: gateway BOOKED/report publish time
    - `t6`: OMS receives gateway report
    - `t7`: OMS publishes `OrderUpdateEvent`

Recommended derived gauges:

- client scheduling delay: `t1 - t0`
- end-to-end ack latency: `t5 - t0`
- actual-send ack latency: `t5 - t1`
- service handling latency: `t5 - t2`
- service business-logic latency: `t4 - t3`
- downstream publication latency: `t6 - t4`
- end-to-end first-event latency: `t6 - t0`
- end-to-end final-event latency: `t7 - t0`

OMS-specific derived gauges from the in-service harness:

- `oms_order = t3 - t0`
- `oms_through = t3 - t1`
- `rpc = t3r - t3`
- `gw_proc = t5 - t4`
- `nats_gw_to_oms = t6 - t5`
- `oms_report = t7 - t6`

Interpretation:

- `oms_order` includes client-to-OMS transport plus OMS internal work up to gateway dispatch
- `oms_through` is the preferred "through-OMS" gauge because it excludes client transport and starts at OMS handler entry
- `rpc` isolates OMS-to-GW send and response time

For benchmark reporting, when both are available:

- use `t5 - t0` or client-observed ack latency for end-to-end user-visible latency
- use `oms_through = t3 - t1` for internal OMS processing latency excluding transport selection and client transport effects

Measurement rules for gauges:

- use `t0` as the default benchmark latency origin to avoid coordinated omission
- if a timestamp is unavailable for a path, omit that derived gauge rather than inventing it
- keep timestamp names stable across gateway and OMS reports even if the internal meaning of `t3` and `t4` differs slightly by service
- when exporting service-side metrics, include raw gauges where possible so derived latencies can be recomputed offline

## Metrics To Record

For each run, record:

- target QPS
- achieved QPS
- success count
- failure count
- timeout count
- peak in-flight requests
- end-of-run in-flight requests

Latency metrics:

- `p50`
- `p90`
- `p99`
- `p99.9`
- `max`
- average

If available, also record server-side segments:

- `t1 - t0`
- `t5 - t0`
- `t5 - t1`
- `t5 - t2`
- `t4 - t3`
- `t6 - t4`
- `t6 - t0`
- `t7 - t0`

Service-specific examples:

- gateway ack latency: `t5 - t0`
- gateway first report latency: `t6 - t0`
- OMS ack latency: `t5 - t0`
- OMS through latency: `t3 - t1`
- OMS first order-update latency: `t6 - t0`
- OMS metric-batch or later lifecycle latency: `t7 - t0`

## Measurement Rules

### Avoid coordinated omission

Measure latency from scheduled send time, not only from actual send completion.

Reason:

- overloaded systems otherwise look artificially better

### Keep payloads realistic

Do not benchmark one identical order repeated forever.

Vary at least:

- instrument
- side
- account
- price
- qty

Reason:

- identical requests can overfit caches and branch behavior

### Track backlog explicitly

A run should be considered degraded if:

- in-flight requests grow without stabilizing
- achieved QPS materially undershoots target QPS
- timeout/error rate rises even if median latency looks acceptable

## Reporting Format

Each run should produce one summary row with:

- mode
- target QPS
- achieved QPS
- total requests
- error rate
- peak in-flight
- p50
- p90
- p99
- p99.9
- max

Recommended additional summary:

- highest QPS where `p99 < target_threshold`
- highest QPS where error rate remains zero
- highest QPS where achieved QPS stays within acceptable drift of target

This is often more actionable than raw “max throughput”.

## Suggested Threshold Views

Useful service-level summaries:

- highest QPS with `p99 < 1 ms`
- highest QPS with `p99 < 5 ms`
- highest QPS with `p99 < 20 ms`

Pick thresholds appropriate to the service under test:

- gateway command ack path
- gateway first report path
- OMS order-path processing
- OMS first order-update path
- simulator-backed integration path

## Recommended Benchmark Matrix

First practical matrix:

- rates: `100/s`, `1k/s`, `5k/s`, `10k/s`
- modes:
  - `place_only`
  - `place_and_fill`
  - `mixed`

For simulator-enabled runs:

- `matching_paused`
- `matching_active_immediate`
- `matching_active_fcfs`

## Future Extensions

Later improvements can add:

- Poisson arrivals
- burst tests
- reconnect/degraded-state benchmarks
- mixed-account and mixed-venue workloads
- admin-control benchmarks for simulator flows
- per-stage latency correlation using existing latency topics/metrics

## Non-Goals

This benchmark note does not define:

- exact CLI flags
- exact benchmark crate name
- exact output storage format

Those can vary by harness, as long as the benchmark semantics above are preserved.
