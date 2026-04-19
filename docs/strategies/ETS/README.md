# ETS Strategy Onboarding Plan

## Scope

This document proposes the onboarding plan for the research strategy:

- strategy: `zkstrategy_research/zk-strategylib/zk_strategylib/entry_target_stop/ETS_on_kline.py`
- pricer: `zkstrategy_research/zk-strategylib/zk_strategylib/entry_target_stop/pricers/rsi_simple_pricer_v2.py`
- target venue: Oanda

The immediate goal is a testable onboarding path for Oanda using the existing Python strategy runtime, while aligning the config model with zkbot's canonical refdata and instrument conventions.

## Problem Statement

The current ETS strategy config is too transport-shaped. It requires the caller to provide multiple symbol references:

- `symbol`
- `symbol_exch`
- `position_symbol`

That duplicates information which should be derivable from canonical refdata.

For Oanda specifically, this is fragile because:

- canonical tradable instrument identity is the zkbot `instrument_id`
- RTMD uses venue-native `instrument_exch`
- OMS positions for Oanda are CFD positions and should not force the strategy author to guess a separate `position_symbol`

The current onboarding also lacks:

- a module-level `__zk_init__` hook to preload kline history from Oanda RTMD
- a thin startup path for Pilot-assisted local testing

## Existing Ground Truth

### Strategy runtime

The existing Python strategy runtime already supports startup init hooks.

- today the legacy runtime still calls `__tq_init__` before `on_reinit()`
- the returned payload is exposed through `tq.get_custom_init_data()`
- `ETS_on_kline.py` already consumes that init payload and feeds it into the pricer warmup path

For zk naming consistency, the target Python-side hook name should be `__zk_init__`.

That means the missing work is twofold:

- add the strategy-specific init producer
- rename the Python lifecycle hook from `__tq_init__` to `__zk_init__` in the runtime and strategy-loading path

### Refdata model

The current canonical refdata model already exposes the fields needed for symbol resolution:

- `instrument_id`
- `venue`
- `instrument_exch`
- `instrument_type`
- `base_asset`
- `quote_asset`
- `settlement_asset`

The refdata service supports both lookup directions:

- by canonical `instrument_id`
- by `(venue, instrument_exch)`

This is consistent with the documented instrument convention:

- strategy and OMS-facing code should use canonical `instrument_id`
- venue-native symbol should be resolved from refdata for RTMD and gateway transport

### Oanda specifics

The Oanda refdata loader already normalizes Oanda instruments into canonical CFD-style instrument ids.

Example:

- canonical: `EUR-CFD/USD@OANDA`
- venue-native: `EUR_USD`

The Oanda RTMD adaptor already supports:

- live tick subscriptions
- live kline subscriptions
- historical `query_klines(...)`

The Oanda market model is FX/CFD:

- no true orderbook semantics
- kline-driven logic is appropriate
- positions are CFD positions

### Rust wrapping Python strategy code

For the longer-term Rust engine path, Python strategy loading should explicitly support two packaging modes:

- local development source trees
- packaged artifacts from a private PyPI repository

This matters because the Rust wrapper path should not assume Python strategies only live inside the main repo checkout.

## Design Goal

The ETS config should accept one canonical symbol only, then resolve the rest from refdata.

The strategy author should not need to manually pass:

- venue-native RTMD symbol
- OMS position symbol
- duplicated market identity fields

## Proposed Config Model

### Current shape

Today the relevant symbol-related config is roughly:

```json
{
  "account_id": 8001,
  "symbol": "EUR-CFD/USD@OANDA",
  "symbol_exch": "EUR_USD",
  "position_symbol": "EUR_USD",
  "pricer_cls": "...",
  "pricer_config_cls": "...",
  "pricer_config_data": {}
}
```

### Target shape

Proposed simplified config:

