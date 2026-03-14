# Pending Items

## Discovery / Registry

- Fix `KvDiscoveryClient` reconnect behavior so consumers do not see an empty cache while a new watch is still being established.
- Ensure every service that uses KV registration supervises `wait_fenced()` and exits or re-bootstraps on ownership loss.
- Decide whether full `lock_key` fencing is still required after the current CAS-based rollout or remains a deferred hardening item.

## RTMD Gateway

- Finalize whether standalone RTMD registrations should always use `svc.mdgw.<logical_id>` or may still use pure venue-scoped keys in limited cases.
- Decide the exact schema and API shape for effective desired RTMD subscription queries and whether aggregate ownership/debug data should be exposed.
- Decide whether a local-only embedded RTMD runtime should ever surface health to Pilot without registering a shared `mdgw` role.

## Doc Cleanup Follow-up

- If more architecture detail is needed for a specific service, expand the corresponding file under `docs/system-arch/services/` instead of reintroducing multi-topic design docs.
- Keep `docs/system-redesign-plan/plan/` focused on executable steps, rollout sequencing, and progress tracking.
