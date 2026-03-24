# GW Bootstrap/Config Refactor Design

**Date:** 2026-03-24
**Scope:** `zk-gw-svc` adopts `zk_infra_rs::bootstrap::ServiceBootstrap` trait

## Problem

`zk-gw-svc` uses a flat `GwSvcConfig` struct loaded entirely from env vars. In Pilot mode, `pilot_reg.payload` is extracted but discarded. Simulator-only fields are always present regardless of venue. There is no typed config validation, no config source tracking, and no path to Pilot-managed config.

## Goal

- Split `GwSvcConfig` into bootstrap/provided/runtime config layers following the `ServiceBootstrap` trait contract.
- In Pilot mode, decode `pilot_reg.payload` into typed `GwProvidedConfig`.
- Keep direct mode working (env-var-based).
- Fail early on invalid Pilot config or inconsistent simulator/venue combinations.
- Carry `secret_refs` through `BootstrapOutcome` for future Vault resolution.

## Non-Goals

- Vault secret resolution (future phase).
- Adding `GetCurrentConfig` gRPC endpoint (future phase; data structures prepared).
- Changing venue adapter interfaces.
- OMS bootstrap adoption (already separate work).

## Type Definitions

### GwBootstrapConfig

Minimal env/bootstrap inputs. Only what's needed to reach NATS and Pilot.

```rust
/// Loaded from env vars at process start. Never from Pilot.
#[derive(Debug, Clone)]
pub struct GwBootstrapConfig {
    pub gw_id: String,            // ZK_GW_ID, default "gw_sim_1"
    pub grpc_host: String,        // ZK_GRPC_HOST, default "127.0.0.1"
    pub grpc_port: u16,           // ZK_GRPC_PORT, default 51051
    pub nats_url: Option<String>, // ZK_NATS_URL, optional
    pub bootstrap_token: String,  // ZK_BOOTSTRAP_TOKEN, empty = direct mode
    pub instance_type: String,    // ZK_INSTANCE_TYPE, default "GW"
    pub env: String,              // ZK_ENV, default "dev"
}

impl GwBootstrapConfig {
    /// Load from ZK_* env vars with defaults. Not part of ServiceBootstrap trait —
    /// each service owns its own bootstrap loading.
    pub fn from_env() -> Result<Self, BootstrapConfigError> { ... }
}
```

### GwProvidedConfig

Pilot-owned (or direct-mode equivalent). The business config that Pilot validates against the service manifest.

```rust
/// In Pilot mode: deserialized from PilotPayload.runtime_config_json.
/// In direct mode: loaded from remaining ZK_* env vars.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GwProvidedConfig {
    pub venue: String,                              // required, no default
    pub account_id: i64,                            // required, no default (was 9001 — now explicit)
    #[serde(default = "default_venue_config")]
    pub venue_config: serde_json::Value,            // default: {}
    #[serde(default)]
    pub venue_root: Option<String>,
    #[serde(default = "default_exec_shard_count")]
    pub exec_shard_count: usize,                    // default: 4
    #[serde(default = "default_exec_queue_capacity")]
    pub exec_queue_capacity: usize,                 // default: 256
    /// Present only when venue == "simulator".
    #[serde(default)]
    pub simulator: Option<SimulatorProvidedConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulatorProvidedConfig {
    #[serde(default = "default_mock_balances")]
    pub mock_balances: String,                      // default: "BTC:10,USDT:100000,ETH:50"
    #[serde(default)]
    pub fill_delay_ms: u64,                         // default: 0
    #[serde(default = "default_match_policy")]
    pub match_policy: String,                       // default: "immediate"
    #[serde(default = "default_admin_grpc_port")]
    pub admin_grpc_port: u16,                       // default: 51052
    #[serde(default)]
    pub enable_admin_controls: bool,                // default: false
}
```

**Intentional behavior change:** `venue` and `account_id` are now required (no defaults). The old `GwSvcConfig` defaulted `account_id` to 9001 and `venue` to "simulator" — these silent defaults are removed to force explicit configuration. In direct mode, `load_direct_config` reads them from env vars and fails if missing.

### GwRuntimeConfig

Effective config the service uses to run. Pure business config — no bootstrap metadata.

```rust
/// Assembled from GwBootstrapConfig + GwProvidedConfig.
/// This is what downstream code (adapter factory, exec pool, gRPC handler) consumes.
#[derive(Debug, Clone)]
pub struct GwRuntimeConfig {
    // From bootstrap
    pub gw_id: String,
    pub grpc_host: String,
    pub grpc_port: u16,
    pub nats_url: Option<String>,
    pub instance_type: String,
    pub env: String,
    // From provided
    pub venue: String,
    pub account_id: i64,
    pub venue_config: serde_json::Value,
    pub venue_root: Option<String>,
    pub exec_shard_count: usize,
    pub exec_queue_capacity: usize,
    pub simulator: Option<SimulatorRuntimeConfig>,
}

/// Simulator runtime config. Same shape as SimulatorProvidedConfig
/// (may diverge later if assembly adds derived fields).
#[derive(Debug, Clone)]
pub struct SimulatorRuntimeConfig {
    pub mock_balances: String,
    pub fill_delay_ms: u64,
    pub match_policy: String,
    pub admin_grpc_port: u16,
    pub enable_admin_controls: bool,
}
```

