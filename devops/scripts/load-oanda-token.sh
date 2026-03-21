#!/usr/bin/env bash
# Usage: source devops/scripts/load-oanda-token.sh [path]
# Reads OANDA token from file and exports as ZK_OANDA_TOKEN.
# Default path: devops/secrets/oanda-demo.key
set -euo pipefail

TOKEN_FILE="${1:-devops/secrets/oanda-demo.key}"
if [[ ! -f "$TOKEN_FILE" ]]; then
    echo "ERROR: Token file not found: $TOKEN_FILE" >&2
    return 1 2>/dev/null || exit 1
fi

export ZK_OANDA_TOKEN
ZK_OANDA_TOKEN="$(tr -d '[:space:]' < "$TOKEN_FILE")"
echo "Loaded OANDA token (${#ZK_OANDA_TOKEN} chars)"
