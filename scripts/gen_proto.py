#!/usr/bin/env python
"""Unified protobuf codegen driver for zkbot.

Drives codegen for all language targets from the single schema source at `zkbot/protos/`:

  * Rust    -> via `buf generate` (prost plugin) into `rust/crates/zk-proto-rs/src`
  * Python pb2/grpc     -> via `grpc_tools.protoc` into `python/proto-pb/src/zk/**`
  * Python BetterProto  -> via `grpc_tools.protoc` into `python/proto-betterproto/src/zk_proto_betterproto`
                            (followed by `scripts/gen_proto_legacy_compat.py` post-pass)

Java is NOT driven here — the Gradle `com.google.protobuf` plugin regenerates Java at
build time directly from `zkbot/protos/` (not committed).

Source layout convention:
  - Python pb2 is generated from the VERSIONED tree `zkbot/protos/zk/**/v1/*.proto`,
    producing `zk/<pkg>/v1/*_pb2.py` with top-level `zk.*` imports.
  - Python BetterProto is (for now) generated from the FLAT root `.proto` files
    (common.proto, oms.proto, etc.) preserving the `zk_proto_betterproto.<pkg>` namespace
    shape that current BetterProto consumers (libs/zk-datamodel) expect. Migration of
    BetterProto to versioned tree tracked as follow-up after Stage 3.

Run: `uv run --python 3.13 python scripts/gen_proto.py`
Equivalent: `make gen`
"""
from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
PROTO_ROOT = REPO_ROOT / "protos"
PROTO_PB_SRC = REPO_ROOT / "python" / "proto-pb" / "src"
PROTO_BP_SRC = REPO_ROOT / "python" / "proto-betterproto" / "src" / "zk_proto_betterproto"
LEGACY_COMPAT_SCRIPT = REPO_ROOT / "scripts" / "gen_proto_legacy_compat.py"

# Flat root .proto files used for BetterProto generation (preserves current namespace shape).
FLAT_BETTERPROTO_SOURCES = [
    "common.proto",
    "exch-gateway.proto",
    "rpc-exch-gateway.proto",
    "oms.proto",
    "rpc-oms.proto",
    "ods.proto",
    "rpc-ods.proto",
    "rpc-refdata.proto",
    "rtmd.proto",
    "strategy.proto",
]


def log(msg: str) -> None:
    print(f"[gen_proto] {msg}", flush=True)


def run(cmd: list[str], cwd: Path | None = None) -> None:
    log(f"$ {' '.join(cmd)}")
    subprocess.run(cmd, cwd=cwd, check=True)


def ensure_clean_dir(path: Path) -> None:
    if path.exists():
        shutil.rmtree(path)
    path.mkdir(parents=True, exist_ok=True)


def generate_buf() -> None:
    """Run `buf generate` for non-Python targets (Rust, cpp). Java target in buf.gen.yaml
    points at a non-existent dir and is a harmless no-op; Gradle generates Java at build time."""
    log("== buf generate (Rust + cpp) ==")
    run(["buf", "generate"], cwd=PROTO_ROOT)


def collect_versioned_protos() -> list[Path]:
    return sorted((PROTO_ROOT / "zk").rglob("*.proto"))


def generate_python_pb() -> None:
    """Generate pb2/grpc Python modules from the versioned tree into python/proto-pb/src/zk/."""
    log("== python pb2/grpc (versioned tree) ==")
    # Clean only the `zk/` subtree we own; preserve pyproject layout files.
    zk_out = PROTO_PB_SRC / "zk"
    ensure_clean_dir(zk_out)

    sources = collect_versioned_protos()
    if not sources:
        raise RuntimeError(f"No versioned .proto files found under {PROTO_ROOT / 'zk'}")
    log(f"  {len(sources)} source files")

    cmd = [
        sys.executable,
        "-m",
        "grpc_tools.protoc",
        f"--proto_path={PROTO_ROOT}",
        f"--python_out={PROTO_PB_SRC}",
        f"--grpc_python_out={PROTO_PB_SRC}",
    ]
    cmd.extend(str(p.relative_to(PROTO_ROOT)) for p in sources)
    run(cmd, cwd=PROTO_ROOT)

    # Drop __init__.py at each zk package level so they import cleanly as namespace packages.
    for pkg_dir in zk_out.rglob("."):
        pass  # no-op; nested __init__.py already created as empty below
    _ensure_init_pys(zk_out)


def _ensure_init_pys(root: Path) -> None:
    """Write empty __init__.py at every directory under `root` (including `root` itself)."""
    for d in [root, *[p for p in root.rglob("*") if p.is_dir()]]:
        init = d / "__init__.py"
        if not init.exists():
            init.write_text("")


def generate_python_betterproto() -> None:
    """Generate BetterProto Python modules from flat root .proto files.

    Preserves the `zk_proto_betterproto.<pkg>` namespace shape that current consumers
    (libs/zk-datamodel) expect, so Stage 3 migration is a sed of `zk_datamodel` ->
    `zk_proto_betterproto`.
    """
    log("== python betterproto (flat root sources) ==")
    ensure_clean_dir(PROTO_BP_SRC)
    # The plugin is installed as protoc-gen-python_betterproto alongside uv's python.
    bp_plugin = Path(sys.executable).with_name("protoc-gen-python_betterproto")
    if not bp_plugin.exists():
        raise RuntimeError(
            f"protoc-gen-python_betterproto not found next to {sys.executable}; "
            "ensure betterproto[compiler] is installed in the uv environment."
        )

    sources_abs = [PROTO_ROOT / name for name in FLAT_BETTERPROTO_SOURCES]
    missing = [p for p in sources_abs if not p.exists()]
    if missing:
        raise RuntimeError(f"Missing flat proto sources: {missing}")

    cmd = [
        sys.executable,
        "-m",
        "grpc_tools.protoc",
        f"--proto_path={PROTO_ROOT}",
        f"--plugin=protoc-gen-python_betterproto={bp_plugin}",
        f"--python_betterproto_out={PROTO_BP_SRC}",
    ]
    cmd.extend(str(p.relative_to(PROTO_ROOT)) for p in sources_abs)
    run(cmd, cwd=PROTO_ROOT)

    log("== legacy-compat post-pass ==")
    run([sys.executable, str(LEGACY_COMPAT_SCRIPT)])


def main() -> int:
    log(f"repo root: {REPO_ROOT}")
    generate_buf()
    generate_python_pb()
    generate_python_betterproto()
    log("done.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
