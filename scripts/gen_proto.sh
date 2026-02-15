#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

BUF_CMD=${BUF_CMD:-buf}
PROTO_ROOT="protos"

if ! command -v "$BUF_CMD" >/dev/null 2>&1; then
  echo "[ERR] buf CLI not found on PATH; install Buf or set BUF_CMD" >&2
  exit 1
fi

"$BUF_CMD" generate "$PROTO_ROOT"
echo "✅ buf generate complete."
