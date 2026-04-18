## zkbot Monorepo

Polyglot trading stack organised by language root with cross-cutting top-level concerns.

- `protos/` – protobuf schema, source-of-truth (`zk/**/v1/*.proto`). Managed with [Buf](https://buf.build/).
- `python/`
  - `proto-pb/` – shared pb2/grpc library, installs as `zk-proto-pb` (exposes top-level `zk` namespace).
  - `proto-betterproto/` – shared BetterProto library, installs as `zk-proto-betterproto`.
  - `libs/{zk-client,zk-core,zk-data,zk-gw-utils,zk-rpc}/` – active Python libraries.
  - `services/zk-refdata-svc/` – active Python services.
  - `tools/` – zkbot CLI tools (`trade-doctor`, `loc-count`, `test_rtmd`).
  - `legacy/` – reference-only snapshots, excluded from the uv workspace.
- `java/` – multi-module Gradle build (`zk-proto-java`, `pilot-service`). Java protobuf code
  is generated at build time, not committed.
- `rust/` – Cargo workspace (core, OMS, gateway, backtester, services).
- `venue-integrations/{oanda,ibkr,okx,simulator}/` – venue adaptors, cross-cutting.
- `devops/` – local docker-compose dev stack and service Dockerfiles.
- `scripts/gen_proto.py` – single codegen driver for Rust, cpp, and Python.

### Quickstart

1. Install [uv](https://github.com/astral-sh/uv) and [Buf](https://buf.build/).
2. Bootstrap dependencies:
   ```bash
   uv sync --python 3.13 --all-groups --all-packages
   ```
3. Regenerate protobuf artifacts whenever `.proto` files change:
   ```bash
   make gen
   ```
4. Run quality checks:
   ```bash
   make lint
   make test
   ```

### Project Conventions

- Python: src-layout packages, Python 3.13, `uv` workspace, declared deps only (no PYTHONPATH hacks).
- Rust: Cargo workspace rooted at `rust/`.
- Java: Gradle multi-module (`cd java && ./gradlew :pilot-service:build`).
- Generated-code policy: see [docs/system-arch/dependency-matrix.md](docs/system-arch/dependency-matrix.md).

See individual package READMEs and `docs/system-arch/` for more detail.
