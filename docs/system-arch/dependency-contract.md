# Dependency Contract — Rust ↔ Python ↔ Java

Authoritative contract for how cross-language boundaries resolve code and artifacts across
local dev, CI, and production. This doc is the companion to
[dependency-matrix.md](dependency-matrix.md) (which covers generated-protobuf artifacts) and
extends that model to embedded runtimes, subprocess binaries, and native extensions.

The rule is simple: **every cross-language edge resolves through an explicit, configured
mechanism.** No `sys.path` tricks, no candidate lists, no auto-detection, no "dev-only"
escape hatches. CI, prod, and local dev follow the same contract.

## Scope and glossary

Three kinds of cross-language edges are covered:

- **Embedded runtime** — one process hosts another language's interpreter in-proc.
  Example: `zk-pyo3-bridge` and `zk-strategy-host-rs` embed CPython via PyO3.
- **Subprocess binary** — one process spawns another language's binary as a child.
  Example: `pilot-service` (Java/Spring) spawns `zk-engine-svc` (Rust).
- **Native extension** — one language consumes code compiled in another language as a
  loadable module. Example: Python code imports `zk_backtest` (PyO3 wheel built from
  `rust/crates/zk-pyo3-rs`).

## The three contract modes

Every edge MUST use one of these three modes. There are no other allowed forms.

1. **Installed-package import** (embedded runtime, Python plugin loading)
   - Caller supplies a Python interpreter whose `site-packages` already contains the
     target package.
   - Lookup is `importlib.import_module(name)` or PyO3 `py.import_bound(name)` where
     `name` is a fully-qualified module path.
   - The interpreter is selected via `VIRTUAL_ENV` / `PYO3_PYTHON`. No sys.path
     mutation, no venv probing.

2. **Artifact-path subprocess** (subprocess binary)
   - Caller reads an absolute filesystem path from a required config property.
   - The path must exist and be executable at startup; otherwise fail-fast.
   - No candidate lists, no bare-CLI-name fallback, no repo-relative guessing.

3. **Versioned wheel extension** (native extension)
   - Native extension is built as a wheel under a stable distribution name and
     consumed as a pyproject dependency.
   - In-repo consumers pin via `[tool.uv.sources]` to a wheel produced by
     `maturin build --release`. No `maturin develop`, no `target/…` on sys.path.
   - Python-visible module name is stable across releases and does NOT depend on the
     crate's Rust identifier.

## Per-edge rules

| Edge | Mode | Config knob | Forbidden |
|---|---|---|---|
| Rust embeds Python (`zk-pyo3-bridge`, `zk-strategy-host-rs`) | Installed-package import | `VIRTUAL_ENV`, `PYO3_PYTHON` | `sys.path` mutation, venv probing, `ZK_VENUE_VENV`, `ZK_VENUE_ROOT` |
| Python imports Rust extension (module `zk_backtest`, crate `zk-pyo3-rs`, dist `zk-backtest`) | Versioned wheel | `[tool.uv.sources]` path (or internal index) | `maturin develop`, `target/…` on `sys.path`, renaming the Python module |
| Java spawns Rust binary (`pilot-service` → `zk-engine-svc`) | Artifact-path subprocess | `pilot.engine-binary-path` (env: `PILOT_ENGINE_BINARY_PATH`) | Candidate lists, CLI-name fallback, repo-relative guessing, `ZK_ENGINE_BINARY_PATH` |
| Python plugin loading (refdata venue adaptors) | Installed-package import | Venue manifest references an installed module path | `sys.path.insert`, directory probing |
| Strategy loading (production) | Installed-package import | Strategy manifest uses `module:class` | `spec_from_file_location` outside `zk_strategy.dev` |
| Strategy loading (tests / dev CLI) | File-path helper | `zk_strategy.dev.load_from_file(...)` with context-manager sys.path cleanup | Leaking sys.path mutations out of the context manager |

## Forbidden patterns (grep-auditable)

The Stage 7 audit script (`scripts/audit_dependency_contract.sh`) enforces these with
zero-hit greps. The patterns here are the contract; the script is the enforcement.

1. `sys.path.insert` / `sys.path.append` outside sanctioned plugin loaders.
   Sanctioned exceptions: `python/libs/zk-core/src/zk_strategy/dev.py` (dev-only
   file-based strategy loader with context-manager cleanup). Test files may use
   pytest fixtures that manipulate `sys.path` locally — no top-level mutations.
2. `PYTHONPATH=…` in the `Makefile` or `scripts/`. Dockerfiles under
   `devops/` are tracked as a follow-up devops ticket (see §Out of scope).
3. `ZK_VENUE_VENV`, `ZK_VENUE_ROOT`, `ZK_DEV_MODE`, `ENGINE_PYTHONPATH`,
   `ZK_ENGINE_BINARY_PATH` — all removed, no aliases retained.
4. `maturin develop` in any docs, scripts, or Makefile.
5. `python_search_path` as a field in Rust structs, JSON schemas, or manifests.
6. `spec_from_file_location` outside `python/libs/zk-core/src/zk_strategy/dev.py`.
7. Repo-relative path probing in `BotService.java` (e.g., `candidate`, `fallback`
   tokens in the engine-binary resolution block).
