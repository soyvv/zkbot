# Bot UI Interaction Design

## Purpose

This document defines the control-plane and operator UX model for the `bot` domain across:

- Pilot backend
- Pilot UI
- engine runtime lifecycle
- strategy run correlation

It refines the earlier bot sections in
[Pilot UI Interaction](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/pilot_ui_interaction.md)
and makes the bot lifecycle explicit as three separate concepts:

- `strategy_definition`
- `engine_instance` (operator-facing bot)
- `strategy_instance` (one run snapshot with `execution_id`)

## Core Model

### 1. Strategy Definition

`strategy_definition` is the reusable strategy artifact and strategy-only config.

It owns:

- strategy identity such as `strategy_id`
- semantic `strategy_key` used for runtime strategy selection, for example `MM_OKX_BTC_PERP_V1`
- runtime type and code reference
- strategy parameter schema and config payload
- strategy metadata such as description, tags, and enablement

It does not own:

- engine/common runtime settings
- OMS or account topology bindings
- per-bot operational metadata
- per-run state

Design rule:

- strategy config must describe only the strategy itself
- engine/process/runtime wiring must not be stored as strategy config

### 2. Engine Instance

`engine_instance` is the operator-managed bot definition.

This is the onboarding object created from the Bot panel.

It owns:

- stable bot identity such as `engine_id`
- bot description and enablement
- common engine fields
- target OMS binding
- deployment or runtime profile
- desired provided config for the engine runtime
- selected `strategy_definition`

Design rule:

- one bot selects one strategy definition as its strategy template
- many bots may reference the same strategy definition
- bot config is the place for engine/common fields and strategy selection
- account, gateway, and symbol scope do not belong on `engine_instance`
- `engine_instance` binds the bot to one OMS workspace and provides the engine runtime envelope

### 3. Strategy Instance

`strategy_instance` is one concrete bot run snapshot.

It owns:

- `execution_id`
- `engine_id`
- `strategy_id`
- lifecycle state
- effective config snapshot at start time
- start and end timestamps
- error/termination metadata

Design rule:

- every bot start creates a new `strategy_instance`
- `strategy_instance` is immutable as a run snapshot except for lifecycle progression
- later events must correlate to both:
  - `strategy_instance` via `execution_id`
  - `strategy_definition` via `strategy_id`

## Lifecycle Ownership

The bot lifecycle is split into three operator phases.

### Phase 1: Strategy Authoring

Operators create or edit a strategy definition without touching engine runtime wiring.

Expected actions:

- create strategy definition
- edit strategy parameters and metadata
- validate strategy schema/config
- enable or disable strategy definition for selection

### Phase 2: Bot Onboarding

Operators create a bot as an `engine_instance`.

Expected fields:

- `engine_id`
- display name / description
- selected `strategy_id`
- target `oms_id`
- engine runtime profile
- engine-level provided config
- operational policy fields such as restart mode or environment tags

Expected validation:

- selected strategy exists and is enabled
- OMS target exists
- required topology dependencies are configured
- engine config validates against the engine schema
- the selected strategy's logical scope is feasible inside the chosen OMS workspace

### Phase 3: Bot Run

Starting a bot creates one `strategy_instance`.

Lifecycle ownership rule:

- bot start and stop are owned by the Pilot runtime orchestrator
- the engine runtime still performs normal bootstrap, claim binding, and self-registration after it
  is launched
- Pilot should not treat engine self-bootstrap as the owner of start/stop lifecycle intent

Environment model:

- local development uses the existing local process orchestrator
- production should use a Kubernetes-backed runtime orchestrator
- both backends must preserve the same bot lifecycle contract and `execution_id` ownership model

Lifecycle:

1. operator starts bot `engine_id`
2. Pilot creates `strategy_instance` with a fresh `execution_id`
3. Pilot asks the runtime orchestrator to start the bot runtime
4. Pilot snapshots:
   - selected `strategy_definition`
   - selected `engine_instance`
   - effective merged runtime config
5. launched engine runtime bootstraps against that pre-created run
6. engine validates that the strategy's logical account, gateway, and symbol scope can be realized
   inside the selected OMS workspace
7. engine resolves the physical runtime scope through OMS queries and RTMD dynamic subscriptions
8. runtime registers in discovery under stable bot identity
9. runtime reports current run via `execution_id`

Design rules:

- discovery identity remains stable at the bot or engine level
- run identity remains `execution_id`
- the current run can change while the bot identity stays the same

## Config Model

The bot domain uses three config layers.

### Strategy Config

