# zk-trade-tools

`zk-trade-tools` is a single entrypoint CLI for trading-stack operational tools.

Current commands:

- `gw-smoke`: basic gateway connectivity and order-path smoke test
- `oms-smoke`: basic OMS connectivity and order-path smoke test
- `sim-full-cycle`: place via OMS and force-match through the simulator admin path
- `gw-latency-bench`: open-loop gateway latency benchmark at controlled target QPS
- `oms-latency-bench`: open-loop OMS latency benchmark at controlled target QPS

## Build

From the Rust workspace root:

```bash
cargo build -p zk-trade-tools
```

Run directly with Cargo:

```bash
cargo run -p zk-trade-tools -- <command> [flags]
```

## Common Flags

These flags are shared by all subcommands unless noted otherwise.

- `--nats-url <url>`: NATS endpoint, default `nats://127.0.0.1:4222`
- `--grpc-host <host>`: host used for service-registry resolution, default `127.0.0.1`
- `--account-id <id>`: logical account id, default `9001`
- `--exch-account-id <id>`: exchange account id, defaults to `account-id` as string
- `--instrument <symbol>`: default `BTCUSDT_SIM`
- `--price <px>`: default `50000`
- `--qty <qty>`: default `0.001`
- `--timeout-ms <ms>`: request timeout, default `5000`
- `--json-out <path>`: write machine-readable output to a file

## Smoke Tests

Gateway smoke test:

```bash
cargo run -p zk-trade-tools -- \
  gw-smoke \
  --gw-id sim-gw
```

OMS smoke test:

```bash
cargo run -p zk-trade-tools -- \
  oms-smoke \
  --oms-id sim-oms
```

Simulator full-cycle test:

```bash
cargo run -p zk-trade-tools -- \
  sim-full-cycle \
  --gw-id sim-gw \
  --oms-id sim-oms
```

All three commands also accept explicit service addresses such as `--gw-addr` and `--oms-addr` to bypass service-registry lookup.

## Gateway Latency Benchmark

`gw-latency-bench` implements the first practical version of the benchmark described in [`gateway_latency_benchmark.md`](../../../docs/domains/gateway_latency_benchmark.md).

Current behavior:

- open-loop fixed-interval scheduling
- target-QPS sweep
- warmup window plus measurement window
- latency measured from scheduled send time to avoid coordinated omission
- percentile summary for gateway ack latency
- optional order-report latency summary through NATS subscription
- explicit backlog and achieved-QPS tracking
- realistic request variation across instrument, side, account, price, and qty

Current limitation:

- only `--mode place_only` is implemented today

Example:

```bash
cargo run -p zk-trade-tools -- \
  gw-latency-bench \
  --gw-id sim-gw \
  --rates 100,1000,5000,10000 \
  --warmup-s 15 \
  --measure-s 60 \
  --track-report-latency true
```

Benchmark-specific flags:

- `--gw-id <id>`: required gateway id
- `--gw-addr <http://host:port>`: optional explicit gateway gRPC address
- `--rates <csv>`: target QPS sweep, default `100,1000,5000,10000`
- `--warmup-s <seconds>`: default `15`
- `--measure-s <seconds>`: default `60`
- `--mode <name>`: default `place_only`
- `--track-report-latency <bool>`: subscribe to gateway report subject and summarize order-report latency
- `--instrument-pool <csv>`: override instrument rotation for more realistic payloads
- `--exch-account-pool <csv>`: override exchange-account rotation for more realistic payloads

The benchmark output includes, per run:

- target QPS
- achieved QPS
- total requests
- success, failure, and timeout counts
- peak and end-of-run in-flight requests
- error rate
- `p50`, `p90`, `p99`, `p99.9`, `max`, and average latency
- degradation markers when throughput, backlog, or error behavior indicate overload

## OMS Latency Benchmark

`oms-latency-bench` applies the same open-loop benchmark model to the OMS place-order path.

Current behavior:

- open-loop fixed-interval scheduling
- target-QPS sweep
- warmup window plus measurement window
- latency measured from scheduled send time
- percentile summary for OMS ack latency
- optional first `OrderUpdateEvent` latency summary through NATS
- optional OMS latency-metric parsing using the service's `t0`..`t7` tags
- optional `oms_through = t3 - t1` summary from OMS metrics
- optional OMS latency-metric batch arrival summary through NATS
- explicit backlog and achieved-QPS tracking
- realistic request variation across instrument, side, price, and qty

Current limitation:

- only `--mode place_only` is implemented today

Example:

```bash
cargo run -p zk-trade-tools -- \
  oms-latency-bench \
  --oms-id sim-oms \
  --rates 100,1000,5000 \
  --warmup-s 15 \
  --measure-s 60 \
  --track-order-update-latency true \
  --track-metric-latency true
```

Benchmark-specific flags:

- `--oms-id <id>`: required OMS id
- `--oms-addr <http://host:port>`: optional explicit OMS gRPC address
- `--rates <csv>`: target QPS sweep, default `100,1000,5000,10000`
- `--warmup-s <seconds>`: default `15`
- `--measure-s <seconds>`: default `60`
- `--mode <name>`: default `place_only`
- `--track-order-update-latency <bool>`: subscribe to OMS order-update subject and summarize first update latency, default `true`
- `--track-metric-latency <bool>`: subscribe to OMS latency-metric subject, summarize `oms_through = t3 - t1`, and also summarize batch-arrival latency, default `false`
- `--instrument-pool <csv>`: override instrument rotation for more realistic payloads

## JSON Output

Use `--json-out` when you want structured output for later analysis:

```bash
cargo run -p zk-trade-tools -- \
  gw-latency-bench \
  --gw-id sim-gw \
  --json-out /tmp/gw-latency-bench.json
```

Smoke-test commands emit a scenario report. Benchmark runs emit a benchmark report with one row per QPS point plus threshold summaries.
