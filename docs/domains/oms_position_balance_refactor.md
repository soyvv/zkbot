# OMS Position / Balance Refactor

## Purpose

This document defines the target refactor for OMS core and OMS gateway integration so that `Position` and `Balance` become separate concepts with clear ownership.

The immediate goal is to remove the current conflation where OMS internal bookkeeping, exchange-reported asset inventory, and position-like exposure all flow through one manager and one internal record shape.

## Scope

This refactor covers:

- `zk-oms-rs` internal state model
- OMS writer/read-replica semantics
- OMS gRPC query semantics
- OMS NATS publish semantics
- OMS gateway integration semantics

This refactor does not require immediate changes to:

- strategy/runtime APIs outside the already-separated `Balance` and `Position` protos
- risk model redesign beyond local reservation handling
- exchange gateway proto redesign in the first step, though the target model assumes later gateway message separation

## Decision Summary

### 1. `Position` and `Balance` are separate domain concepts

`Position` means instrument exposure.

Examples:

- perp / futures / options positions
- CFD positions
- stock / ETF holdings
- margined spot instrument exposure when represented as an instrument

`Balance` means asset inventory or cash ledger state.

Examples:

- USD
- USDT
- BTC
- JPY
- SGD
- collateral asset balances

### 2. OMS keeps positions locally, but does not own canonical balances

General rule:

- the system should keep position quantity as accurate as possible locally
- balance should be treated as exchange-owned state

Rationale:

- local position quantity is critical for hedging, risk alignment, and order sizing
- local cash balance is vulnerable to drift if OMS tries to fully own it
- exchanges are the canonical source for margin, PnL, available cash, and collateral state

### 3. Position quantity is OMS-owned; margin and PnL are exchange-owned

For positions:

- OMS should own executable quantity state
- exchange should own margin, liquidation, PnL, leverage, and other economics-related properties

For balances:

- exchange push/query is the canonical source
- OMS may cache balances, but should not synthesize canonical balance values from local order bookkeeping

### 3a. OMS-owned position quantity requires automatic reconciliation

If OMS owns executable position quantity, OMS must also implement automatic reconciliation against exchange-reported position state.

Rationale:

- local position state is required for fast execution, hedging, and risk alignment
- local state will drift over time due to late reports, dropped messages, manual trading, venue-side adjustments, and restart gaps
- without reconciliation, OMS-managed quantity eventually becomes unsafe to trust

Therefore:

- OMS-managed position quantity is the fast operational view
- exchange position quantity is the external truth anchor
- OMS should continuously compare and reconcile the two

This requirement applies to `Position`, not `Balance`.

- positions are OMS-managed and must be reconciled
- balances are exchange-owned and should be refreshed / invalidated rather than reconstructed from local bookkeeping

### 4. OMS still needs a local reservation ledger

If OMS stops mutating canonical balances locally, it still needs protection against over-ordering between exchange sync points.

Therefore OMS should maintain a local reservation layer derived from open orders:

- cash reservations
- inventory reservations

This reservation layer is not the same thing as canonical balance.

## Critical Review of the Domain Split

### Stock / ETF holdings under `Position`

This is the right choice.

Although stock and ETF holdings often feel "cash-like" from a custody perspective, they behave as instrument exposure for:

- risk
- hedge alignment
- execution
- portfolio construction

So:

- `AAPL`, `SPY`, `GLD`, `BTC-PERP`, `EURUSD CFD` belong under `Position`
- `USD`, `USDT`, `JPY`, `SGD`, `BTC` wallet balances belong under `Balance`

### Spot assets can appear in both domains

This is acceptable and should be expected.

Examples:

- `Balance(asset=BTC)` means raw BTC inventory or collateral
- `Position(instrument=BTC-USDT spot instrument)` means tradable exposure in the BTC-USDT market

These are not the same thing and should not be collapsed in OMS core.

### OMS should not derive canonical spot positions from cash balances

If a venue only reports asset balances, OMS should not silently convert those into canonical instrument positions.