```json
{
  "account_id": 8001,
  "symbol": "EUR-CFD/USD@OANDA",
  "pricer_cls": "...RSISimplePricer",
  "pricer_config_cls": "...RSISimplePricerConfig",
  "pricer_config_data": {},
  "init_kline_interval": "1m",
  "init_lookback_bars": 10080,
  "max_holding_time_in_secs": 1800
}
```

Optional explicit overrides may still exist temporarily for debugging:

- `market`
- `init_venue`
- `init_symbol_exch_override`

But the default contract should be one canonical `symbol`.

## Resolution Model

Given `symbol = instrument_id`, runtime resolution should derive the following:

- `instrument_id`
  - used as the canonical strategy-facing market identity
- `venue`
  - derived from refdata or parsed from `instrument_id`
- `instrument_exch`
  - used for RTMD subscriptions and RTMD history query
- `instrument_type`
  - used to interpret position semantics
- `market`
  - optional market-session key such as `FX` or `CFD`

### Position identity

For ETS on Oanda, the strategy should not require a standalone `position_symbol`.

Target rule:

- for non-spot instruments such as CFDs, the canonical `instrument_id` should be the strategy's position key
- if the current legacy balance API still exposes positions keyed by venue-native codes, the adapter layer should translate that detail before the strategy uses it

Transitional rule:

- keep a private derived field inside the runtime if needed
- do not keep `position_symbol` in the public strategy config unless a concrete unresolved OMS limitation requires it

### RTMD identity

RTMD should subscribe/query using `instrument_exch`, but that must be derived from refdata from the canonical `symbol`.

For Oanda:

- config input: `EUR-CFD/USD@OANDA`
- resolved RTMD symbol: `EUR_USD`

## Proposed Strategy Changes

### 1. Simplify config parsing in `ETS_on_kline.py`

Replace the public config contract so that:

- `symbol` is required
- `symbol_exch` becomes derived runtime state
- `position_symbol` becomes derived runtime state or is eliminated

Implementation expectation:

- normalize config in `on_reinit()`
- resolve canonical instrument metadata before the state machine starts
- store resolved runtime fields separately from external config

Suggested internal split:

- `ETSExternalConfig`
  - user-facing config payload
- `ResolvedInstrumentBinding`
  - canonical symbol, venue, instrument_exch, instrument_type, market

### 2. Add `__zk_init__` for kline warmup

Add a module-level init hook:

```python
async def __zk_init__(config: dict, dt: datetime, tq_client):
    ...
```

Responsibilities:

- resolve `instrument_exch` for the configured canonical symbol
- query Oanda RTMD historical bars using `query_klines(...)`
- return `list[rtmd.Kline]`

That payload is then consumed by the existing:

- `tq.get_custom_init_data()`
- `self.pricer.on_bar(k)`

### 3. Rename the Python init hook from `__tq_init__` to `__zk_init__`

The Python-side lifecycle hook should follow zk naming rather than legacy tq naming.

Target direction:

- strategy modules export `__zk_init__`
- Python strategy loading/runtime looks for `__zk_init__`
- legacy `__tq_init__` support may remain temporarily as a compatibility alias during migration

Compatibility recommendation:

- prefer `__zk_init__` in new and migrated strategy code
- keep a short transition window where the runtime accepts both names
- log a deprecation warning when `__tq_init__` is used

### 4. Support both Python packaging modes in the Rust wrapper path

When the Rust engine eventually wraps Python strategy code, it should support both:

- local dev loading from folders/packages in a workspace checkout
- installed package loading from a private package index

These are different operational modes and should be modeled directly rather than forced through one fragile path.

#### Mode A: Local development source tree

Primary use case:

- local iteration in a workspace such as `zkstrategy_research/`
- editable code without building and publishing a package for every change

Recommended contract:

- strategy config identifies `python_module` and `python_class`
- the package is installed into the workspace venv via `[tool.uv.sources]`
  (editable path install from `zkstrategy_research/zk-strategylib`)
