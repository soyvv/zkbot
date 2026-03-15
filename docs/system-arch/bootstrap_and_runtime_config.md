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