If higher layers need synthetic spot exposure, that should be a derived view, not the core domain model.

### Spot position derivation from balances

GW publishes asset balances, not synthetic spot positions. OMS may derive sellable spot-inventory positions from balances as an operational projection. This is required for pre-trade sell checks and fill processing on spot instruments.

The shared utility `derive_spot_positions_from_balances` (in `zk-oms-rs/src/utils.rs`) implements this projection and must be used at all balance-to-spot-position projection points: backtester init, live OMS bootstrap, and any future reconcile path.

Rules:

- input: asset balances (per account) + SPOT instrument refdata
- output: `OmsManagedPosition` entries keyed by `base_asset` (the OMS `pos_symbol` for spot instruments)
- only SPOT instruments in refdata trigger derivation
- non-positive balances are skipped (no margin/borrow semantics)
- multiple SPOT instruments sharing the same `base_asset` produce one position
- this is an operational projection, not canonical exchange truth

Merge behavior with explicit positions:

- `merge_positions_with_override(derived, explicit)` — explicit entries override derived ones for the same `(account_id, instrument_code)`
- non-overlapping entries from both sets are kept

## Target Architecture

## Internal State Model

Replace the current single `OmsPosition`-based accounting path with three distinct internal state types.

### `OmsManagedPosition`

OMS-maintained executable exposure state.

Suggested fields:

- `account_id`
- `instrument_id`
- `symbol_exch`
- `is_short`
- `qty_total`
- `qty_frozen`
- `qty_available`
- `last_local_update_ts`
- `last_exch_sync_ts`
- optional local average cost / fill basis

This is the primary source for position quantity used by OMS.

Suggested reconcile-tracking fields:

- `last_exch_qty`
- `last_match_ts`
- `first_diverged_ts`
- `divergence_count`
- `reconcile_status`

### `ExchPositionSnapshot`

Exchange-reported position state.

Suggested fields:

- wrapped proto `Position`
- exchange raw payload
- sync timestamps

This holds exchange-owned economics and exchange-native position metadata.

### `ExchBalanceSnapshot`

Exchange-reported balance state.

Suggested fields:

- wrapped proto `Balance`
- exchange raw payload
- sync timestamps

This is the only canonical balance source inside OMS.

### `ReservationLedger`

OMS-local execution guardrail, not canonical account state.

Suggested keys:

- `account_id + asset`
- possibly `account_id + instrument_id` for inventory reservations

Suggested fields:

- `reserved_qty`
- associated order ids
- reservation type

## Manager Split

The current `BalanceManager` should be split into:

- `PositionManager`
- `BalanceManager`
- `ReservationManager`

### `PositionManager`

Owns:

- `managed_positions: HashMap<i64, HashMap<String, OmsManagedPosition>>`
- `exch_positions: HashMap<i64, HashMap<String, ExchPositionSnapshot>>`

Responsibilities:

- apply local position deltas from OMS order lifecycle
- merge exchange position updates
- build canonical `Position` view for query/publish
- track divergence between OMS-managed and exchange-reported quantity
- trigger auto-reconcile when drift becomes persistent or severe

### `BalanceManager`

Owns:

- `exch_balances: HashMap<i64, HashMap<String, ExchBalanceSnapshot>>`

Responsibilities:

- merge exchange balance updates
- expose query/publish view for `Balance`

Non-responsibilities:

- local order bookkeeping
- synthetic balance derivation

### `ReservationManager`

Owns:

- local reservations implied by active orders

Responsibilities:

- track cash reserved by pending/open buy orders
- track inventory reserved by pending/open sell orders
- expose effective available values for pre-trade checks

Non-responsibility:

- canonical account balance

## Gateway Integration Semantics

## Target Gateway Message Model

The target gateway integration should separate:

- `GatewayPositionUpdate`
- `GatewayBalanceUpdate`

OMS internal message types should eventually become:

- `OmsMessage::GatewayPositionUpdate(...)`
- `OmsMessage::GatewayBalanceUpdate(...)`

This is preferred over continuing with one overloaded balance-like gateway payload.

