#!/usr/bin/env bash
# Source this file:  . devops/scripts/load-okx-demo-env.sh [keyfile]
#
# Reads the OKX demo key file and exports credentials as environment variables.
# Also loads passphrase from okx-demo-passphrase.key if it exists.
# NEVER prints secrets to stdout/stderr.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
KEY_FILE="${1:-$SCRIPT_DIR/../secrets/okx-demo.key}"
PASSPHRASE_FILE="$SCRIPT_DIR/../secrets/okx-demo-passphrase.key"

if [[ ! -f "$KEY_FILE" ]]; then
    echo "ERROR: key file not found: $KEY_FILE" >&2
    return 1 2>/dev/null || exit 1
fi

count=0
while IFS='=' read -r name value || [[ -n "$name" ]]; do
    [[ -z "$name" || "$name" =~ ^[[:space:]]*# ]] && continue
    name="$(echo "$name" | xargs)"
    value="$(echo "$value" | xargs)"
    export "$name=$value"
    count=$((count + 1))
done < "$KEY_FILE"

# Load passphrase from separate file (raw value, not name=value).
if [[ -f "$PASSPHRASE_FILE" ]]; then
    export OKX_PASSPHRASE="$(cat "$PASSPHRASE_FILE" | tr -d '\n')"
    count=$((count + 1))
fi

echo "OKX demo credentials loaded ($count vars)" >&2
