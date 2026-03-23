# Consolidated Agent Prompt: Config Refactor With Secret Loading

Use `/Users/zzk/.claude/plans/misty-pondering-patterson.md` as the primary plan. Extend and execute
that plan so the resulting design and implementation treat runtime secret loading as a first-class
part of config management rather than a side topic.

Constraints and target decisions:

- Keep the existing 4-layer config model from `docs/system-arch/bootstrap_and_runtime_config.md`
- Bootstrap must return secret references only, never raw secret values
- Vault is the source of truth for runtime secret material
- Pilot is the control-plane owner for desired config, secret metadata, and ops provisioning
- Runtime services authenticate to Vault directly
- The initial runtime auth model is AppRole-based, provisioned by Pilot ops workflows
- The orchestrator backend is abstracted behind Pilot and supports:
  - local process management for dev/debug
  - k3s later
- Venue adaptors should not contain Vault client logic; the runtime host resolves refs before
  adaptor/client construction
- Keep runtime service behavior environment-agnostic; env-specific Vault address and AppRole
  material are injected by the orchestration backend

What to add to the plan:

1. Extend proto/config metadata work so runtime config explicitly carries secret-reference metadata
   and redaction rules.
2. Add a shared secret-ref model and resolution contract to the Rust config library.
3. Add manifest/schema support for:
   - fields that are secret refs
   - fields that are resolved secret outputs
   - reloadability and restart semantics when secret refs or resolved material change
4. Define Pilot ops responsibilities for:
   - writing/stashing business secrets into Vault (deferred)
   - creating/updating Vault policy/AppRole bindings
   - generating short-lived `secret_id` material
   - handing runtime identity material to the selected orchestrator backend
5. Define orchestration backend adapters for:
   - local process env injection in dev/debug
   - k3s Secret or equivalent injection later
6. Define runtime startup and reload flows for:
   - bootstrap returning desired config plus secret refs
   - runtime login to Vault
   - in-memory secret resolution
   - adaptor/client initialization
   - secret rotation and restart/reload behavior
7. Ensure `GetCurrentConfig` and drift inspection always redact secret values while still surfacing
   secret refs and change classification.

Expected deliverables from the refactor:

- updated proto/config model
- shared Rust config and secret-resolution support
- manifest/schema changes across service kinds and venue integrations
- Pilot-side ops/orchestration interfaces for secret provisioning and runtime identity injection
- runtime service changes for GW, OMS, engine, and MDGW where applicable
- clear startup/reload/rotation/error-handling rules
- tests covering redaction, secret-ref resolution, and backend-specific injection paths

Required design details:

- separate logical secret refs from physical Vault paths
- define the path/template expansion rules
- define AppRole scope granularity:
  - per workload class, per logical instance, or another justified choice
- define how local-process and k3s backends share one abstract runtime identity contract
- define failure behavior when Vault login or secret resolution fails before service registration
- define what metadata Pilot persists in DB versus what stays only in Vault or orchestrator-managed
  secrets

Files that should inform the work:

- `docs/system-arch/bootstrap_and_runtime_config.md`
- `docs/system-arch/ops.md`
- `docs/system-arch/services/pilot_service.md`
- `zkbot/java/src/main/java/com/zkbot/pilot/bootstrap/BootstrapService.java`
- `zkbot/java/src/main/java/com/zkbot/pilot/meta/VenueManifestLoader.java`
- `zkbot/venue-integrations/oanda/schemas/*.json`
- runtime config loaders in the Rust services and venue hosts

Implementation bias:

- prefer extending existing config/bootstrap mechanisms over inventing a parallel secret subsystem
- keep Pilot out of raw secret delivery at runtime
- keep the runtime host as the single place where `secret_ref` becomes resolved secret material
- keep the design portable between local-process and k3s orchestration

Output format:

- first update the plan structure with explicit secret-loading phases or sub-phases
- then enumerate code/doc changes by phase
- then identify risks, migration steps, and tests