## Transitional Adapter

If the exchange gateway proto cannot be changed immediately, OMS service should add a thin adapter layer that:

- decodes the current gateway payload
- classifies entries as position-like or balance-like
- routes them to the correct internal manager

The classification should remain outside the core managers as much as possible.

## Classification Rule

Route to `Balance` when the entry represents asset inventory or cash ledger state.

Route to `Position` when the entry represents exposure in a tradable instrument.

Examples:

- USD / USDT / BTC wallet ledger -> `Balance`
- AAPL / SPY / BTC-PERP / EURUSD CFD -> `Position`

If a venue reports only asset balances and not instrument exposure, OMS should keep that as `Balance` rather than synthesizing canonical positions in core.

## Query Semantics

## `QueryBalances`

`QueryBalances` should return exchange-owned balances only.

Behavior:

- `query_gw=false`: cached exchange balance snapshot from OMS
- `query_gw=true`: optional direct gateway passthrough or freshest gateway-backed read if implemented later

The important rule is that the returned balance view is exchange-owned, not OMS-synthesized.

## `QueryPosition`

`QueryPosition` should remain supported and should return OMS canonical position view.

Canonical merge rule:

- quantity / frozen / available exposure fields: OMS-managed position wins
- pnl / margin / liquidation / leverage / exchange-native fields: exchange position snapshot wins when available

If no exchange position snapshot exists, `QueryPosition` should still return the OMS-managed quantity view.

If only exchange position snapshot exists, OMS may return it as a degraded read path, but this should be explicit in implementation comments.

## Publish Semantics

OMS outward publish surface should be:

- `OrderUpdateEvent`
- `BalanceUpdateEvent`
- `PositionUpdateEvent`

### `BalanceUpdateEvent`

Should be built from exchange-owned balance state only.

### `PositionUpdateEvent`

Should be built from the canonical OMS position view:

- OMS-managed quantity
- optionally enriched with exchange-owned margin/PnL fields

This preserves the rule that position quantity is the most trusted locally-maintained state.

## OMS Core Behavior Changes

## Current Problem

Today OMS local bookkeeping mutates a balance-like store for both:

- cash/fund legs
- instrument/position legs

This should be removed.

## Target Behavior

OMS order lifecycle should mutate:

- local managed positions
- local reservation ledger

OMS order lifecycle should not mutate:

- canonical balances

### Pre-trade check rule

Pre-trade checks should use:

- latest exchange balance snapshot
- minus local reservations not yet reflected by exchange updates

This gives OMS safe execution behavior without pretending to be the canonical balance ledger.

## Position Reconciliation

## Why reconciliation is required

OMS-managed position quantity is operationally critical, but it cannot be assumed to remain correct forever.

Common sources of drift include:

- missing or delayed order reports
- missed exchange position updates
- manual trading outside this OMS
- venue-side adjustments
- settlement / exercise / expiry effects
- recovery gaps after restart

Position reconciliation is therefore a first-class OMS responsibility.

## Reconcile model

The system should maintain two views for every position:

- OMS-managed quantity view
- exchange-reported quantity view

Canonical behavior:

- use OMS-managed quantity for immediate execution and hedge decisions
- compare it with exchange-reported quantity on every sync point
- escalate when divergence is persistent or unexplained

## Reconcile triggers

Reconciliation should run on both event-driven and periodic triggers.

Event-driven triggers:

- exchange position update received
- gateway order report received
- fill / trade update applied
- OMS recovery / warm-start completed

Periodic triggers:

- scheduled reconcile loop for active accounts
- scheduled deep reconcile for accounts with recent divergence

## Divergence states

Each managed position should carry a reconcile state such as:

- `InSync`
- `DivergedTransient`
- `DivergedPersistent`
- `ExchangeOnly`
- `OmsOnly`
- `ReconcileFailed`

Suggested meanings:

