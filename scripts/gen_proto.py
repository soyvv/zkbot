#!/usr/bin/env python
"""Unified protobuf codegen driver for zkbot.

Single schema source: the versioned tree ``protos/zk/**/v1/*.proto``. The codegen
rule (see zb-00028 ADR) is:

  1. Rust / Java  -> normal pb code (prost via ``build.rs``; protobuf-java via Gradle).
  2. Python       -> BetterProto datamodel **and** pb2/grpc.
  3. Both Python outputs are generated from the **same** versioned ``.proto`` files.

This script drives the two Python outputs:

  * Python pb2/grpc    -> via ``grpc_tools.protoc`` into ``python/proto-pb/src/zk/**``.
  * Python BetterProto -> via ``grpc_tools.protoc`` + the BetterProto plugin into
    ``python/proto-betterproto/src/zk_proto_betterproto/zk/**/v1`` (natural namespace),
    followed by:
      - flat-name alias modules ``zk_proto_betterproto.<pkg>`` re-exporting
        ``zk_proto_betterproto.zk.<pkg>.v1`` — the import surface the PyO3 backtester
        wheel (``rust/crates/zk-pyo3-rs``) and the research strategies depend on.
      - ``scripts/gen_proto_legacy_compat.py`` post-pass for legacy enum aliases.

The PyO3 backtester wheel is an **active** ``zk_proto_betterproto`` consumer (it does
``py.import_bound("zk_proto_betterproto.{rtmd,oms,common}")`` at runtime), so this output
must not be retired without rebuilding/repointing that wheel.

Rust is generated at build time via ``zk-proto-rs/build.rs`` (prost-build). Java is
generated at build time by the Gradle ``com.google.protobuf`` plugin in
``java/zk-proto-java/`` directly from ``protos/``.

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
PROTO_BP_SRC = REPO_ROOT / "python" / "proto-betterproto" / "src" / "zk_proto_betterproto"
LEGACY_COMPAT_SCRIPT = REPO_ROOT / "scripts" / "gen_proto_legacy_compat.py"


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


def generate_python_betterproto() -> None:
    """Generate BetterProto Python modules from the versioned tree (same source as pb2).

    Output lands at ``zk_proto_betterproto/zk/<pkg>/v1`` (matching the proto package
    declarations). A post-pass then emits flat-name alias modules
    ``zk_proto_betterproto/<pkg>`` so consumers can keep importing
    ``zk_proto_betterproto.{rtmd,oms,common}`` — the surface the PyO3 backtester wheel
    and research strategies rely on.
    """
    log("== python betterproto (versioned tree) ==")
    ensure_clean_dir(PROTO_BP_SRC)

    # The plugin is installed as protoc-gen-python_betterproto alongside uv's python.
    bp_plugin = Path(sys.executable).with_name("protoc-gen-python_betterproto")
    if not bp_plugin.exists():
        raise RuntimeError(
            f"protoc-gen-python_betterproto not found next to {sys.executable}; "
            "ensure betterproto[compiler] is installed in the uv environment."
        )

    sources = collect_versioned_protos()
    if not sources:
        raise RuntimeError(f"No versioned .proto files found under {PROTO_ROOT / 'zk'}")
    log(f"  {len(sources)} source files")

    cmd = [
        sys.executable,
        "-m",
        "grpc_tools.protoc",
        f"--proto_path={PROTO_ROOT}",
        f"--plugin=protoc-gen-python_betterproto={bp_plugin}",
        f"--python_betterproto_out={PROTO_BP_SRC}",
    ]
    cmd.extend(str(p.relative_to(PROTO_ROOT)) for p in sources)
    run(cmd, cwd=PROTO_ROOT)

    _emit_flat_aliases()

    log("== legacy-compat post-pass ==")
    run([sys.executable, str(LEGACY_COMPAT_SCRIPT)])


def _emit_flat_aliases() -> None:
    """Create flat-name alias modules ``zk_proto_betterproto/<pkg>`` for each generated
    ``zk_proto_betterproto/zk/<pkg>/v1`` package.

    Preserves the legacy ``zk_proto_betterproto.<pkg>`` import surface (e.g. ``.rtmd``,
    ``.oms``, ``.common``) used by the PyO3 wheel and strategies, while the real
    generated code lives under the versioned namespace.
    """
    zk_root = PROTO_BP_SRC / "zk"
    if not zk_root.is_dir():
        raise RuntimeError(f"BetterProto output missing expected 'zk/' tree at {zk_root}")

    aliased: list[str] = []
    for v1_init in sorted(zk_root.glob("*/v1/__init__.py")):
        pkg = v1_init.parent.parent.name  # zk/<pkg>/v1 -> <pkg>
        alias_dir = PROTO_BP_SRC / pkg
        alias_dir.mkdir(parents=True, exist_ok=True)
        (alias_dir / "__init__.py").write_text(
            "# Auto-generated flat-name compatibility alias. Do not edit.\n"
            f"# Re-exports zk_proto_betterproto.zk.{pkg}.v1 under the legacy flat module\n"
            f"# name `zk_proto_betterproto.{pkg}` used by the PyO3 backtester wheel and\n"
            "# research strategies.\n"
            f"from zk_proto_betterproto.zk.{pkg}.v1 import *  # noqa: F401,F403\n",
            encoding="utf-8",
        )
        aliased.append(pkg)

    log(f"  flat aliases: {', '.join(aliased)}")


def main() -> int:
    log(f"repo root: {REPO_ROOT}")
    generate_python_pb()
    generate_python_betterproto()
    log("done.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
