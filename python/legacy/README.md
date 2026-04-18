# python/legacy

Reference-only snapshots of pre-refactor Python code. **Not imported by active code**, not a uv workspace member, not installable.

Contents exist purely as historical reference for the zb-00028 restructure.

## zk-datamodel-pre-refactor/

Snapshot of the BetterProto datamodel before it was extracted into shared `python/proto-betterproto/`. Active consumers now import from `zk_proto_betterproto.*`.

## services/

- `zk-pilot/` — control-plane service prototype. Superseded by the upcoming Pilot rewrite in Rust.
- `zk-refdata-loader/` — exchange reference-data loader. Active loader lives in `python/services/zk-refdata-svc/`.
- `zk-service/` — monolithic pre-refactor service bundle (OMS/strategy/simulator Python implementation). Active OMS lives in `rust/crates/zk-oms-svc/`; strategy/simulator ports tracked under `docs/rust-port-plan/`.

These are not installable, not unit-tested, and may not even import cleanly under the current workspace. They exist for reference only.