- `InSync`: OMS qty and exchange qty are within tolerance
- `DivergedTransient`: mismatch exists but is still within the short tolerance window
- `DivergedPersistent`: mismatch survived repeated checks or elapsed threshold
- `ExchangeOnly`: exchange knows the position but OMS does not
- `OmsOnly`: OMS has local position state but exchange does not
- `ReconcileFailed`: explicit reconcile attempt did not restore a trusted state

## Divergence thresholds

Thresholds should be instrument-aware.

Recommended policy:

- integer/share/contract products: exact match required
- floating-point products: compare using venue/instrument quantity step or epsilon

Each position should track:

- absolute quantity difference
- divergence age
- number of consecutive mismatches

## Reconcile action policy

### Small or brief mismatch

If mismatch is small or recent:

- mark `DivergedTransient`
- emit metric/log
- do not immediately rewrite OMS-managed quantity

### Persistent mismatch

If mismatch survives threshold:

- trigger active reconcile query against gateway / exchange
- refresh exchange position snapshot
- attempt to explain drift using open orders, pending fills, and local reservations

### Severe or unexplained mismatch

If mismatch remains unexplained after active reconcile:

- mark `DivergedPersistent` or `ReconcileFailed`
- emit alert
- optionally block new orders for the affected account/instrument
- optionally put strategy / hedger into degraded mode

## Trust rule during reconcile

Recommended asymmetric trust rule:

- during normal operation, OMS-managed quantity drives execution
- after persistent unexplained divergence, exchange quantity becomes the recovery anchor

This means OMS should not overwrite local quantity on every one-off mismatch.

But after threshold breach and explicit reconcile, OMS should be able to realign local managed quantity to exchange quantity.

## Recovery actions

When recovery is required, OMS should support:

- refresh exchange position query
- rebuild exchange position snapshot
- reset OMS-managed quantity to exchange quantity
- mark the reset in audit/logging/metrics

This reset should be explicit and observable.

## Query semantics during divergence

During divergence, `QueryPosition` should still return the canonical OMS position view, but the system should expose reconcile state internally and eventually in the API if needed.

At minimum:

- logs and metrics should show divergence
- risk / hedge layers should be able to see account or instrument degrade state

Future extension:

- add reconcile metadata to `Position`
- or expose a side-channel diagnostics query

## Publish semantics during divergence

`PositionUpdateEvent` should continue to publish the canonical OMS position view.

However, the system should also emit internal telemetry when:

- drift begins
- drift becomes persistent
- a forced reset is applied
- reconcile fails

This telemetry is required for safe live operations.

## Replica / Snapshot Model

The read replica should eventually store distinct maps for:

- managed positions
- exchange positions
- exchange balances
- optional reservations

This enables clean query semantics and avoids leaking manager conflation into gRPC handlers.

## Proto Semantics Review

## `Position`

`Position` should stay instrument-scoped.

Good fields:

- `account_id`
- `instrument_code`
- `long_short_type`
- `instrument_type`
- `total_qty`
- `frozen_qty`
- `avail_qty`
- sync timestamps
- margin / pnl / liquidation metadata

## `Balance`

`Balance` should stay asset-scoped.

Good fields:

- `account_id`
- `asset`
- `total_qty`
- `frozen_qty`
- `avail_qty`
- optional borrowed / interest / net fields later
- sync timestamps

`Balance` should not carry `long_short_type`.

## Concrete Refactor Plan

### Phase 1: Domain split in code structure

- introduce `OmsManagedPosition`
- introduce `ExchPositionSnapshot`
- introduce `ExchBalanceSnapshot`
- introduce `ReservationLedger`
- stop using `OmsPosition` as the universal account-state record

### Phase 2: Manager split

- extract `PositionManager`
- reduce `BalanceManager` to exchange-owned balances only
- add `ReservationManager`

### Phase 3: OMS core bookkeeping split

- move local exposure mutations into `PositionManager`
- move local cash/inventory hold logic into `ReservationManager`
- stop mutating canonical balance state from order lifecycle
- add position divergence tracking and reconcile state storage

### Phase 4: Gateway integration split

- add internal separation between gateway balance updates and gateway position updates
- keep a temporary adapter if the upstream gateway payload is still overloaded
- add active reconcile fetch path for exchange positions