### Wrapper: BootstrapOutcome (existing, from zk-infra-rs)

```rust
// Already exists in zk_infra_rs::bootstrap
pub struct BootstrapOutcome<R: std::fmt::Debug> {
    pub runtime_config: R,
    pub source: ConfigSource,
    pub metadata: ConfigMetadata,
    pub secret_refs: Vec<SecretRef>,
}
```

Main.rs stores `BootstrapOutcome<GwRuntimeConfig>`. Downstream code receives `&outcome.runtime_config`. Metadata stays at the top level for future introspection.

## ServiceBootstrap Implementation

```rust
pub struct GwBootstrap;

impl ServiceBootstrap for GwBootstrap {
    type BootstrapConfig = GwBootstrapConfig;
    type ProvidedConfig = GwProvidedConfig;
    type RuntimeConfig = GwRuntimeConfig;

    fn decode_pilot_config(
        payload: &PilotPayload,
    ) -> Result<GwProvidedConfig, BootstrapConfigError> { ... }

    fn load_direct_config() -> Result<GwProvidedConfig, BootstrapConfigError> { ... }

    fn assemble_runtime_config(
        bootstrap: &GwBootstrapConfig,
        provided: GwProvidedConfig,
    ) -> Result<GwRuntimeConfig, BootstrapConfigError> { ... }
}
```

### decode_pilot_config

1. `serde_json::from_str::<GwProvidedConfig>(&payload.runtime_config_json)?`
2. Return the deserialized config (serde handles Option<SimulatorProvidedConfig> naturally).
3. Invalid JSON → `BootstrapConfigError::InvalidJson`.
4. Missing required fields (venue, account_id) → serde error → `InvalidJson`.

### load_direct_config

1. Read `ZK_VENUE`, `ZK_ACCOUNT_ID`, `ZK_VENUE_CONFIG`, `ZK_VENUE_ROOT`, `ZK_EXEC_SHARD_COUNT`, `ZK_EXEC_QUEUE_CAPACITY` from env.
2. If venue == "simulator": populate `simulator: Some(SimulatorProvidedConfig { ... })` from `ZK_MOCK_BALANCES`, `ZK_FILL_DELAY_MS`, `ZK_MATCH_POLICY`, `ZK_ADMIN_GRPC_PORT`, `ZK_ENABLE_ADMIN_CONTROLS`.
3. If venue != "simulator": `simulator: None`.
4. Env parse failures → `BootstrapConfigError::Validation`.

### assemble_runtime_config

1. **Validate simulator consistency:**
   - `venue == "simulator"` && `simulator.is_none()` → `MissingField { field: "simulator" }`
   - `venue != "simulator"` && `simulator.is_some()` → `Validation("simulator config present on non-simulator venue")`
2. **Validate exec pool:**
   - `exec_shard_count < 1` → `Validation("exec_shard_count must be >= 1")`
   - `exec_queue_capacity < 1` → `Validation("exec_queue_capacity must be >= 1")`
3. **Merge:** Copy bootstrap fields + provided fields into `GwRuntimeConfig`.
4. **Map simulator:** `provided.simulator.map(|s| SimulatorRuntimeConfig { ... })`.

## Startup Flow

### Before (current)

```
GwSvcConfig::from_env()
→ cfg used by everything
→ if token: register_with_pilot() → payload discarded
→ if !token: register_direct()
```

### After

```
1. GwBootstrapConfig::from_env()
2. NATS connect (bootstrap.nats_url)
3. Determine mode:
   a. Direct (bootstrap_token empty):
      - register_direct(...)
      - mode = BootstrapMode::Direct
   b. Pilot (bootstrap_token set):
      - register_with_pilot(...) → pilot_reg
      - mode = BootstrapMode::Pilot { payload: pilot_reg.payload, validate_hash: false }
4. outcome = bootstrap_runtime_config::<GwBootstrap>(&bootstrap, mode)?
     → internally calls decode_pilot_config or load_direct_config, then assemble_runtime_config
5. let runtime_cfg = &outcome.runtime_config;
6. All downstream code uses runtime_cfg
```

### Registration ordering note

Current code does NATS KV registration *before* config assembly. In Pilot mode, `register_with_pilot` returns both the KV grant AND the payload. The payload feeds into `bootstrap_runtime_config`. Registration and config decoding happen in sequence:

```
register_with_pilot() → PilotRegistration { registration, payload }
bootstrap_runtime_config(bootstrap, Pilot { payload }) → BootstrapOutcome
```

