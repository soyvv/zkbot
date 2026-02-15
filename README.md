## zkbot Monorepo

This repository contains the polyglot trading stack for **zkbot**. The codebase is organised as a multi-language monorepo:

- `libs/` – shared Python libraries such as `zk-core`, `zk-client`, `zk-datamodel`, `zk-gw-utils`, and `zk-rpc`.
- `services/` – deployable services (gateways, refdata loaders, core APIs).
- `protos/` – protobuf contracts managed with [Buf](https://buf.build/).
- `rust/` – Rust crates that mirror the Python core/datamodel implementations.
- `polyglot/` – language-specific client stubs (Java, C++).
- `scripts/` – developer tooling (codegen, automation).

### Quickstart

1. Install [uv](https://github.com/astral-sh/uv) and [Buf](https://buf.build/).
2. Bootstrap dependencies:
   ```bash
   uv sync --all-groups
   ```
3. Regenerate protocol buffers whenever `.proto` files change:
   ```bash
   make gen
   ```
4. Run quality checks:
   ```bash
   make lint
   make test
   ```

### Project Conventions

- All Python packages follow the `src/` layout and are built with Hatch.
- Namespaces have migrated from the old `tq_*` prefixes to `zk_*`.
- Generated code (protos) is produced via Buf; only configurations live in source control.

See individual package READMEs for package-specific documentation.