- the engine embeds that same venv via `VIRTUAL_ENV` / `PYO3_PYTHON` and
  resolves the module with `importlib.import_module`

Recommended examples:

- `python_module = "zk_strategylib.entry_target_stop.ETS_on_kline"`
- `python_class = "ETS"`
- ensure `uv sync` has pulled `zk-strategylib` into `.venv/…/site-packages/`

Rules:

- local dev and prod both use installed-package imports; no `sys.path` overrides
- the wrapper must fail clearly if the module or requested class is missing
- see [../../system-arch/dependency-contract.md](../../system-arch/dependency-contract.md)
  for the full Rust-Python resolution contract

#### Mode B: Private PyPI package

Primary use case:

- controlled deployment of versioned strategy artifacts
- reproducible production or shared test environments

Recommended contract:

- the strategy is published as a versioned Python package to a private package index
- the runtime image or host environment installs that package ahead of startup
- runtime config still refers to `python_module` and `python_class`, not to wheel filenames

Recommended examples:

- package name: `zk-strategylib`
- installed version: `0.3.7`
- module path: `zk_strategylib.entry_target_stop.ETS_on_kline`
- class: `ETS`

Rules:

- the Rust wrapper should not contain ad hoc pip-install logic on the hot startup path
- package installation should be handled by image build, virtualenv preparation, or deployment automation
- private index credentials should stay in deployment/bootstrap secret handling, not in strategy config

#### Recommendation

Treat import resolution and package installation as separate responsibilities.

- the Rust wrapper should own Python import resolution
- deployment tooling should own package installation

That split gives a clean model:

- local dev: workspace venv has `zk-strategylib` path-installed via uv sources;
  engine embeds that venv and imports by module name
- deployed env: install package from private PyPI into the deploy venv, import
  package module without special path overrides

#### Config shape for future Python-wrapper support

The future Python-wrapper strategy config should be able to express both modes cleanly.

Suggested fields:

- `python_module`
- `python_class`
- `python_strategy_config`

Non-goals for the strategy config:

- direct wheel file paths
- pip command lines
- private index credentials
- arbitrary shell snippets

### 3. Keep ETS bar-driven on Oanda

ETS should be treated as a kline-driven strategy for Oanda.

Do not require:

- orderbook-driven entry logic
- orderbook-driven close logic

Kline-driven state transitions are already present and are the correct default for Oanda.

## Refdata Resolution Options

There are two practical ways to resolve canonical symbol to venue-native symbol.

### Option A: Resolve in strategy init using a refdata client

Pros:

- strategy owns its own bindings
- no extra orchestration preprocessing needed

Cons:

- introduces a runtime dependency from Python strategy init to refdata lookup
- duplicates binding logic across strategies if repeated elsewhere

### Option B: Resolve before strategy start and inject resolved binding

Pros:

- cleaner runtime contract
- aligns well with Pilot/control-plane enrichment

Cons:

- requires the launcher or engine host to own symbol binding logic

### Recommendation

Use a staged approach:

- short term: resolve inside strategy init or its launcher so onboarding can proceed quickly
- medium term: move symbol binding enrichment into the engine/Pilot startup path

## Pilot-Assisted Start Path

The immediate goal is not full new-architecture engine adoption. It is a practical local startup flow.

Recommended interpretation of "Pilot-assisted":

- Pilot allocates or records execution metadata
- Pilot or a small wrapper resolves environment-specific config
- a local launcher starts the Python strategy runtime with those resolved inputs

This should be implemented as a thin script, not as a large new orchestration subsystem.

### Proposed script responsibilities

- accept a canonical `symbol`
- resolve instrument binding from refdata
- assemble ETS config
- request or record execution metadata from Pilot where applicable
- launch the strategy runtime against Oanda GW/RTMD/OMS

Suggested output:

- execution id if available
- resolved canonical/venue symbol mapping
- effective config JSON
- log file path or process pid

## Implementation Phases

### Phase 1: Strategy config simplification