### Phase 5: Replica and query rewrite

- store separate snapshot maps
- rewrite `QueryBalances`
- rewrite `QueryPosition` to use canonical merged position semantics
- expose enough internal state for reconcile monitoring

### Phase 6: Publish rewrite

- publish true `BalanceUpdateEvent`
- publish canonical `PositionUpdateEvent`
- remove any remaining balance-to-position or position-to-balance compatibility behavior
- wire periodic and event-driven reconcile loops

## First Recommended Implementation Step

The first step should be to stop evolving the current `balance_mgr.rs` as a mixed account-state manager.

Recommended immediate change:

- extract a new `PositionManager` from the current OMS-managed and exchange-reported position-like maps
- narrow `BalanceManager` to exchange-owned asset balances only

This is the most important structural break because it prevents future features from deepening the current conflation.

## Backtester Impact And Required Refactor

The Rust backtester currently assumes a simpler accounting model than the target live OMS design. It already carries separate `BalanceUpdateEvent` and `PositionUpdateEvent` lanes at the runtime level, but its internal OMS simulation still depends on the old conflated bookkeeping model.

This means the backtester must be treated as a first-class migration surface, not just a consumer of the new protos.

### Current backtester mismatches

#### 1. Initial balances are still seeded as position-like records

Today [`BacktestOms::new()`](/Users/zzk/workspace/zklab/zkbot/rust/crates/zk-backtest-rs/src/backtest_oms.rs) converts `init_balances` into `OmsPosition` values via `build_init_positions()`.

That is incompatible with the target domain model because:

- initial cash / asset inventory should seed exchange-owned `Balance`
- initial instrument exposure should seed `Position`
- a single `init_balances: account_id -> symbol -> qty` map is no longer semantically sufficient

#### 2. The simulator does not model exchange-owned balances explicitly

The backtester simulator currently drives OMS almost entirely through order reports and OMS local bookkeeping.

Under the new design, live OMS balance truth comes from exchange push/query only. Backtest therefore needs an explicit simulated exchange balance state, not just OMS-local bookkeeping side effects.

Without that, backtest behavior will drift from live behavior in:

- pre-trade available cash checks
- asset inventory updates
- strategy callbacks that consume `BalanceUpdateEvent`
- Python `tq.get_account_balance()` parity

#### 3. The simulator does not yet have a true exchange position sync path

If live OMS uses:

- OMS-managed position quantity
- exchange position snapshot as reconcile anchor

then backtest must also decide how exchange position truth is represented.

Otherwise:

- reconcile logic cannot be tested
- divergence handling cannot be tested
- canonical `QueryPosition` merge semantics cannot be tested

#### 4. OMS output handling is still biased toward order and balance updates

[`BacktestOms`](/Users/zzk/workspace/zklab/zkbot/rust/crates/zk-backtest-rs/src/backtest_oms.rs) currently requeues:

- `OrderUpdateEvent`
- `BalanceUpdateEvent`

but does not yet model a full canonical position publication flow from OMS core. Once live OMS starts publishing real `PositionUpdateEvent` from `PositionManager`, backtest must mirror that.

#### 5. Current config naming is too narrow

`BacktestConfig.init_balances` is now underspecified for the target domain model.

The backtester needs to distinguish at least:

- initial asset balances
- initial positions

Otherwise spot inventory and instrument exposure are forced back into one initialization surface.

### Required backtester refactor

The backtester should be refactored so that its simulated exchange state matches the live OMS ownership rules.

### 1. Split backtest initialization inputs

Replace the single `init_balances` concept with separate initialization surfaces.

Recommended shape:

- `init_balances: account_id -> asset -> qty`
- `init_positions: account_id -> instrument -> qty`

Optional future extension:

- richer structs for frozen/available balances
- richer position seed fields like side, average price, exchange sync timestamp

This keeps backtest startup semantically aligned with the live system.

### 2. Introduce simulated exchange balance state

Backtest needs a simulated exchange-owned balance ledger.

