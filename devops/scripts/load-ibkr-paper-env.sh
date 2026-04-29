#!/usr/bin/env bash
# Source this file:  . devops/scripts/load-ibkr-paper-env.sh [envfile]
#
# Reads IBKR paper TWS / IB Gateway connection settings and exports them for
# the Rust gw/rtmd hosts via the python-venue bridge.
#
# Required vars:   ZK_IBKR_HOST, ZK_IBKR_PORT, ZK_IBKR_CLIENT_ID, ZK_IBKR_ACCOUNT_CODE
# Optional:        ZK_IBKR_MODE (default: paper)
#
# NEVER prints secrets to stdout/stderr.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
ENV_FILE="${1:-$SCRIPT_DIR/../secrets/ibkr-paper.env}"

if [[ ! -f "$ENV_FILE" ]]; then
    echo "ERROR: IBKR paper env file not found: $ENV_FILE" >&2
    return 1 2>/dev/null || exit 1
fi

count=0
while IFS='=' read -r name value || [[ -n "$name" ]]; do
    [[ -z "$name" || "$name" =~ ^[[:space:]]*# ]] && continue
    name="$(echo "$name" | xargs)"
    value="$(echo "$value" | xargs)"
    export "$name=$value"
    count=$((count + 1))
done < "$ENV_FILE"

: "${ZK_IBKR_MODE:=paper}"
export ZK_IBKR_MODE

# ZK_IBKR_ACCOUNT_CODE may be empty (defaults to "all visible accounts").
missing=""
[[ -z "${ZK_IBKR_HOST:-}" ]] && missing="$missing ZK_IBKR_HOST"
[[ -z "${ZK_IBKR_PORT:-}" ]] && missing="$missing ZK_IBKR_PORT"
[[ -z "${ZK_IBKR_CLIENT_ID:-}" ]] && missing="$missing ZK_IBKR_CLIENT_ID"
if [[ -n "$missing" ]]; then
    echo "ERROR: missing required IBKR env vars:$missing" >&2
    return 1 2>/dev/null || exit 1
fi
export ZK_IBKR_ACCOUNT_CODE="${ZK_IBKR_ACCOUNT_CODE:-}"

echo "IBKR paper env loaded ($count vars, mode=$ZK_IBKR_MODE)" >&2
