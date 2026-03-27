# OKX Integration Stabilization Plan

## Purpose

This note captures the current OKX demo-trading integration issues observed during
local Pilot onboarding and separates them into dispatchable work items.

The three issues are related in operator experience, but they are not the same
runtime defect:

- reject visibility is an observability problem
- stale pending orders are an OMS state-convergence problem
- missing positions in Pilot are a gateway/OMS position-query integration problem

They should be fixed in sequence so each step improves both usability and
correctness without conflating root causes.

## Current Issues

### 1. OKX reject reason visibility is too weak

Observed behavior:

- gateway logs show top-level OKX failure text such as `All operations failed`
- operators cannot see the useful venue rejection context quickly
- synthetic rejection reports do not reliably preserve the most actionable venue
  reason end to end

Practical impact:

- hard to tell whether the rejection was caused by margin, instrument mode,
  account config, order parameters, or demo-environment quirks
- debugging requires code reading or raw log spelunking

### 2. Rejected orders can remain pending in OMS until reconcile

Observed behavior:

- one rejected perp order remained `Pending` in OMS
- OMS only converged later after the order reconcile scan

Practical impact:

- OMS loses immediate truth for a rejected order
- operators cannot trust the live order state without waiting for background repair

This issue is distinct from restart recovery:

- restart reconcile is a safety net
- immediate gateway reject handling must converge OMS without waiting for a timer

### 3. OKX positions are not visible in Pilot

Observed behavior:

- OKX gateway receives balance updates
- perp positions are not surfaced in Pilot query results

Practical impact:

- operational visibility for derivatives is incomplete
- balance visibility and position visibility diverge in ways that confuse operators

This issue overlaps with the broader OMS position/balance catch-up work, but it
still needs an OKX-specific execution path to become usable in the near term.

## Fix Sequence

### Phase 1: Reject observability

Goal:

- make OKX rejections explain themselves in gateway logs and propagated rejection
  payloads

Why first:

- this is the fastest operator-facing improvement
- it reduces debugging time for every later order-state issue

### Phase 2: Immediate OMS reject convergence

Goal:

- ensure an OKX adapter rejection transitions the order to `Rejected` in OMS
  immediately, without waiting for reconcile

Why second:

- correctness comes before background recovery
- order reconcile should remain a repair path, not the primary source of truth

### Phase 3: OKX position query and Pilot visibility

Goal:

- wire real OKX position query through gateway and OMS so Pilot can read usable
  perp position state

Why third:

- this overlaps with broader position/balance architecture work already tracked
- the immediate production pain is lower than broken order-state convergence

## Design Constraints

- keep gateway/OMS rejection handling compatible with synthetic reject reports
- do not assume restart reconcile is an acceptable substitute for immediate
  order-state convergence
- do not silently invent canonical position semantics outside the existing
  `oms_position_balance_refactor.md` direction
- prefer preserving raw venue reason text alongside normalized rejection fields

## Related Work

- [zb-00004](/Users/zzk/workspace/zklab/zkbot/docs/superpowers/tickets/zb-00004.md):
  live gateway `QueryPosition` plus OMS position reconcile
- [zb-00005](/Users/zzk/workspace/zklab/zkbot/docs/superpowers/tickets/zb-00005.md):
  canonical OMS position query and publication semantics
- [zb-00007](/Users/zzk/workspace/zklab/zkbot/docs/superpowers/tickets/zb-00007.md):
  OMS pending-order restart reconcile and periodic order recheck

## Exit Criteria

- OKX rejects are operator-readable from gateway logs alone
- OMS transitions rejected OKX orders immediately without waiting for recheck
- OKX positions are queryable through the live OMS/Pilot path
