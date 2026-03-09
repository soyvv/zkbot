#!/usr/bin/env bash
# Seeds the Vault dev container with mock gateway credentials.
# Uses docker exec so no local vault CLI is required.
set -euo pipefail

CONTAINER="${VAULT_CONTAINER:-zk-dev-vault-1}"

echo "Waiting for Vault container '$CONTAINER' ..."
until docker exec "$CONTAINER" \
    vault status -address=http://localhost:8200 >/dev/null 2>&1; do
  sleep 1
done
echo "Vault is ready."

# Enable the kv-v2 secrets engine at path "kv/" (idempotent — ignore if already mounted).
docker exec "$CONTAINER" \
  env VAULT_ADDR=http://localhost:8200 VAULT_TOKEN=dev-root-token \
  vault secrets enable -path=kv kv-v2 2>/dev/null || true

docker exec "$CONTAINER" \
  env VAULT_ADDR=http://localhost:8200 VAULT_TOKEN=dev-root-token \
  vault kv put kv/trading/gw/9001/api_key value="mock-api-key"

docker exec "$CONTAINER" \
  env VAULT_ADDR=http://localhost:8200 VAULT_TOKEN=dev-root-token \
  vault kv put kv/trading/gw/9001/api_secret value="mock-api-secret"

echo "Vault seeded."
