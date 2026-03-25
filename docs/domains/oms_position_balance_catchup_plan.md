# OMS Position / Balance Catch-Up Plan

## Purpose

This document translates the target domain model in `oms_position_balance_refactor.md`
into a dispatchable implementation plan for the current codebase.

The main problem is not that the design is unclear. The problem is that the live
runtime is still in a transitional state:

- balances and positions are partly separated, but not end to end
- position reconcile is designed but not operational
- spot inventory projection exists as a utility, but is not consistently wired into
  live OMS
- gateway contracts still use a mixed balance/position carrier on the inbound path
- replay/backtest has started to move, but is still not the same architecture as live OMS

This doc defines the intended catch-up sequence so work can proceed without redoing
the same review.

## Design Invariants

These rules are already established by `oms_position_balance_refactor.md` and should
be treated as fixed unless a deliberate design change is approved.

### 1. `Balance` and `Position` are different concepts

- `Balance` is asset-scoped exchange-owned inventory, cash, or collateral
- `Position` is instrument-scoped executable exposure

Examples:

- `Balance(asset=BTC)` is wallet or collateral inventory
- `Position(instrument=BTC-USDT spot instrument)` is tradable spot exposure
- `Position(instrument=BTC-USDT-SWAP)` is derivative exposure

### 2. OMS owns executable position quantity, not canonical balances

- OMS-managed quantity is the fast operational view for execution and risk checks
- exchange balance is the canonical source for asset inventory, cash, collateral, margin
- OMS may cache balances, but should not reconstruct canonical balances from local bookkeeping

### 3. OMS must reconcile positions continuously

- managed position quantity will drift over time
- exchange position state is the truth anchor
- reconcile is first-class runtime behavior, not deferred cleanup

### 4. OMS must keep a reservation layer

- reservations are local execution guardrails
- they are derived from in-flight orders
- they are not balances and should never be presented as balances

### 5. Spot inventory projection is operational, not canonical

- gateway should publish asset balances as balances
- OMS may derive sellable spot exposure from those balances
- derived spot exposure is used for sell checks and execution logic
- derived spot exposure must not replace canonical exchange balances

## Current Runtime State

### What is already aligned

- OMS V2 has distinct stores for exchange balances, managed positions, exchange positions,
  and reservations
- `QueryBalances` returns exchange-owned balances
- warm-start persists balances and positions separately
- unresolved exchange balances are now preserved instead of being dropped
- backtest init already derives spot positions from balances and merges them with explicit positions

### What is still transitional

#### Gateway message shape remains overloaded

The gateway still publishes both asset balances and position-like exposure through the
same `BalanceUpdate` envelope with `PositionReport` entries.

Implication:

- OMS must still classify spot vs non-spot at ingest time
- gateway and OMS remain coupled through an overloaded wire shape

This is acceptable as a temporary compatibility layer, but it is not the target design.

#### Position reconcile is not live

The service starts a periodic `PositionRecheck`, but the core handler is still a no-op.
Startup reconcile is balance-only in practice.

Implication:

- OMS-managed position quantity can drift without a real repair path
- the design promise around exchange-anchored quantity is not yet true in production terms

#### Canonical `QueryPosition` is not implemented

Current behavior is split:

- one path returns exchange snapshots
- one path returns managed positions

The target model requires a merged canonical position view:

- quantity/frozen/available come from OMS-managed exposure
- exchange-owned economics come from the exchange snapshot when available

#### Spot position projection is only partially wired

The utility exists and replay uses it, but live OMS does not yet systematically derive
spot executable exposure from exchange balances at bootstrap/reconcile points.

Implication:

- the design and utilities say spot inventory can drive operational exposure
- live OMS still mostly treats spot updates as balance-only state

#### Replay is not yet the live model

Backtest has started separating balances from positions, but it still uses the legacy
OMS core path and therefore cannot be treated as a faithful validation target for the
new runtime semantics.

## Catch-Up Strategy

The implementation should proceed in ordered phases. Do not try to finish every domain
goal in one patch.

### Phase 1: Make live position reconcile real

Goals:

- implement real gateway `QueryPosition`
- call it during OMS startup reconcile
- make periodic `PositionRecheck` compare managed vs exchange positions
- persist resulting exchange position refreshes

Exit criteria:

- OMS can fetch exchange positions from live gateways
- startup reconcile updates both exchange balance state and exchange position state
- periodic recheck updates reconcile status instead of being a no-op

### Phase 2: Implement canonical position query and publication

Goals:

- define one canonical `QueryPosition` view
- merge managed quantity with exchange-owned economics
- make `PositionUpdateEvent` publish the same canonical view

Exit criteria:

- `QueryPosition` no longer exposes the current managed-vs-gateway split as the primary model
- event publication and gRPC reads agree on one position contract

### Phase 3: Wire spot exposure projection into live OMS

Goals:

- derive sellable spot exposure from exchange balances during live bootstrap/reconcile
- define precedence between derived spot positions and explicit exchange position snapshots
- use this derived view for spot sell-side checks

Exit criteria:

- spot asset balances remain canonical balances
- spot executable exposure exists as an OMS-managed operational projection
- sell checks do not require collapsing balance and position into one store

### Phase 4: Split gateway position and balance update contracts

Goals:

- add a distinct gateway-originated position update path
- keep balance updates asset-scoped
- reduce OMS ingest dependence on spot-vs-nonspot classification of one mixed payload

Exit criteria:

- gateway position reports and balance reports are separate concepts on the wire
- OMS ingest path no longer depends on one overloaded message type for both domains

### Phase 5: Bring replay onto the same architecture

Goals:

- move replay off the legacy conflated OMS core path
- keep separate simulated exchange balances, exchange position snapshots, managed positions,
  and reservations
- make replay validate the same semantics as live OMS

Exit criteria:

- replay is a meaningful testbed for the new live architecture
- strategy tests cover the same balance/position boundaries as production runtime

## Scope Boundaries

These are intentionally out of scope for the catch-up work unless a specific ticket says otherwise.

- redesigning risk policy beyond existing reservation semantics
- redesigning external strategy APIs unless needed to expose canonical merged position state
- introducing venue-specific spot semantics inside OMS core
- replacing all compatibility paths in one step

## Non-Goals

- Do not collapse spot balance and spot exposure back into one store
- Do not let OMS synthesize canonical balances from local fills or reservations
- Do not rely on backtest parity as proof of live correctness until replay is migrated

## Validation Requirements

Every implementation phase should include:

- targeted Rust unit tests in `zk-oms-rs`, `zk-oms-svc`, or `zk-gw-svc`
- one integration-level path covering startup reconcile or query semantics
- explicit checks that `Balance` remains asset-scoped and `Position` remains instrument-scoped

Regression coverage should lock these invariants:

- balances are exchange-owned
- reservations are not balances
- positions are instrument exposure
- spot exposure may be derived operationally from balances
- canonical position query semantics are stable across runtime and replay