Stored on `strategy_definition`.

Examples:

- quoting widths
- skew parameters
- rebalance thresholds
- model feature flags
- strategy timer thresholds

These are generic strategy parameters, not direct engine bindings.

Design rule:

- generic strategy config should not carry account or symbol selections
- `strategy_key` is the semantic selector used by runtime dispatch and operator workflows
- if strategy-specific scope is needed later, it should be modeled explicitly rather than mixed into
  generic strategy parameter config

### Engine/Bot Config

Stored on `engine_instance`.

Examples:

- OMS binding
- runtime mode
- supervision mode
- restart policy
- engine log level

Design rule:

- `engine_instance` provides the execution workspace by binding to an OMS
- it should not duplicate account, gateway, or symbol scope already declared by the strategy

### Run Snapshot

Stored on `strategy_instance`.

This is the immutable effective config used by that execution.

Recommended snapshot sections:

- `strategy_config_snapshot`
- `engine_config_snapshot`
- `effective_runtime_config`
- `binding_snapshot`

Design rule:

- the run snapshot must survive later edits to both the strategy definition and the bot definition
- `binding_snapshot` should capture the resolved physical scope derived at start time from:
  - strategy identity and runtime selection metadata
  - OMS workspace binding
  - runtime feasibility checks

## Event Correlation Model

All strategy-originated or engine-originated events should be attributable at three levels:

- bot identity: `engine_id`
- strategy template identity: `strategy_id`
- concrete run identity: `execution_id`

Recommended event envelope fields:

- `engine_id`
- `strategy_id`
- `execution_id`
- `event_type`
- `event_ts`
- `source_component`

Correlation rules:

- UI run views primarily query by `execution_id`
- UI bot views aggregate by `engine_id`
- strategy history views aggregate by `strategy_id`
- recorder and audit paths should persist all three ids when available

Recommended runtime provenance fields:

- requested logical scope from strategy config
- resolved account set
- resolved gateway set when applicable
- resolved symbol or instrument subscription set

## Bot Panel Design

### List View

The Bot management list should show one row per `engine_instance`.

Suggested columns:

- bot name / `engine_id`
- selected strategy
- bot status
- OMS target
- strategy scope summary
- current run or last active run
- last start time
- last stop reason
- drift / readiness state

The row must not treat the strategy definition itself as the bot row.

### Detail View

The Bot detail page should be bot-first, with strategy and run shown as related sections.

Summary cards:

- Bot summary
- Selected strategy summary
- Current run or last active run
- Runtime health / topology state
- OMS workspace and resolved runtime scope

Tabs:

- `Run`
- `Config`
- `Activities`
- `Lifecycle`
- `Logs`
- `History`

### Current Run / Last Active Run

The page should always show either:

- the currently running `strategy_instance`, or
- the most recent non-draft `strategy_instance`

This section should include:

- `execution_id`
- lifecycle state
- start time
- end time if stopped
- selected strategy version/snapshot reference
- last operator action
- recent activity counters

Design rule:

- the bot detail view anchors on bot identity, not strategy definition identity
- run detail is contextual to the bot

## Onboarding UX

The bot workflow should be a two-step control-plane authoring flow.

### Flow A: Strategy Library

Purpose:

- create reusable strategy definitions

Primary actions:

- `Create Strategy`
- `Edit Strategy`
- `Validate Strategy`
- `Disable Strategy`

### Flow B: Bot Onboarding

Purpose:

- create a runnable bot from engine/common fields plus one strategy selection

Primary steps:

1. open `Bots`
2. click `Create Bot`
3. fill bot identity and common engine fields
4. select one strategy definition
5. choose OMS workspace
6. validate readiness
7. save bot definition

Recommended API direction:

- `POST /v1/bot/definitions`
- `PUT /v1/bot/definitions/{engine_id}`
- `GET /v1/bot/definitions`
- `GET /v1/bot/definitions/{engine_id}`

Start/stop actions should target the bot:

- `POST /v1/bot/definitions/{engine_id}/start`
- `POST /v1/bot/definitions/{engine_id}/stop`
- `POST /v1/bot/executions/{execution_id}/stop`
- `POST /v1/bot/executions/{execution_id}/pause`
- `POST /v1/bot/executions/{execution_id}/resume`

Orchestration rule:

- `start` and bot-level `stop` are Pilot orchestrator actions
- the current implementation may use local process management
- the production implementation should use Kubernetes workload orchestration
- execution-scoped stop or pause/resume may still call engine control APIs after the runtime exists

## Readiness Model

Suggested bot readiness states:

- `draft`
- `configured`
- `validated`
- `startable`
- `starting`
- `running`
- `degraded`
- `stopped`

`startable` should mean:

- the target OMS is reachable
- the selected strategy's logical scope can be satisfied inside that OMS workspace
- required RTMD subscriptions can be established dynamically

Suggested strategy-definition readiness states:

- `draft`
- `configured`
- `validated`
- `enabled`
- `disabled`

Suggested run states for `strategy_instance`:

- `INITIALIZING`
- `RUNNING`
- `PAUSED`
- `STOPPED`
- `FAILED`
- `CRASHED`

## Data Model Direction

Recommended relational model:

- `cfg.strategy_definition`
  - reusable strategy template and strategy-only config
- `cfg.engine_instance`
  - bot definition, OMS binding, and engine/common desired config
- `cfg.strategy_instance`
  - run snapshot with `execution_id`, `engine_id`, `strategy_id`

Recommended schema changes:

- add `engine_id` foreign key to `cfg.strategy_instance`
- keep `strategy_id` foreign key to `cfg.strategy_definition`
- store run-time config snapshots on `cfg.strategy_instance`
- keep `cfg.engine_instance.provided_config` as the bot/engine desired config authority
- do not duplicate strategy account, gateway, or symbol scope onto `cfg.engine_instance`

## Gaps

### Pilot Backend Gaps

- current `/v1/bot` API is strategy-first; it does not expose a first-class bot or `engine_instance` authoring resource
- `BotService` starts from `strategyKey` instead of a stable bot identity
- `StrategyRepository` and `MetaRepository` join only `strategy_definition` to running `strategy_instance`, ignoring `cfg.engine_instance`
- strategy create/update requests need to clearly own logical account, gateway, and symbol scope
- execution creation does not snapshot both bot config and strategy config separately
- current start flow resolves OMS from strategy-oriented data, not from a dedicated bot definition that binds only to OMS
- validation is strategy-centric and does not validate a full bot onboarding object
- Pilot lacks a readiness check that answers whether a strategy's logical scope is feasible in the chosen OMS workspace
- orchestrator ownership is not yet explicit enough in the bot API and lifecycle model
- local process orchestration exists, but the production-target Kubernetes orchestration contract is
  not yet made first-class in the bot workflow

### Engine Runtime Gaps

- engine bootstrap and runtime docs are execution-aware, but the runtime contract does not clearly carry both `engine_id` and `strategy_id` as first-class fields everywhere
- discovery registration is keyed by stable logical identity, but the control-plane side needs a canonical rule that this identity is the bot or engine instance, not the strategy definition row
- effective runtime config loading needs an explicit merged model:
  - strategy config from `strategy_definition`
  - engine config from `engine_instance`
  - run snapshot metadata from `strategy_instance`
- engine needs an explicit resolution phase that maps:
  - strategy logical scope
  - OMS workspace binding
  into resolved physical account, gateway, and RTMD subscription scope
- engine lifecycle reporting should expose enough data for Pilot to answer "what is the current run for this bot?" without reconstructing from strategy-only records

### Recorder / Event Gaps

- strategy events today are mostly correlated by `strategy_id` and `execution_id`; the bot identity should also be persisted as `engine_id`
- event schemas and recorder upserts need a consistent source-binding model that can resolve bot, strategy template, and run snapshot together
- historical queries for bot detail need a stable way to fetch the last active run per `engine_id`

### Pilot UI Gaps

- current `BotsPage` is mock-only and presents the bot row as a strategy-like object rather than as a distinct bot definition
- the page does not separate selected strategy config from engine/common bot config
- the detail view should anchor on bot definition and then show current or last active run as a child context
- onboarding UX is still described as "create strategy, then start execution" rather than "create strategy library item, then onboard bot, then start run"

## Recommended Implementation Order

1. make `engine_instance` the control-plane bot definition authority
2. add `engine_id` to `strategy_instance` and treat each row as a run snapshot
3. update Pilot bot APIs to be bot-first for onboarding and start actions
4. update engine bootstrap payloads and status/reporting to carry `engine_id`, `strategy_id`, and `execution_id`
5. update recorder/event correlation to persist all three ids
6. update Bot UI to show bot definition plus current or last active run

## Relationship To Existing Docs

This document narrows the bot-specific interaction model.

The following remain canonical for their broader scopes:

- [Pilot Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/pilot_service.md)
- [Engine Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/engine_service.md)
- [Pilot UI Interaction](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/pilot_ui_interaction.md)
- [Data Layer](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/data_layer.md)