8. Hard-coded relative reach-out from the repo: `../zkstrategy_research` in
   `Makefile`, `rust/`, or `python/` — the only allowed reference is inside
   `[tool.uv.sources]` in `pyproject.toml` where it resolves a uv workspace source.

## Environment variable contract

**Kept** (part of the contract):

| Var | Purpose |
|---|---|
| `VIRTUAL_ENV` | Selects the Python interpreter / site-packages for embedded and subprocess Python. Set by `uv run` and by Makefile targets that launch Rust services embedding Python. |
| `PYO3_PYTHON` | Absolute path to the Python interpreter for PyO3 to link against at runtime. |
| `PYTHONHOME` | Standard CPython initialization var; may be set where the host process needs to pin the interpreter's stdlib location. |
| `PILOT_ENGINE_BINARY_PATH` | Absolute path to the `zk-engine-svc` binary. Spring relaxed-binding of `pilot.engine-binary-path`. Required at pilot-service startup. |
| `ZK_VENUE_INTEGRATIONS_DIR` | Absolute path to the venue-integrations manifest root (manifest.yaml + schemas). Required when `zk-refdata-svc` (or any in-process host that calls `venue_registry`) needs to read venue manifests; no `__file__`-relative probing is performed. Python module imports themselves still go through installed-package resolution. |
| `ZK_SERVICE_MANIFESTS_ROOT` | Absolute path to the service-manifests root. Read by `pilot-service`. |

**Removed** (retired by zb-00029, no aliases):

| Var | What it used to do | Replacement |
|---|---|---|
| `ZK_VENUE_VENV` | Tell `zk-pyo3-bridge` which venv to add via `site.addsitedir`. | `VIRTUAL_ENV` / `PYO3_PYTHON` before process launch. |
| `ZK_VENUE_ROOT` | Prepend a venue repo root onto `sys.path`. | Install the venue package into the active venv (already a uv workspace member). |
| `ZK_DEV_MODE` | Toggle dev-only fallbacks (venv probing, path guessing). | Removed entirely. No fallbacks in any environment. |
| `ENGINE_PYTHONPATH` | Point the embedded Python at `../zkstrategy_research/zk-strategylib`. | `[tool.uv.sources] zk-strategylib = { path = "../zkstrategy_research/zk-strategylib", editable = true }`. |
| `ZK_ENGINE_BINARY_PATH` | Earlier binary path override read by `BotService`. | `PILOT_ENGINE_BINARY_PATH` (Spring-bound), no alias. |
| Ad-hoc `PYTHONPATH=…` | Per-target path munging in Makefile / scripts. | `uv sync` + workspace declarations. |

## Acceptance checklist for new Rust↔Python edges

When adding a new cross-language edge, confirm all of:

- [ ] The edge fits one of the three contract modes (installed-package import,
      artifact-path subprocess, versioned wheel).
- [ ] No new `sys.path` mutation in non-sanctioned locations.
- [ ] No new env var introduced for path resolution; reuse `VIRTUAL_ENV` /
      `PYO3_PYTHON` / the pilot `engine-binary-path` pattern.
- [ ] For embedded Python: target module is installed into the active venv (uv
      workspace member or pyproject dep), not reached via a repo-relative path.
- [ ] For native extensions: wheel is built via `maturin build --release` and
      consumed through `[tool.uv.sources]`. The Python-visible module name is
      stable and documented.
- [ ] For subprocess binaries: caller reads an absolute path from config and
      fail-fast validates executability at startup.
- [ ] `scripts/audit_dependency_contract.sh` passes on the resulting tree.
- [ ] This doc's per-edge table is updated.

## Migration notes (zb-00028 → zb-00029)

- zb-00028 normalized repo layout (uv workspace members under `python/libs`,
  `python/services`, `python/tools`, `venue-integrations/*`; Gradle multi-module
  under `java/`; unified `scripts/gen_proto.py`). See
  [dependency-matrix.md](dependency-matrix.md) for the protobuf half of the
  story.
- zb-00029 builds on that foundation and closes the remaining implicit edges:
  embedded Python runtimes, the `zk-pyo3-rs` extension, and the pilot engine
  subprocess. All five locked design decisions (A–E in the zb-00029 plan) are
  materialized in this document.
- Old ad-hoc dev setups that relied on `ZK_VENUE_VENV`, hand-rolled venvs
  outside the uv workspace, or `ZK_ENGINE_BINARY_PATH` exports will NOT keep
  working after this ticket lands. The `.env.example` and Makefile are updated
  to reflect the new contract; run `uv sync` and export
  `PILOT_ENGINE_BINARY_PATH` before launching the pilot-service.

## Out of scope (explicit follow-ups)

- **Dockerfiles under `devops/`** currently use `ENV PYTHONPATH=/app/src` +
  bare `pip install` lists. These predate the uv-workspace contract and should
  migrate to `uv sync` + installed-package imports. Tracked as a follow-up
  devops ticket (see zb-00029 Risks table). Until that lands, `devops/` is
  excluded from the audit rule 2 (`PYTHONPATH=`).
- **Publishing `zk-pyo3-rs` to an external index**: local `[tool.uv.sources]`
  path is the in-repo contract; a private index is a follow-up once release
  infra exists.
- **Restructuring `zkstrategy_research`**: only added as a uv workspace source.