In direct mode, `register_direct()` is independent of config loading — both can happen in either order. We keep registration first for consistency.

## Startup Reordering

**Important sequencing change:** Current code calls `build_adapter` (line 49) *before* NATS KV registration (line 152). The new flow puts NATS connect → registration → `bootstrap_runtime_config` *before* `build_adapter`. This is required because in Pilot mode, the adapter factory needs the runtime config assembled from the Pilot payload. In direct mode this reordering is harmless since both paths read env vars independently.

New order: NATS connect → register → bootstrap config → build adapter → exec pool → gRPC server.

## Downstream Migration

All ~50 `cfg.xxx` access points in main.rs, venue/mod.rs, etc. change to `runtime_cfg.xxx`. Field names are identical. Simulator fields change from `cfg.mock_balances` to `runtime_cfg.simulator.as_ref().unwrap().mock_balances` — but in practice, simulator fields are only accessed inside venue=="simulator" branches where `unwrap()` is safe (validated at assembly).

### Adapter factory signature change

```rust
// Before
pub async fn build_adapter(cfg: &GwSvcConfig) -> Result<BuiltVenue, ...>

// After
pub async fn build_adapter(cfg: &GwRuntimeConfig) -> Result<BuiltVenue, ...>
```

### parse_balances utility

`GwSvcConfig::parse_balances()` is called in `venue/mod.rs` to parse `mock_balances` into `HashMap<String, f64>`. With `GwSvcConfig` removed, this becomes a free function `parse_balances(s: &str) -> HashMap<String, f64>` in `config.rs` (or on `SimulatorRuntimeConfig`).

All other downstream consumers already receive individual fields (not the config struct), so they need no signature changes — only the extraction site in main.rs changes.

## Config Introspection

No new RPC endpoint in this phase. `BootstrapOutcome` at the top level provides:
- `outcome.source` — `ConfigSource::Direct` or `ConfigSource::Pilot { version, hash, issued_at }`
- `outcome.metadata` — `ConfigMetadata` with version/hash/timestamps
- `outcome.secret_refs` — `Vec<SecretRef>` for future Vault resolution

Future work: wrap `GwRuntimeConfig` in `ConfigEnvelope` and expose via `GetCurrentConfig` RPC.

## Validation Summary

| Check | Where | Error |
|-------|-------|-------|
| Invalid Pilot JSON | `decode_pilot_config` | `InvalidJson` |
| Missing venue/account_id in Pilot JSON | `decode_pilot_config` (serde) | `InvalidJson` |
| venue=simulator but no simulator config | `assemble_runtime_config` | `MissingField` |
| venue≠simulator but simulator config present | `assemble_runtime_config` | `Validation` |
| exec_shard_count < 1 | `assemble_runtime_config` | `Validation` |
| exec_queue_capacity < 1 | `assemble_runtime_config` | `Validation` |
| Config hash mismatch (when validate_hash=true) | `bootstrap_runtime_config` (shared) | `HashMismatch` |

## File Changes

| File | Change |
|------|--------|
| `zk-gw-svc/src/config.rs` | Replace `GwSvcConfig` with `GwBootstrapConfig`, `GwProvidedConfig`, `SimulatorProvidedConfig`, `GwRuntimeConfig`, `SimulatorRuntimeConfig`, `GwBootstrap` impl |
| `zk-gw-svc/src/main.rs` | New bootstrap flow; `cfg` → `runtime_cfg`; metadata in `outcome` |
| `zk-gw-svc/src/venue/mod.rs` | `build_adapter` takes `&GwRuntimeConfig` |
| `zk-gw-svc/tests/` | New `bootstrap_config.rs` test module |

## Tests

1. **direct_mode_builds_runtime_from_env** — Set env vars, call `load_direct_config` + `assemble_runtime_config`, verify all fields populated correctly.
2. **pilot_mode_builds_runtime_from_payload** — Construct `PilotPayload` with valid JSON, call `decode_pilot_config` + `assemble_runtime_config`, verify all fields.
3. **malformed_pilot_json_fails** — Garbage JSON in payload → `InvalidJson`.
4. **missing_required_fields_fails** — JSON missing `venue` or `account_id` → error.
5. **hash_mismatch_fails** — Use `bootstrap_runtime_config` with `validate_hash: true` and wrong hash → `HashMismatch`.
6. **real_venue_uses_venue_config** — JSON with venue="okx" + `venue_config` blob → verify `venue_config` preserved in runtime config, `simulator` is None.
7. **simulator_payload_accepts_simulator_fields** — JSON with venue="simulator" + simulator object → verify simulator fields parsed.
8. **non_simulator_rejects_simulator_config** — JSON with venue="okx" + simulator object → `Validation` error.
9. **simulator_venue_missing_simulator_config_fails** — JSON with venue="simulator" but no `simulator` object → `MissingField { field: "simulator" }`.
