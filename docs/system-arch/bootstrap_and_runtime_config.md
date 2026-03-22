# Bootstrap And Runtime Config

## Decision

All services should use the same bootstrap shape.

Bootstrap is the control-plane step that:

- authenticates the service identity
- authorizes whether the logical instance may start
- returns the effective runtime configuration
- returns registration/session metadata
- returns secret references, not secret material

This rule applies across gateway, OMS, engine, market data gateway, and other managed services.

All bootstrap-managed services should also follow one shared config-management contract:

- Pilot owns the desired runtime config
- the desired config is validated against a manifest/schema contract for that service kind
- the runtime applies that desired config as its effective config after bootstrap or reload
- the runtime exposes a default `GetCurrentConfig` style query so Pilot can inspect the live effective
  config

## Rationale

Using one bootstrap model reduces service-specific startup logic and keeps the control-plane
contract stable.

It also keeps concerns separated:

- deployment tooling owns only minimal bootstrap inputs
- Pilot owns effective runtime configuration and start authorization
- Vault owns secret material
- NATS KV owns runtime liveness

That split avoids:

- large per-service env/file configs drifting over time
- Pilot becoming a secret-distribution hub
- services becoming live before they have loaded required config and secrets

## Config Layers

### 1. Minimal Deployment Config

This is deployment-owned and intentionally small.

It should include only what the process needs to reach Pilot and Vault and identify itself.

Typical fields:

- `env`
- Pilot bootstrap endpoint
- NATS bootstrap endpoint if needed before enriched config is returned
- Vault endpoint and auth method bootstrap info
- workload identity inputs
- bootstrap token path or logical instance identity input
- local dev override flags if needed

It should not include the full service runtime config or raw secrets.

### 2. Effective Enriched Runtime Config

This is Pilot-owned control-plane config returned during bootstrap.

It may include:

- generic service runtime config
- service-specific config payload
- topology bindings
- venue/account scope
- capability flags
- registration metadata
- `secret_ref`

The service should switch to this config as its effective runtime configuration after bootstrap.

### 3. Manifest-Driven Desired Config

The desired config shape should be defined by a manifest/schema contract, not by ad hoc UI forms or
service-local environment variables.

Recommended rule:

- every bootstrap-managed service kind should declare a config manifest
- venue-backed services such as `gw`, `mdgw`, and refdata should use the venue integration manifest
- non-venue services such as `oms`, `engine`, and other managed runtimes should use a service-kind
  manifest/schema contract with the same logical purpose

The manifest/schema contract should define:

- supported config fields and their types
- defaults and enum values where appropriate
- capability flags
- which fields are reloadable vs restart-required
- which fields are secret references vs ordinary config

Pilot should use this manifest/schema contract to:

- render config authoring forms
- validate desired config before persistence
- classify drift and change impact
- decide whether a change can be applied by reload or requires restart

### 4. Runtime Config Introspection

Every bootstrap-managed runtime should expose a default `GetCurrentConfig` style query.

Purpose:

- let Pilot inspect the currently loaded effective runtime config
- compare desired config in control-plane storage against live effective config
- expose operator-visible drift information
- support reload/restart decisions

Recommended response contents:

- normalized effective config payload
- service/runtime config revision or version if available
- `loaded_at`
- `config_source`
- reloadability metadata if the runtime knows it
- optional config hash

Security rule:

- `GetCurrentConfig` must redact secret material
- secret references may be returned, but raw secret values must not be returned

## Bootstrap Token Decision

The default bootstrap authentication model is:

- every managed service uses a bootstrap token
- the bootstrap token is distributed by deployment tooling as part of the minimal deployment config
- the token is used only for Pilot bootstrap

Recommended guardrails:

- scope the token to:
  - `logical_id`
  - `instance_type`
  - `env`
- store only the token material or token file path in deployment-managed secrets/config
- rotate the token through normal deployment rollout
- do not reuse bootstrap tokens as general runtime API credentials

Why this is the default:

- simplest to implement and operate
- works uniformly across gateway, OMS, engine, and RTMD gateway
- keeps Vault focused on runtime secrets instead of bootstrap complexity
- matches the minimal deployment config model

## Alternative Bootstrap Identity Models

These are valid later options, but not the default design:

- workload-identity-issued bootstrap assertion
  - the service proves identity from Kubernetes/workload identity and exchanges that for a short-lived bootstrap grant
- Vault-backed bootstrap secret retrieval
  - deployment config contains only enough identity to read a bootstrap token from Vault first
- mTLS or SPIFFE-style service identity bootstrap
  - service uses platform-issued cert identity instead of a pre-distributed token
- short-lived bootstrap token minting service
  - deployment or operator workflow requests an ephemeral token shortly before startup

These alternatives improve credential hygiene, but they add platform and operational complexity.
The phase-1 design should stay with deployment-distributed bootstrap tokens.

## Secret Rule

Bootstrap must not return raw secrets.

Instead:

- Pilot returns `secret_ref` metadata
- the service authenticates directly to Vault using workload identity
- the service reads secret material from Vault

This applies to all services that need private credentials, especially trading gateways.

## Unified Startup Contract

The recommended startup sequence for any managed service is:

1. start with minimal deployment config
2. authenticate to Pilot using the bootstrap identity/token
3. receive bootstrap grant plus effective enriched runtime config
4. fetch required secrets from Vault using `secret_ref`
5. initialize internal runtime/adaptors/clients
6. register live endpoint/state in NATS KV
7. begin serving traffic or publishing data

If any step before KV registration fails, the service should fail startup without becoming live.

## Unified Shutdown Contract

The recommended shutdown sequence is:

1. stop accepting new work
2. withdraw or delete the live KV registration
3. notify Pilot if graceful deregistration is supported
4. stop background loops and exit

Hard shutdown is handled by KV lease loss and Pilot reconciliation, not by a special service-local
contract.

## Drift And Restart Policy

Pilot should compare:

- desired config from control-plane storage
- current effective config from the runtime `GetCurrentConfig` query

Decision rule:

- if there is no material diff, Pilot should not trigger change
- if the diff is within fields marked reloadable by the manifest/schema contract, Pilot may issue
  reload
- if the diff touches restart-required fields, Pilot should surface restart-required state and let
  the operator trigger restart explicitly

Pilot should not infer reload vs restart from UI heuristics alone.

## Notes

- Service-specific docs may refine what the enriched config contains.
- Service-specific docs should not redefine the overall bootstrap shape unless there is a clear
  exception.

## Related Docs

- [Architecture](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/arch.md)
- [Service Discovery](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/service_discovery.md)
- [Pilot Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/pilot_service.md)
- [Gateway Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/gateway_service.md)
- [Engine Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/engine_service.md)
- [Market Data Gateway Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/market_data_gateway_service.md)
