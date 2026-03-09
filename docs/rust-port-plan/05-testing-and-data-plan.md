# Testing And Data Plan

## Testing principle

Build replay and parity infrastructure before major ports.

## Test layers

### 1. Protobuf golden tests
- [ ] generate golden fixtures for critical messages
- [ ] verify Python serialize -> Rust parse
- [ ] verify Rust serialize -> Python parse
- [ ] compare semantic equivalence

### 2. Domain parity tests
- [ ] OMS transition parity
- [ ] balance/position parity
- [ ] strategy action parity
- [ ] timer/event ordering parity

### 3. Concurrency tests
- [ ] snapshot visibility correctness
- [ ] no torn reads for reader paths
- [ ] burst input behavior under load
- [ ] timeout/backpressure behavior

### 4. End-to-end replay tests
- [ ] market data replay
- [ ] order/gateway report replay
- [ ] strategy event/action replay
- [ ] service shadow comparisons

### 5. Strategy compatibility tests
- [ ] representative Python strategy under legacy runtime
- [ ] same strategy under embedded runtime
- [ ] same strategy under worker runtime where applicable
- [ ] representative Rust-native strategy

### 6. Performance and soak tests
- [ ] throughput benchmark
- [ ] latency benchmark
- [ ] memory growth benchmark
- [ ] long-run stability test

## ArcticDB data preparation

### Source of truth
- use local `ArcticDB` as the primary test/research data source

### Additional local seed data
- use historical 1-minute TXT files under `~/Downloads/全套数据TXT` as an approved source for constructing local ArcticDB libraries and deterministic replay fixtures
- treat these TXT files as a seed/import source, not the long-term execution format

### Export format for deterministic replay
- use `Parquet` for exported replay fixtures

### Data prep workflow
- [ ] define importer path from `~/Downloads/全套数据TXT` into local ArcticDB test libraries
- [ ] select representative libraries, symbols, venues, and date ranges
- [ ] materialize local ArcticDB slices
- [ ] normalize to replay schema
- [ ] export deterministic Parquet fixtures
- [ ] register fixtures in dataset manifest
- [ ] keep tiny fixtures in repo or reproducibly generated
- [ ] keep larger regression/benchmark sets outside the repo

## Dataset tiers

### Golden
- small
- deterministic
- CI-friendly

### Regression
- medium
- realistic
- local and scheduled test use

### Benchmark
- larger
- stress/performance use

## Reference backtest workload

- use `/Users/zzk/workspace/zklab/zkstrategy_research/zk-strategylib/zk_strategylib/entry_target_stop/ETS_kline_bt.py` as one of the baseline Python backtest workloads
- do not rely on its current remote-data assumptions for regression testing
- create a copied local-data variant or wrapper for deterministic test execution when implementing the data-prep pipeline
- include at least one `USDJPY` minute-kline scenario sourced from `~/Downloads/全套数据TXT`

## Dataset accessor responsibilities
- read ArcticDB for local preparation
- read Parquet fixtures for replay
- expose Python-friendly access for orchestration
- expose Rust-friendly stream/iterator access for execution cores
- enforce normalized schemas through manifest metadata
