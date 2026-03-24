# ZKBot Topology + Token Registration Design

Pilot-authorized bootstrap registration with direct KV heartbeat thereafter.

## 1. Objective

Provide a managed registration flow where:

- topology is declared upfront by admin in Pilot
- each logical instance has a dedicated token
- first registration is allowed only if token and topology both match
- after bootstrap, instance can register/heartbeat directly to KV without Pilot dependency
- duplicate token usage is detected and rejected (or controlled takeover)

This keeps runtime startup simple: start process with one token.

## 2. Topology Model (Control Plane)

Pilot stores desired topology in PostgreSQL.

Core entities:

- `logical_instance`
  - `instance_type`: coarse category such as `GW`, `OMS`, `ENGINE`, `STRATEGY`, `COMPOSITE`
  - `logical_id`: stable logical identity granted by Pilot
  - `env`, `tenant`, `enabled`
  - `metadata`:
    - registration profile / kind
    - optional constraints: region, venue, account scope
    - business-specific bindings and singleton policy
- `binding`
  - `engine_id -> oms_id`
  - `oms_id -> gw_id[]`
  - `strategy_key -> engine_id`

Only registrations that satisfy configured bindings are accepted.

Design rule:

- the registry/discovery mechanism should remain generic
- Pilot should use `logical_instance.metadata` to interpret business-specific registration kinds such as:
- `gw`
- `oms`
- `strategy`
- `oms+gw`
- `oms+gw+strategy`

Metadata/profile guidance:

- `instance_type` should remain a coarse category, not the full business meaning
- `cfg.logical_instance.metadata` carries the registration profile and control-plane policy

Typical metadata fields include:

- `registration_kind`
- `strategy_key`
- bound OMS/GW logical IDs
- venue/account scope
- singleton policy

Pilot uses that metadata to decide:

- whether registration is allowed
- what config payload to return
- what duplicate policy to apply
- what stable logical KV key to grant

Config ownership note:

- `cfg.logical_instance` is the identity/topology authority used for bootstrap-token validation,
  session ownership, and topology views
- desired control-plane config should live in service-specific tables as `provided_config`
- for venue-scoped refdata config, Pilot stores `provided_config` in `cfg.refdata_venue_instance`
- if a refdata runtime bootstraps as a logical service, it still needs a matching
  `cfg.logical_instance` row in addition to `cfg.refdata_venue_instance`

## 3. Token Model

One token per logical instance.

Claims:

- `sub`: logical id
- `instance_type`
- `env`
- `tenant`
- `capabilities` (optional)
- `jti` (token id)
- `exp`, `iat`

Token properties:

- signed by Pilot key (JWT/PASETO)
- normal expiry and rotation policy
- revocable in Pilot DB

Current bootstrap output:

- `registration_grant`:
  - `kv_key` (e.g. `svc.gw.<gw_id>`)
  - `lock_key` (currently returned as reserved/compatibility metadata)
  - `owner_session_id`
  - `lease_ttl_ms`
  - `instance_id` (engines only)

Simplification rule for now:

- the current contract does not depend on scoped per-key runtime credentials
- `lock_key` may still be present on the wire, but it is not the active ownership mechanism today
- duplicate ownership is enforced by Pilot session checks plus CAS updates on `kv_key`
- stronger credential scoping and extra ownership keys are later hardening topics

## 4. Registration Flow

1. Instance starts with token from secure config (Vault/env/secret mount).
2. Instance sends NATS request to Pilot bootstrap subject:
   - subject: `zk.bootstrap.register`
   - pattern: request/reply over NATS
3. Pilot validates:
   - token signature + expiry
   - token not revoked
   - token claims match requested logical instance
   - topology constraints and bindings
   - duplicate policy for active session
4. Pilot returns:
   - `registration_grant` (`kv_key` + session metadata, with reserved compatibility fields)
   - any type-specific runtime config required for startup
5. Instance writes registry entry directly to KV and starts direct heartbeat loop.

Pilot is not in the steady-state heartbeat path.
Bootstrap dependency is NATS only (`ZK_NATS_URL`); no gRPC endpoint is required.

Runtime info includes:

- process/node identity
- service endpoint (`protocol`, `address`, `authority`)
- build/version
- optional capability flags

Design rule:

- Pilot owns business-specific startup/config decisions
- runtime instances publish generic service registrations into KV
- consumers derive business routing from registration content and Pilot-managed metadata, not from one-off discovery protocols per service class

## 5. Duplicate Token and Session Policy

Default policy: **single active session per logical instance**.

For strategies, this should be interpreted as:

- only one active execution instance per `strategy_key` at a time
- `execution_id` identifies one concrete run and must be freshly allocated for each new execution
- the stable logical discovery identity should remain strategy-scoped, while `execution_id` is carried as execution metadata

Duplicate checks happen at two levels:

- bootstrap time (Pilot): reject duplicate active topology session
- runtime KV updates: enforce ownership with CAS on `kv_key`

Optional controlled takeover:

- requires explicit `takeover=true` + privileged claim or operator action
- old session is fenced and expires

This provides immediate detection of duplicate token usage and runtime fencing without requiring a
second ownership key.

## 6. Heartbeat + Lease (Direct KV)

After registration:

- instance renews the service `kv_key` directly in NATS KV
- updates use CAS and must carry matching session ownership metadata
- if CAS fails, instance is fenced and must re-bootstrap with Pilot
- stale sessions expire automatically on lease timeout

