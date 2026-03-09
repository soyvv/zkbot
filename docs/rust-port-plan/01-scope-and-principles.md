# Scope And Principles

## In scope

- OMS core
- backtester event engine and simulator core
- live strategy engine orchestration
- shared runtime state model
- Python strategy compatibility layers
- Rust-native strategy SDK
- protocol updates needed for multi-strategy and multi-asset support
- testing, replay, and local data preparation infrastructure

## Out of scope for the initial port

- rewriting all Python strategies
- rewriting research notebooks and report generation in Rust
- replacing `NATS`, `protobuf`, `Redis`, or `ArcticDB`
- porting all exchange/refdata utilities first

## Non-negotiable constraints

- existing Python strategies must remain usable
- backtesting must still be orchestrated from Python
- local ArcticDB data must be usable for deterministic testing
- GIL-dependent memory-sharing behavior must be explicitly redesigned
- protocol changes should be backward-compatible where practical

## Success criteria

- Rust services and libraries match Python behavior on replay and parity suites
- researchers can run Python strategies on the Rust backtest core from Python
- live system supports Rust-native strategies and Python strategies via embedded or worker mode
- OMS mutable state ownership is explicit and race-safe
- protocol layer can express broader strategy types and instrument classes without ad hoc side channels
