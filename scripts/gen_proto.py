# scripts/gen_proto.py
from __future__ import annotations

import os
import shutil
import subprocess
import sys
from pathlib import Path


def ensure_init(out_dir: Path) -> None:
    init_py = out_dir / "__init__.py"
    if not init_py.exists():
        init_py.write_text("# generated models package\n", encoding="utf-8")


def main() -> int:
    repo_root = Path(__file__).resolve().parents[1]
    proto_dir = (repo_root / "protos").resolve()
    out_dir = (repo_root / "libs/zk-datamodel/src/zk_datamodel").resolve()

    buf_cmd = os.environ.get("BUF_CMD", "buf")
    if shutil.which(buf_cmd) is None:
        print(f"[ERR] '{buf_cmd}' not found on PATH. Install Buf or set BUF_CMD.", file=sys.stderr)
        return 2

    try:
        completed = subprocess.run(
            [buf_cmd, "generate", str(proto_dir)],
            cwd=repo_root,
            check=True,
            capture_output=True,
            text=True,
        )
    except subprocess.CalledProcessError as exc:
        sys.stderr.write(exc.stderr or "")
        print(f"[ERR] buf generate failed with exit code {exc.returncode}", file=sys.stderr)
        return exc.returncode
    else:
        if completed.stdout:
            sys.stdout.write(completed.stdout)

    ensure_init(out_dir)
    print(f"[OK] Buf generated protobuf outputs under {out_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