Registry entries are valid only while lease is active.

## 7. Registry Integration

Current implementation direction:

- Pilot bootstrap + direct KV writes by instance
- CAS heartbeat on `kv_key`

This keeps Pilot out of steady-state and avoids heartbeat SPOF concerns.

## 8. Bootstrap Contract (NATS Request/Reply)

NATS subjects:

- `zk.bootstrap.register` (required)
- `zk.bootstrap.deregister` (optional)

`zk.bootstrap.register` request fields:

- `token`
- `logical_id`
- `instance_type`
- `runtime_info`

`zk.bootstrap.register` response fields:

- `owner_session_id`
- `kv_key`
- `lock_key`
- `lease_ttl_ms`
- `instance_id`
- `server_time_ms`
- `status` / `error`

All bootstrap operations are NATS request/reply so runtime instances only need `ZK_NATS_URL` to start.

Later hardening topics:

- `zk.bootstrap.reissue`
- `zk.bootstrap.sessions.query`
- scoped runtime credentials
- active `lock_key` ownership path

## 9. Data Model Additions

Suggested tables:

- `cfg.logical_instance`
- `cfg.logical_binding`
- `cfg.instance_token` (hashed token reference / jti / status)
- `mon.active_session`
- `mon.registration_audit`

Audit records should include:

- token `jti`
- logical id
- decision (`ACCEPTED`, `REJECTED`, `FENCED`)
- reason
- source address and timestamp

## 10. Security Notes

- tokens should be loaded from Vault-backed secret distribution where possible
- never log full token values
- store only token hash or `jti` for audit/revocation
- enforce strict clock sync (NTP) for exp/iat validation
- if stricter runtime KV credentials are introduced later, scope them to exact `kv_key`/`lock_key`
  subjects only

## 11. Operational Simplicity

Operator workflow:

1. define topology in Pilot UI/API
2. issue token per logical instance
3. first start: service bootstraps with Pilot and obtains registration grant
4. service registers and heartbeats directly in KV using the granted `kv_key`
5. duplicates are blocked by bootstrap policy and KV CAS fencing

This keeps onboarding and restart workflows simple while preserving strict control.

For engine/strategy-style workloads:

1. engine asks Pilot to start/claim execution for a `strategy_key`
2. Pilot enforces singleton policy and returns config plus a fresh `execution_id`
3. engine registers under a stable logical KV key for the strategy
4. `execution_id` is included in runtime metadata and lifecycle records
5. graceful shutdown finalizes with Pilot; hard shutdown is recovered through KV loss and Pilot reconciliation

## 12. Runtime Sequences

### 12.1 Normal Restart With Different IP (valid token)

Scenario:
- logical instance is valid (`logical_id` unchanged)
- node/pod restarts and gets a new IP
- token remains valid

Sequence:
1. instance starts with `token` and `ZK_NATS_URL`.
2. instance sends `zk.bootstrap.register` request (token + runtime_info with new IP/endpoint).
3. Pilot validates token and topology binding.
4. Pilot checks current active session for `logical_id`.
5. If prior session is stale/expired:
   - Pilot returns new `registration_grant` (`owner_session_id`, `kv_key`, `lock_key`, TTL, scoped credential).
6. instance writes KV registration with new endpoint and starts direct KV heartbeat.
7. consumers watching KV refresh endpoint and reconnect.

Optional takeover behavior:
- if old session still appears active but is unreachable, operator or privileged `takeover=true` policy can fence old session and issue a new grant.

Expected result:
- same logical instance is recovered at a new network location without manual topology edits.

### 12.2 Misconfigured Startup With Duplicate Token

Scenario:
- second node starts with the same token/logical identity while original node is still active.

Sequence:
1. duplicate node sends `zk.bootstrap.register` with same token/logical_id.
2. Pilot validates token syntax/signature but detects existing active session for that logical instance.
3. Pilot rejects registration with `status=REJECTED`, `error=ALREADY_ACTIVE`.
4. Pilot writes audit entry in `mon.registration_audit` with decision `REJECTED`.
5. duplicate node does not receive usable grant and cannot write/heartbeat KV entry.

Runtime lock safety (defense in depth):
- if duplicate node somehow obtains stale credentials and attempts KV update, lock/CAS ownership checks fail and node is fenced.

Expected result:
- duplicate token usage is detected early and blocked before registry corruption.

### 12.3 Strategy Hard Shutdown And Replacement

Scenario:
- strategy execution crashes without graceful deregistration
- replacement engine for the same `strategy_key` must be allowed only after liveness is lost

Sequence:
1. engine starts and asks Pilot to claim/start execution for `strategy_key`.
2. Pilot validates policy and returns:
   - strategy config
   - stable logical registration identity
   - fresh `execution_id`
   - KV registration grant
3. engine registers its generic service record in KV under the stable logical key and starts CAS heartbeat.
4. engine crashes hard (`SIGKILL`, node loss, process panic, etc.).
5. no explicit deregister occurs.
6. KV entry disappears by delete/purge/expiry or is observed missing on reconciler rebuild.
7. Pilot fences the old execution and marks it crashed/expired.
8. replacement engine requests start for the same `strategy_key`.
9. Pilot issues a new `execution_id` and allows startup.

Expected result:
- only one live execution per strategy at a time
- hard shutdown recovery does not depend on graceful callbacks
- `execution_id` is unique per run and never reused