Responsibilities:

- track cash / asset balances by account and asset
- update on simulated fills, cancels, settlement-like events
- emit `BalanceUpdateEvent`
- answer balance queries using exchange-owned state

This ledger should be the only canonical balance source in replay, matching live OMS semantics.

### 3. Introduce simulated exchange position state

Backtest also needs an exchange-facing position snapshot model.

Responsibilities:

- track exchange-reported position state by account and instrument
- support testing of position reconcile logic
- emit `PositionUpdateEvent` when exchange-side position state changes

This does not have to be fully exchange-realistic at first, but it must exist as a distinct state store.

### 4. Keep OMS-managed positions in replay

Replay OMS should still maintain local managed positions, just like live OMS.

Therefore backtest should simulate both:

- OMS-managed position state
- exchange position snapshot state

This allows replay to test:

- normal execution behavior
- divergence
- reconcile
- forced reset / degraded mode logic

### 5. Add a replay reservation model

If live OMS uses a reservation ledger rather than local canonical balances, replay must do the same.

Backtest pre-trade checks should eventually use:

- simulated exchange balance
- minus local replay reservations

not OMS-synthesized cash balance.

### 6. Update `BacktestOms` output plumbing

`BacktestOms` should be updated so OMS actions can requeue:

- `OrderUpdateEvent`
- `BalanceUpdateEvent`
- `PositionUpdateEvent`

and so replay order/fill processing can drive both balance and position changes through the correct simulated ownership layers.

### 7. Update strategy/runtime parity tests

Backtest parity tests should be expanded so they validate:

- `on_balance_update` visibility in `StrategyContext`
- `on_position_update` visibility in `StrategyContext`
- `get_balance()` and `get_spot_inventory()` semantics
- `get_position()` semantics
- Python adapter `get_account_balance()` behavior for asset balances

Additional reconcile-focused tests should be added later:

- OMS qty matches exchange qty
- transient divergence
- persistent divergence
- forced realignment

### 8. Preserve a simpler mode only if explicitly documented

If full live-parity simulation is too much for the first refactor step, the backtester may temporarily use a simplified exchange model.

But that simplification should be explicit:

- replay balances are still exchange-owned, even if simulated locally
- replay positions still maintain separate OMS-managed and exchange views
- reconcile may initially be deterministic or no-op, but the state split must exist

The system should not keep the old conflated `OmsPosition` initialization path as the long-term backtest architecture.

### Recommended phased backtester migration

#### Backtest Phase A: API and state split

- split `init_balances` and `init_positions`
- introduce separate simulated balance and position state stores
- stop seeding asset balances as `OmsPosition`

#### Backtest Phase B: event plumbing

- emit real `BalanceUpdateEvent`
- emit real `PositionUpdateEvent`
- update `BacktestOms` and event queue flows accordingly

#### Backtest Phase C: pre-trade and reservation parity

- add replay reservation ledger
- shift pre-trade checks to simulated exchange balance minus reservations

#### Backtest Phase D: reconcile parity

- add simulated exchange position sync
- add divergence and reconcile tests

### Summary rule for replay

Backtest should mirror live ownership rules:

- replay balances are exchange-owned state
- replay positions are OMS-managed but exchange-anchored
- replay reservations are local execution guardrails

If replay does not follow this split, strategy behavior in backtest will diverge from live in exactly the places this refactor is trying to clean up.

## Deferred TODOs

The following items are intentionally deferred and should be treated as follow-up implementation work after the core OMS domain split.

### ~~OMS service: persist balance and position snapshots to Redis~~ — **DONE**

Implemented: `OmsAction::PersistBalance` and `OmsAction::PersistPosition` variants emitted by OmsCore on balance/position updates. Service actor dispatches these to `RedisWriter::write_balance()` / `write_position()`. 24h TTL on all persisted entries.

### ~~OMS service: warm-start balance and position state~~ — **DONE**

