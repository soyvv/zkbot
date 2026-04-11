#!/bin/sh
# Vault entrypoint for dev: starts with file storage, auto-unseals, and ensures
# "dev-root-token" is always available so vault.sh seeding works unchanged.
set -e

VAULT_ADDR="http://127.0.0.1:8200"
export VAULT_ADDR

INIT_FILE="/vault/data/.init"

# Write config (VAULT_LOCAL_CONFIG is not processed when we replace the entrypoint)
mkdir -p /vault/config
cat > /vault/config/local.hcl <<'EOF'
storage "file" { path = "/vault/data" }
listener "tcp" { address = "0.0.0.0:8200" tls_disable = true }
disable_mlock = true
ui = true
EOF

# Start vault server in background
vault server -config=/vault/config &
VAULT_PID=$!

# Wait for vault API to respond
echo "[vault] Waiting for Vault to start..."
i=0
until vault status 2>&1 | grep -q "Initialized"; do
  i=$((i+1))
  if [ "$i" -gt 30 ]; then
    echo "[vault] Timeout waiting for Vault to start"
    exit 1
  fi
  sleep 1
done

# Initialize on first boot
if vault status 2>&1 | grep -q "Initialized.*false"; then
  echo "[vault] First boot — initializing (1-of-1 key)..."
  vault operator init -key-shares=1 -key-threshold=1 > "$INIT_FILE"
  FIRST_BOOT=1
fi

# Auto-unseal using stored key
echo "[vault] Unsealing..."
UNSEAL_KEY=$(grep "^Unseal Key 1:" "$INIT_FILE" | awk '{print $NF}')
vault operator unseal "$UNSEAL_KEY" > /dev/null

# On first boot: create the fixed dev-root-token so vault.sh works unchanged
if [ "${FIRST_BOOT:-0}" = "1" ]; then
  echo "[vault] Creating dev-root-token..."
  ROOT_TOKEN=$(grep "^Initial Root Token:" "$INIT_FILE" | awk '{print $NF}')
  VAULT_TOKEN="$ROOT_TOKEN" vault token create \
    -id=dev-root-token -policy=root -orphan -ttl=0 > /dev/null
  echo "[vault] Ready (dev-root-token active, secrets will persist across restarts)."
else
  echo "[vault] Ready (unsealed, existing data loaded)."
fi

wait "$VAULT_PID"
