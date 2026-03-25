# Pilot config edits do not refresh GW schema provenance after schema version changes

- Date: 2026-03-24
- Time: 17:07 +08
- Domain: pilot, gw
- Reporter: codex
- Status: open

## Description

When a GW config schema version is bumped and synced, Pilot appears to validate against the active schema but the generic desired-config update path only updates provided_config. It does not also refresh schema_resource_key, schema_version, schema_content_hash, or venue schema provenance on cfg.gateway_instance. This can leave the saved GW row claiming old schema provenance after an operator edits config against a newer schema.

## Symptoms

- Operator bumps manifest version and syncs schema to DB
- Pilot active schema lookup moves to the new version
- Existing GW is stopped and config is edited
- Saved cfg.gateway_instance row can retain old schema_version and related provenance fields even though provided_config was edited against the newer schema
- Bootstrap still loads provided_config, so the inconsistency is mostly in control-plane provenance and drift tracking

## Impact

Pilot can persist misleading schema provenance for gateway configs, making drift inspection, auditability, and future schema-aware operations unreliable. Operators may believe a GW config is still attached to an old schema version even after editing it with the current active schema.

## Proposed Options Or Fixes

1. Update the GW config edit path to resolve active service-kind and venue-capability schema provenance at save time and persist the new schema_resource_key/schema_version/schema_content_hash fields in the same transaction as provided_config
2. Apply the same provenance refresh logic to OMS, ENGINE, MDGW, and REFDATA config updates where applicable
3. Add an API-level integration test covering: bump schema version, sync, edit existing GW config, verify provenance columns move to the new active versions
4. Surface schema provenance explicitly in the config edit/read API so operators can see which schema version the saved config is bound to

## Open Questions

- Is the current UI already editing against GET /v1/schema/service-kinds/gw/config?venue=<venue> for existing GW rows, or is it still using row-local provenance somewhere in the edit flow?
- Should Pilot bind edited config to the latest active schema automatically, or require an explicit operator choice when multiple active-compatible versions exist?
- Should venue-capability schemas gain explicit activate/deprecate endpoints similar to service-kind schemas?

## Evidence

- Files:
- zkbot/java/src/main/java/com/zkbot/pilot/config/DesiredConfigRepository.java
- zkbot/java/src/main/java/com/zkbot/pilot/topology/TopologyService.java
- zkbot/java/src/main/java/com/zkbot/pilot/topology/TopologyRepository.java
- zkbot/java/src/main/java/com/zkbot/pilot/schema/SchemaService.java
- zkbot/java/src/main/java/com/zkbot/pilot/bootstrap/BootstrapService.java
- zkbot/docs/system-arch/bootstrap_and_runtime_config.md
- zkbot/docs/system-arch/services/pilot_service.md
- zkbot/docs/system-arch/data_layer.md
- Logs:
- none
- Notes:
- SchemaService active lookup uses the active schema resource for service-kind and venue-capability manifests
- TopologyService resolves schema provenance from active schema resources on create
- DesiredConfigRepository.setDesiredConfig only updates provided_config and does not refresh schema provenance columns
- BootstrapService consumes provided_config from the service table, so runtime startup is less affected than control-plane provenance
