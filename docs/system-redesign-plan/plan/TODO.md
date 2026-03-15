# Pending Items

## Discovery / Registry

- Fix `KvDiscoveryClient` reconnect behavior so consumers do not see an empty cache while a new watch is still being established.
- Ensure every service that uses KV registration supervises `wait_fenced()` and exits or re-bootstraps on ownership loss.
- Decide whether full `lock_key` fencing is still required after the current CAS-based rollout or remains a deferred hardening item.

## RTMD Gateway

- Finalize whether standalone RTMD registrations should always use `svc.mdgw.<logical_id>` or may still use pure venue-scoped keys in limited cases.
- Decide the exact schema and API shape for effective desired RTMD subscription queries and whether aggregate ownership/debug data should be exposed.
- Decide whether a local-only embedded RTMD runtime should ever surface health to Pilot without registering a shared `mdgw` role.

## KV Bucket Naming (RESOLVED)

- `api_contracts.md` and `sdk.md` updated to `zk-svc-registry-v1` (dashes).
  NATS KV bucket names cannot contain dots; all plan and implementation files now use the dash form.
  Supersedes:
  (`ZK_DISCOVERY_BUCKET` default).
- The current implementation uses `zk-svc-registry-v1` (dashes) because NATS KV bucket
  names cannot contain dots.
- Plan files and the Phase 6 scaffolding plan use the dash form (correct for implementation).
- Decide: update `api_contracts.md` and `sdk.md` to use the dash form, or document the mismatch
  as a known naming constraint. The Phase 7 SDK implementation must use dashes regardless.

## Strategy Identity Naming

- Older plan and schema notes may still use `strategy_id` where `strategy_key` is now
  the canonical stable identity term (see `reconcile.md` item 2).
- `execution_id` identifies one concrete run; it is allocated by Pilot, not the engine.
- Audit remaining plan and schema files for residual `strategy_id` usage and update to
  `strategy_key` / `execution_id` as appropriate.

## Bootstrap Contract for Early Service Phases

- Phases 3 and 4 (OMS, Gateway) currently use direct KV registration without going through
  the Pilot bootstrap flow that Phases 5+ introduce.
- Once Phase 6 (scaffolding services) is delivered, revisit Phases 3 and 4 to add the unified
  bootstrap contract (token → Pilot scaffold → enriched config → Vault → KV registration).
- Reference: [Bootstrap And Runtime Config](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/bootstrap_and_runtime_config.md)

## Doc Cleanup Follow-up

- If more architecture detail is needed for a specific service, expand the corresponding file under `docs/system-arch/services/` instead of reintroducing multi-topic design docs.
- Keep `docs/system-redesign-plan/plan/` focused on executable steps, rollout sequencing, and progress tracking.
