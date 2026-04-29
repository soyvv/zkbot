#!/usr/bin/env python
"""Unified protobuf codegen driver for zkbot.

Drives codegen for the Python pb2/grpc target only:

  * Python pb2/grpc -> via ``grpc_tools.protoc`` into
    ``python/proto-pb/src/zk/**`` from the versioned ``protos/zk/**/v1/*.proto``
    sources.

Rust is generated at build time via ``zk-proto-rs/build.rs`` (prost-build).
Java is generated at build time by the Gradle ``com.google.protobuf`` plugin
in ``java/zk-proto-java/`` directly from ``protos/``.

The legacy flat-style protos (``common.proto``, ``ods.proto`` …) and the
BetterProto generation flow were retired alongside the dead modules; if you
need to bring BetterProto back, see git history for ``gen_proto_legacy_compat.py``
and the previous version of this script.

Run: ``uv run --python 3.13 python scripts/gen_proto.py``
Equivalent: ``make gen``
"""
from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
PROTO_ROOT = REPO_ROOT / "protos"
PROTO_PB_SRC = REPO_ROOT / "python" / "proto-pb" / "src"


def log(msg: str) -> None:
    print(f"[gen_proto] {msg}", flush=True)


def run(cmd: list[str], cwd: Path | None = None) -> None:
    log(f"$ {' '.join(cmd)}")
    subprocess.run(cmd, cwd=cwd, check=True)


def ensure_clean_dir(path: Path) -> None:
    if path.exists():
        shutil.rmtree(path)
    path.mkdir(parents=True, exist_ok=True)


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

    _ensure_init_pys(zk_out)


def _ensure_init_pys(root: Path) -> None:
    """Write empty __init__.py at every directory under `root` (including `root` itself)."""
    for d in [root, *[p for p in root.rglob("*") if p.is_dir()]]:
        init = d / "__init__.py"
        if not init.exists():
            init.write_text("")


def main() -> int:
    log(f"repo root: {REPO_ROOT}")
    generate_python_pb()
    log("done.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
