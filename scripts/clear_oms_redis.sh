#!/usr/bin/env bash
# Clear OMS state from dev-stack Redis.
#
# Deletes all keys matching  oms:{OMS_ID}:*
# Key patterns (from zk-infra-rs::redis::key):
#   oms:{oms_id}:order:{order_id}
#   oms:{oms_id}:open_orders:{account_id}
#   oms:{oms_id}:balance:{account_id}:{asset}
#   oms:{oms_id}:position:{account_id}:{instrument}:{side}
#
# Usage:
#   ./scripts/clear_oms_redis.sh                     # OMS_ID=oms_dev_1
#   OMS_ID=oms_prod_1 ./scripts/clear_oms_redis.sh
#   OMS_ID="*" ./scripts/clear_oms_redis.sh          # clear ALL oms instances

set -euo pipefail

OMS_ID="${OMS_ID:-oms_dev_1}"
PATTERN="oms:${OMS_ID}:*"

# ── Resolve CLI command as an array (works with xargs) ────────────────────────
if [[ -n "${REDIS_CLI:-}" ]]; then
    CLI=("$REDIS_CLI" -h "${REDIS_HOST:-localhost}" -p "${REDIS_PORT:-6379}")
elif docker inspect "${REDIS_CONTAINER:-zk-dev-redis-1}" &>/dev/null 2>&1; then
    CONTAINER="${REDIS_CONTAINER:-zk-dev-redis-1}"
    CLI=(docker exec "$CONTAINER" redis-cli)
elif command -v redis-cli &>/dev/null; then
    CLI=(redis-cli -h "${REDIS_HOST:-localhost}" -p "${REDIS_PORT:-6379}")
else
    echo "ERROR: no redis-cli found and container '${REDIS_CONTAINER:-zk-dev-redis-1}' is not running." >&2
    exit 1
fi

echo "Pattern: ${PATTERN}"

count=$("${CLI[@]}" --scan --pattern "$PATTERN" | wc -l | tr -d ' ')

if [[ "$count" -eq 0 ]]; then
    echo "No keys found — nothing to do."
    exit 0
fi

echo "Found ${count} key(s). Deleting..."

# Pipe keys into batched DEL calls (xargs appends keys after the fixed args).
"${CLI[@]}" --scan --pattern "$PATTERN" | xargs -r -n 100 "${CLI[@]}" DEL

echo "Done. Deleted ${count} key(s)."