- introduce a simplified public config model using canonical `symbol`
- remove public dependence on `symbol_exch` and `position_symbol`
- keep temporary internal compatibility shims if needed

### Phase 2: Init hook rename and warmup

- add `__zk_init__` to `ETS_on_kline.py`
- update the Python runtime to recognize `__zk_init__`
- keep temporary compatibility for `__tq_init__` if needed
- fetch Oanda historical klines using RTMD query
- warm up `RSISimplePricer`

### Phase 3: Python wrapper packaging contract

- define the Python-wrapper import contract (installed-module imports only,
  per [../../system-arch/dependency-contract.md](../../system-arch/dependency-contract.md))
- keep package installation outside the runtime import path

### Phase 4: Launcher

- create a Pilot-assisted local launch script
- include refdata resolution and effective config generation
- make Oanda account/environment selection explicit

### Phase 5: Validation

- verify warmup fetch count is sufficient for minute RSI and derived hourly RSI
- verify live Oanda kline subscription matches the init interval
- verify position detection works without `position_symbol` in public config
- verify first entry signal does not occur before warmup is complete
- verify Python packaging: installed-package import resolves `zk_strategylib`
  from the workspace venv (uv-path-install) and from a private-index install

## Risks And Gaps

### 1. Legacy Python runtime path

The current immediate path is the legacy Python strategy runtime, not the new Rust engine runtime.

Reason:

- the new engine manifest mentions `python-wrapper`
- but the current strategy host parser still only accepts the known Rust-native strategy keys

This should be treated as a separate platform gap, not hidden inside ETS onboarding.

### 2. Python hook rename compatibility

Renaming `__tq_init__` to `__zk_init__` is the right direction, but it is a runtime contract change, not just a strategy-file rename.

Affected areas include:

- Python strategy loader/runtime
- any research strategies still exporting `__tq_init__`
- parity notes and Rust comments that still mirror the legacy Python lifecycle naming

### 3. Python packaging ambiguity in the future Rust wrapper path

Without an explicit contract, the Python-wrapper path can drift into an unclear mix of:

- local file loading
- package import loading
- runtime pip installation

That should be avoided. The wrapper path should support:

- local package-root search path overrides for development
- normal package imports for deployed environments

and should avoid runtime-managed package installation.

### 4. Kline subscription bug in legacy client

The legacy Python `tqclient.subscribe_rtmd_kline()` path appears fragile and should be verified or fixed before relying on it for ETS live operation.

### 5. Position API shape

If the strategy currently reads holdings from a balance/position API keyed by a non-canonical symbol, there may be a temporary translation step needed until the runtime consistently exposes canonical instrument identity to strategies.

### 6. Oanda market session handling

For production-quality gating, ETS should eventually consult market session state from refdata for Oanda `FX` / `CFD` markets instead of assuming the venue is always tradable.

## Acceptance Criteria

This plan is considered complete when the following are true:

- ETS public config accepts one canonical `symbol`
- Oanda venue-native symbol is derived from refdata, not manually configured
- no public `position_symbol` is required for ETS config
- Python strategy startup uses `__zk_init__` as the primary init hook name
- startup init fetches historical klines from Oanda RTMD query
- the pricer is warmed before live signal generation
- the future Rust Python-wrapper path has a documented contract for:
  - local package-root imports during development
  - installed package imports from private PyPI in deployed environments
- a Pilot-assisted launcher can start the strategy with the simplified config

## Recommended Next Step

Implement the plan in this order:

1. simplify ETS config around canonical `symbol`
2. rename the Python init hook to `__zk_init__` and add the kline warmup hook
3. define the Python-wrapper packaging/import contract for local-dev and private-package modes
4. add the Pilot-assisted launcher
5. fix any legacy kline subscription/runtime issues discovered during validation

This keeps the onboarding scoped, testable, and aligned with the existing refdata model without waiting for the full Python-wrapper engine path to be completed.
