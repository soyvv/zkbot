# Auth And Authorization

## Goal

Keep authentication and authorization for the control plane lightweight while still being suitable
for an internal trading admin backend.

## Recommended Direction

Use:

- external OIDC for authentication
- Pilot-local RBAC for authorization

This keeps login/session handling standard without forcing the full control-plane permission model
into the identity provider.

## Why This Split

Pilot mainly needs:

- operator login
- token/session validation
- endpoint-level authorization
- a small set of internal roles

That does not require Pilot to become a full IAM system.

Recommended ownership:

- OIDC provider:
  - login
  - identity proof
  - token issuance
- Pilot:
  - role mapping
  - permission checks
  - endpoint authorization
  - environment-aware control-plane scoping

## Candidate Providers

### Lightweight Recommendation

Use a smaller OIDC provider and keep RBAC in Pilot.

Example candidate:

- ZITADEL

Rationale:

- API-first and usable as a self-hosted or managed OIDC provider
- better fit for a smaller internal control plane than a very large IAM stack

### Heavier Option

- Keycloak

Assessment:

- technically solid
- probably heavy for the current Pilot scope unless it is already part of the environment

## Pilot Authorization Model

Pilot should store coarse roles in its own control-plane tables.

Suggested roles:

- `admin`
- `trader`
- `bot_manager`
- `risk_manager`
- `ops`
- `viewer`

Suggested model:

- map OIDC subject/email/groups into a Pilot user record
- assign Pilot roles in PostgreSQL
- enforce endpoint authorization in Pilot itself

## Environment Model

`env` should be a property of the Pilot service/environment, not a user preference field.

Examples:

- one Pilot for `test`
- one Pilot for `prod`

Authorization checks may still use environment-aware scoping, but environment ownership belongs to
the deployed Pilot instance.

## Non-Goals

- Pilot should not become a general-purpose IAM product
- Pilot should not own secret-value administration; Vault UI/workflows can cover that

## TODO

- define exact Pilot RBAC schema
- define JWT validation/session middleware
- decide whether OIDC groups are mapped directly or indirectly into Pilot roles
- define operator-session audit fields
- confirm provider choice and licensing fit

## Related Docs

- [Architecture](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/arch.md)
- [Pilot Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/pilot_service.md)
