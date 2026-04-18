# Dependency Matrix — Generated Protobuf Artifacts

Authoritative contract for how each consumer obtains protobuf-generated code in the post-zb-00028
layout. All dependencies are declared in `pyproject.toml`, `Cargo.toml`, or `build.gradle.kts`
files — there are no vendored generated-code trees and no on-demand codegen at import time.
`sys.path` is reserved for runtime plugin loaders that resolve user-supplied paths (e.g.
`zk_strategy.strategy_core`, `zk_refdata_svc.venue_registry`); it is never used to paper over
missing workspace declarations.

## Per-consumer matrix

| Consumer | Schema source | Generated artifact consumed | Mechanism | Committed? |
|---|---|---|---|---|
| `python/proto-pb` | `protos/zk/**/v1/` | — (produces pb2/grpc) | `grpc_tools.protoc` via `scripts/gen_proto.py` | yes |
| `python/proto-betterproto` | `protos/*.proto` (flat root) | — (produces BetterProto) | `grpc_tools.protoc` + BetterProto plugin + legacy-compat post-pass | yes |
| `venue-integrations/oanda` | — | `zk-proto-pb` | pyproject dep | — |
| `venue-integrations/ibkr` | — | `zk-proto-pb` | pyproject dep | — |
| `python/libs/zk-core` (and peers) | — | `zk-proto-betterproto` | pyproject dep | — |
| `python/services/zk-refdata-svc` | — | `zk-proto-pb` | pyproject dep | — |
| `python/tools` (trade-doctor, loc-count) | — | `zk-proto-pb` | pyproject dep | — |
| `rust/crates/zk-proto-rs` | `protos/zk/**/v1/` | — (produces `.rs`) | `buf generate` (prost) | yes (status quo) |
| Other Rust crates | — | `zk-proto-rs` | Cargo workspace dep | — |
| `java/zk-proto-java` | `protos/` (via `srcDir("../../protos")`) | — (produces `.java`) | Gradle `com.google.protobuf` plugin at build time | **no** |
| `java/pilot-service` (and future peers) | — | `:zk-proto-java` | `implementation(project(":zk-proto-java"))` | — |

## Python import conventions

- pb2/grpc consumers: `from zk.<pkg>.v1 import <pkg>_pb2` and
  `from zk.<pkg>.v1 import <pkg>_pb2_grpc`. The `zk-proto-pb` package exposes `zk` as a
  top-level namespace package, so these imports resolve from the installed distribution with no
  extra configuration.
- BetterProto consumers: `from zk_proto_betterproto.<pkg> import <Message>` — replaces the
  pre-refactor `zk_datamodel.<pkg>` namespace. Enum/message shapes are identical; a legacy-compat
  post-pass re-adds historic enum aliases.

## Codegen driver

- Single entrypoint: `scripts/gen_proto.py` (invoked via `make gen`).
- Order: `buf generate` (Rust) → Python pb2 (versioned tree) → Python BetterProto
  (flat root) → legacy-compat post-pass.
- Java is **not** driven here — Gradle's `com.google.protobuf` plugin regenerates at every
  `./gradlew build` directly from `protos/`.

## Forbidden patterns

1. `sys.path.insert(...)` used to paper over missing workspace declarations. Internal code must be
   declared in `pyproject.toml`; `sys.path` is only acceptable inside runtime plugin loaders that
   resolve paths supplied by users or manifests.
2. `PYTHONPATH=…` workarounds in dev-stack scripts or tests. The `uv` workspace handles resolution.
3. Per-consumer vendored copies of generated code. Active venues consume `zk-proto-pb`; the
   pre-refactor vendored trees live at `venue-integrations/legacy/vendored-proto/` for reference
   only.
4. On-demand codegen at import time (e.g., `trade_doctor.py`'s old `.trade_doctor_generated/`
   stamp cache). Generated artifacts are either committed or produced by the build system.
5. Imports from `python/legacy/**` or `venue-integrations/legacy/**` from active code. Those trees
   are excluded from the uv workspace and must stay excluded.

## Verification

Run from repo root:

```bash
# generated-source parity
make gen && git diff --stat

# Python imports resolve from installed packages
uv run --python 3.13 python -c "from zk.rtmd.v1 import rtmd_pb2; from zk_proto_betterproto.oms import Order; print('ok')"

# Rust + Java + OMS parity
cd rust && cargo test -p zk-oms-rs
cd java && ./gradlew :pilot-service:build

# forbidden patterns (must be empty)
rg "from zk_datamodel" python/ venue-integrations/ -g '!**/legacy/**'
rg "sys\.path\.insert" python/ venue-integrations/ -g '!**/legacy/**'
rg "\.trade_doctor_generated"
rg "gen_proto\.sh"
```