Implemented: `RedisWriter::load_balances()` and `load_positions()` with `into_exch_balance_snapshot()` / `into_oms_managed_position()` conversion methods. Startup calls `core.init_state(warm_orders, persisted_positions, persisted_balances)`. Precedence: Redis warm-start first, then immediate gateway reconcile overwrites with fresh data.

### ~~Backtester: keep reservations enabled~~ — **DONE**

Reservations are active in the backtester by default. Pre-trade checks (balance + inventory) work. `sim_balances` feeds exchange balance state into OMS. No blanket disable flag exists.

### OMS service: canonical `QueryPosition` merge

Current state:

- `query_position(query_gw=false)` returns OMS-managed positions only
- exchange-owned economics in `exch_positions` are not merged into the default query path

TODO:

- merge managed quantity view with exchange position snapshot
- keep OMS-managed qty / frozen / available as primary
- enrich with exchange-owned PnL / margin / leverage / liquidation metadata when available

Reason:

- current query is operationally usable but narrower than the intended canonical position model

### ~~OMS service: startup reconcile against gateway~~ — **DONE** (balance only)

Implemented: `GwClientPool::query_account_balance()` added. Startup queries each connected gateway via `QueryAccountBalance`, injects results through `OmsCore::process_message(BalanceUpdate)`. Persist actions from reconcile are forwarded to Redis.

Note: position reconcile on startup is deferred — `mock-gw` `query_position` returns empty `PositionResponse`. Will be wired when gateways report meaningful position state.

### OMS service: reconcile loop and degraded-state handling

Current state:

- OMS core tracks reconcile state (`ReconcileStatus` enum)
- `PositionRecheck` message added to `OmsMessage` with noop handler
- Periodic timer emits `PositionRecheck` every 30s (configurable via `position_recheck_interval_secs`)
- Service layer does not yet drive active reconcile fetches or degradation actions

TODO:

- implement real reconcile logic in `OmsCore::process_position_recheck()` — compare managed vs exchange positions
- add event-driven reconcile triggers where appropriate
- define alert / degraded-mode / order-block behavior for persistent divergence

Reason:

- core tracking and periodic timer exist, but actual comparison + recovery logic is not complete yet

### OMS service: startup reconcile should persist all relevant actions

Current state:

- startup balance reconcile calls `OmsCore::process_message(BalanceUpdate)`
- the startup loop persists only `PersistBalance` actions
- any `PersistPosition` actions produced during the same reconcile pass are currently ignored

TODO:

- handle `PersistPosition` during startup reconcile, not just `PersistBalance`
- keep startup reconcile persistence behavior aligned with the normal OMS actor action-dispatch path
- add a regression test covering gateway reconcile that yields both balance and position persistence actions

Reason:

- otherwise startup reconcile updates in-memory position state without fully refreshing Redis-backed warm-start state

### OMS service: preserve richer warm-start metadata for balances and positions

Current state:

- Redis persistence now stores enough data to reconstruct basic balances and managed positions
- persisted balance/position records currently drop richer metadata such as sync timestamps, exchange raw payloads, and reconcile-tracking fields
- warm-start reconstruction resets those fields to defaults

TODO:

- extend persisted balance snapshots to retain freshness metadata needed for future reconcile logic
- extend persisted managed positions to retain reconcile-related state where it is useful across restart
- decide explicitly which fields are intentionally ephemeral versus restart-stable

Reason:

- current warm-start is good enough for basic inventory recovery, but not for richer reconcile and freshness semantics

## Non-Goals

This refactor does not attempt to:

- fully redesign gateway proto in the same PR
- introduce a complete portfolio accounting engine
- guarantee exact real-time margin parity with every exchange

The goal is narrower:

- clean domain separation
- reliable local position quantity
- exchange-owned balance truth
- explicit reservation semantics

## Final Rules

- `Position` = instrument exposure
- `Balance` = asset inventory / cash / collateral
- OMS owns canonical executable position quantity
- OMS must continuously reconcile managed position quantity to exchange position state
- exchange owns canonical balance, margin, and PnL economics
- OMS reservations are local execution guardrails, not balances
- OMS core should not synthesize canonical balances from order bookkeeping
