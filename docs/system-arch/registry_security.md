# ZKBot Registry Security Design

Secure, simple service registration for NATS KV using Vault-backed credentials and bootstrap authorization.

## 1. Goals

- Keep service discovery simple for application services.
- Prevent rogue instances from overwriting registry entries.
- Avoid Pilot dependency in steady-state heartbeat/update path.
- Reuse existing Vault usage (especially for gateways).

## 2. Recommended Model

Use **Pilot bootstrap authorization + direct KV writes**.

- On first registration, workload service authenticates to Pilot with topology token.
- Pilot issues a scoped registration grant (KV key + lock key + scoped NATS credential).
- Service writes and heartbeats its own KV entry directly.
- Pilot is not on the recurring heartbeat path.

This gives:
- simple client behavior
- control-plane validation at bootstrap
- no heartbeat SPOF at control plane

## 3. Auth and ACL

## 3.1 NATS identities

Each service instance uses a scoped NATS identity (JWT/NKey or equivalent account user):

- `gw:<gw_id>`
- `oms:<oms_id>`
- `engine:<engine_id>`
- optional bootstrap identity + runtime grant identity

## 3.2 Permissions

Workload services:
- `PUB`: normal event subjects
- `SUB`: event streams they consume
- `SUB`: `$KV.zk-svc-registry-v1.>` (watch/read only)
- runtime registration grant allows `PUB` only to:
  - `$KV.zk-svc-registry-v1.<kv_key>`
  - `$KV.zk-svc-registry-v1.<lock_key>`

Pilot/bootstrap service:
- validates topology token
- issues scoped runtime grant
- optional: revokes/reissues grants

## 3.3 Credential delivery

**Vault scope**: Vault is used only by Trading Gateways to fetch exchange credentials (API key, API secret, private key). See [ops.md](ops.md) §1.

**NATS credentials for all other services** are delivered via env vars, CLI args, or k8s Secrets — not Vault. Each service instance receives its scoped NATS credential (JWT or NKey) as a startup input:
- `ZK_NATS_CREDS` env var — path to NATS credential file, or inline credential
- Alternatively mounted as a k8s Secret volume

Credential paths (for operator reference, not Vault paths):
- Gateway NATS cred: `nats-creds/gw/<gw_id>.creds`
- OMS NATS cred: `nats-creds/oms/<oms_id>.creds`
- Engine NATS cred: `nats-creds/engine/<engine_id>.creds`

Bootstrap tokens (for Pilot registration, see [topology_registration.md](topology_registration.md)) are also provided via env/args, not Vault.

## 4. Registration Protocol

## 4.1 Bootstrap authorization

Instance calls Pilot once via NATS request/reply:
- subject: `zk.bootstrap.register`
- request: `{ token, logical_id, instance_type, runtime_info }`

Pilot returns:
- `kv_key`
- `lock_key`
- `owner_session_id`
- `lease_ttl_ms`
- scoped runtime credential
- `status` / `error`

No gRPC bootstrap endpoint is required; NATS URL is sufficient for first registration.

## 4.2 Direct KV heartbeat/write

Service writes `ServiceRegistration` directly to `kv_key` and renews `lock_key`.

For each renewal:

1. update `lock_key` with owner/session and expiry using CAS
2. update `kv_key` registration payload using CAS
3. if CAS fails, treat as fenced and re-bootstrap

## 4.3 Lease and expiry

- heartbeat interval: 5s
- lease TTL: 20s
- if heartbeat missing, entry expires by lease
- consumers evict stale endpoints once expired

## 5. Pilot Availability Model

Pilot is required for bootstrap only:

- if Pilot is down, existing instances continue direct heartbeat
- new instances (or fenced instances) cannot bootstrap until Pilot recovers

## 6. Anti-Rogue Controls

- strict ACL: runtime grants can only update assigned `kv_key` + `lock_key`
- bootstrap-time identity-to-topology binding checks in Pilot
- owner/session token in lock payload:
  - reject updates if owner session does not match
- CAS revision check for updates
- alert on:
  - identity mismatch
  - unexpected key ownership change
  - excessive churn per service key

## 7. Failure Modes

- Pilot down:
  - existing instances continue
  - first-time bootstrap/reissue blocked
- NATS partial outage:
  - heartbeats delayed; TTL tuning must exceed normal transient jitter
- Vault unavailable:
  - existing sessions continue until token/credential expiry
  - startup of new instances may fail (expected fail-closed behavior)

## 8. Simpler Alternative (if registrar not desired)

Registrar model:

- services publish heartbeat subject only
- registrar is sole KV writer

Tradeoff:
- stronger central policy
- extra component and potential heartbeat-path dependency

Preferred for this redesign: **bootstrap-authorized direct KV model**.
