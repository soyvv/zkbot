# Agent Prompt: Refactor Refdata Service To Use `venue-integrations`

Use this prompt with a coding agent.

---

Refactor `zkbot/services/zk-refdata-svc` so venue-specific refdata loading follows the
`zkbot/venue-integrations` manifest model described in:

- `zkbot/docs/system-arch/venue_integration.md`

The current refdata service still uses a service-local loader registry under:

- `zkbot/services/zk-refdata-svc/src/zk_refdata_svc/loaders/`

That is now the wrong architecture boundary. The refdata host should stay generic, and venue logic
should live under:

- `zkbot/venue-integrations/<venue>/...`

## Primary references

- `zkbot/docs/system-arch/venue_integration.md`
- `zkbot/docs/system-arch/services/refdata_loader_service.md`
- `zkbot/docs/system-arch/api_contracts.md`
- `zkbot/docs/system-redesign-plan/plan/13-python-venue-bridge.md`
- `zkbot/docs/system-redesign-plan/plan/14-refdata-service.md`
- `zkbot/docs/domains/instrument_convention.md`

## Current code to refactor

- `zkbot/services/zk-refdata-svc/src/zk_refdata_svc/jobs/refresh_refdata.py`
- `zkbot/services/zk-refdata-svc/src/zk_refdata_svc/jobs/refresh_sessions.py`
- `zkbot/services/zk-refdata-svc/src/zk_refdata_svc/loaders/`
- `zkbot/venue-integrations/`

## Goal

Make the refdata service load venue-specific refdata adaptors from `zkbot/venue-integrations`
through the manifest/entrypoint pattern, instead of hardcoding venue classes inside the service.

The end state should be:

- generic refdata host in `zk-refdata-svc`
- venue-specific refdata adaptors in `zkbot/venue-integrations/<venue>/...`
- manifest-driven resolution of the `refdata` capability
- config-schema-aware loading path
- market session handling routed through the same venue adaptor model

## Hard constraints

1. Do not reintroduce MongoDB or any legacy loader path.
2. Do not keep two parallel loader registries long-term.
3. Keep the refdata host generic; venue-specific code belongs under `venue-integrations`.
4. Respect the manifest contract in `venue_integration.md`.
5. Refdata adaptors remain Python implementations.
6. Keep current gRPC/discovery behavior intact unless a change is required by the design docs.
7. Preserve the canonical `instrument_id` convention.
8. Avoid speculative plugin machinery beyond what is needed for the manifest-driven Python loading path.

## Required deliverables

### 1. Python venue-integration facade for refdata

Add a small Python-side integration loader in the refdata service, for example:

```text
zkbot/services/zk-refdata-svc/src/zk_refdata_svc/venue_registry.py
```

It should:

1. resolve `zkbot/venue-integrations/<venue>/manifest.yaml`
2. parse the manifest
3. validate that `capabilities.refdata` exists
4. validate that `capabilities.refdata.language == python`
5. resolve the manifest-declared Python entrypoint
6. validate config against the manifest-declared schema
7. instantiate the loader object

Do not hardcode venue class imports in the host once this is in place.

### 2. Stable Python refdata adaptor contract

Define a minimal Python-side contract for venue refdata adaptors.

Recommended shape:

```python
class SomeRefdataLoader:
    def __init__(self, config: dict | None = None): ...
    async def load_instruments(self) -> list[dict]: ...
    async def load_market_sessions(self) -> list[dict]: ...
```

`load_market_sessions()` may return an empty list for always-open crypto venues.

The host owns:

- scheduling
- diff/lifecycle handling
- PostgreSQL writes
- gRPC serving
- service discovery

The venue adaptor owns:

- venue-specific fetching
- venue-specific metadata normalization quirks
- venue-specific market-session source behavior

### 3. Move or wrap current service-local loaders

Port the current refdata venue logic out of:

- `zkbot/services/zk-refdata-svc/src/zk_refdata_svc/loaders/`

and into:

- `zkbot/venue-integrations/<venue>/...`

The service-local loader package should either:

- be deleted, or
- reduced to temporary compatibility wrappers that delegate to `venue-integrations`

Prefer deleting it if the migration can be done cleanly.

### 4. Manifest-driven config loading

Introduce a refdata venue config path so the host can load per-venue config and validate it
against:

- `schemas/refdata_config.schema.json`

This can be simple in the first pass. The important part is that the service no longer instantiates
adaptors as `loader_cls()` with no manifest/config/schema awareness.

### 5. Session integration

Refactor market-session refresh so it uses the same venue adaptor path instead of this current
logic:

- blanket `session_state='open'` for every configured venue

Required rule:

- if a venue manifest indicates session-constrained behavior or the adaptor provides session data,
  the service must consume adaptor-provided session state
- only crypto-style always-open venues may use the trivial default-open path, and even that should
  be explicit rather than assumed for every venue

### 6. Tests

Add or update tests for:

- manifest loading
- invalid/missing refdata capability in manifest
- invalid entrypoint format
- config-schema validation failure
- loading the correct Python refdata adaptor from `venue-integrations`
- `refresh_refdata()` using the venue facade instead of the old loader registry
- `refresh_sessions()` using adaptor-provided session data
- fallback session behavior for always-open venues

## Implementation guidance

- Keep the host-side integration loader small and boring.
- Prefer `importlib` + YAML parsing for the Python host path.
- Do not make the Python refdata service depend on the Rust bridge just to load Python adaptors.
- Align naming and contract shape with the long-term `13-python-venue-bridge.md` design so the
  eventual Rust/PyO3 host can mirror the same manifest semantics.
- Use the existing manifest format under `zkbot/venue-integrations/`.
- If a venue integration package is still a placeholder, either:
  - implement the refdata entrypoint there, or
  - add a temporary wrapper there that delegates to migrated logic

## Required review loop

You must use Codex to review the work before considering the task complete.

Review loop requirements:

1. Implement a coherent slice of the refactor.
2. Call Codex to review the diff against these docs:
   - `zkbot/docs/system-arch/venue_integration.md`
   - `zkbot/docs/system-arch/services/refdata_loader_service.md`
   - `zkbot/docs/system-redesign-plan/plan/13-python-venue-bridge.md`
   - `zkbot/docs/system-redesign-plan/plan/14-refdata-service.md`
   - `zkbot/docs/domains/instrument_convention.md`
3. Treat the following as major issues:
   - host still hardcodes venue loader classes
   - venue-specific code remains in the wrong boundary
   - manifests are ignored or only partially used
   - refdata config schema is bypassed
   - session handling remains globally hardcoded to `open`
   - tests fail to cover the new loading path
4. Fix the major issues Codex finds.
5. Repeat until Codex reports no major issues.

## Suggested Codex review prompt

Use something close to this:

```text
Review this refdata venue-integration refactor against:
- zkbot/docs/system-arch/venue_integration.md
- zkbot/docs/system-arch/services/refdata_loader_service.md
- zkbot/docs/system-redesign-plan/plan/13-python-venue-bridge.md
- zkbot/docs/system-redesign-plan/plan/14-refdata-service.md
- zkbot/docs/domains/instrument_convention.md

Focus on major issues only:
- incorrect architecture boundary between host and venue package
- manifest/config/schema loading gaps
- broken refdata adaptor resolution
- incorrect session integration behavior
- canonical instrument identity regressions
- missing critical tests
```

## Out of scope

Do not:

- build the full Rust-side PyO3 bridge in this task
- redesign service discovery
- redesign the refdata gRPC API
- add unrelated gateway or RTMD refactors unless required to keep manifests coherent

## Verification

Run the relevant refdata-service tests and any new tests you add.

At minimum, report:

1. which host-side files changed
2. which venue-integration packages now provide working refdata entrypoints
3. how manifest loading works
4. how session handling changed
5. Codex review findings and fixes
6. final test results

## Expected output from the agent

1. concise implementation summary
2. concise description of the new host/adaptor boundary
3. Codex review findings and fixes
4. verification results
5. any small deferred follow-ups
