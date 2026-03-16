# Gateway Simulator

## Scope

The gateway simulator is a dedicated test-harness runtime for the trading gateway contract.

It is separate from the real gateway design because it must optimize for determinism, fault
injection, and integration coverage rather than real venue connectivity.

## Design Role

The simulator should:

- implement the same gateway RPC surface as real gateways
- publish the same normalized event families and delivery semantics
- support configurable matching/fill policies
- provide deterministic controls for OMS and engine integration tests

The existing Python simulator in
`zkbot/services/zk-service/src/tq_service_simulator/gw_simulator.py` is the reference behavioral
source during migration.

## Required Semantics

The simulator must preserve the same publication rules as the real gateway:

- trade/fill publication: exactly once
- order lifecycle publication: at least once
- position/balance publication after a trade/order event must reflect that change

This is important because historical inconsistency between these event families caused downstream
state bugs.

## Controls

The simulator should support:

- immediate, delayed, partial, and manual fills
- cancel-before-fill races
- deterministic reset between test cases
- configurable balance and position initialization
- fault injection for reconnects, retries, and duplicate order events

### Control Plane Boundary

Simulator-specific operational controls should not be added to the normal trading
`GatewayService` RPC surface.

Recommended rule:

- keep `GatewayService` behavior-compatible with real gateways
- expose simulator-only controls on a separate admin gRPC service
- treat that admin service as test-harness/operator tooling, not part of the live trading contract

Recommended service name:

- `GatewaySimulatorAdminService`

This avoids leaking simulator-only semantics such as forced fills and injected
synthetic faults into downstream production-facing interfaces.

## Proposed Admin RPCs

The first operator-facing admin surface should include:

- `ForceMatch`
- `InjectError`
- `ListInjectedErrors`
- `ClearInjectedError`
- `SetAccountState`
- `ResetSimulator`
- `AdvanceTime`
- `SubmitSyntheticTick`
- `ListOpenOrders`
- `PauseMatching`
- `ResumeMatching`
- `SetMatchPolicy`
- optional `GetSimulatorState`

### 1. `ForceMatch`

Purpose:

- deterministically force a fill or fill-sequence for an open order without
  waiting for market data / match policy

Recommended request shape:

- `gw_key`
- one of:
  - `order_id`
  - `client_order_id`
  - `exch_order_ref`
- `fill_mode`
  - `FULL`
  - `PARTIAL`
- `fill_qty` (required for partial)
- `fill_price`
  - optional; defaults to order price or current best executable price if omitted
- `publish_balance_update` default `true`
- `publish_position_update` default `true` when/if simulator supports distinct positions
- `reason` / `operator_tag`

Recommended response:

- `accepted`
- `matched_order_id`
- `generated_trade_count`
- `remaining_open_qty`
- `message`

Semantics:

- must fail if the target order is not currently open
- must be serialized through the same simulator core lock / event loop as normal order processing
- must emit the same downstream `OrderReport`, `BalanceUpdate`, and future `PositionUpdate`
  events as a normal match path
- must not bypass publication ordering rules

Implementation direction:

- add a `force_match_order(...)` path in simulator core
- reuse existing report generation and balance-update plumbing
- do not handcraft downstream publications in the gRPC handler

### 2. `InjectError`

Purpose:

- enqueue deterministic synthetic failures that are triggered when future
  requests match configured criteria

Recommended request shape:

- `error_id` optional; generated if omitted
- `scope`
  - `PLACE_ORDER`
  - `CANCEL_ORDER`
  - `QUERY_ACCOUNT_BALANCE`
  - `QUERY_ORDER_DETAIL`
  - optional later: `QUERY_POSITION`, `CONNECT`, `STREAM_PUBLISH`
- `match_criteria`
  - optional `account_id`
  - optional `side`
  - optional `instrument`
  - optional `order_id`
  - optional `client_order_id`
  - optional `exch_account_id`
- `effect`
  - `RPC_ERROR`
  - `REJECT_ORDER`
  - `DROP_REPORT`
  - `DELAY_RESPONSE`
  - `DUPLICATE_REPORT`
  - `DISCONNECT`
- effect parameters:
  - `grpc_status_code`
  - `error_message`
  - `delay_ms`
  - `duplicate_count`
- trigger policy:
  - `once`
  - `times`
  - `until_cleared`
- `priority`
  - lower number wins if multiple rules match
- `enabled`

Recommended response:

- `error_id`
- `accepted`
- `message`

Semantics:

- matching must be deterministic and documented
- first matching enabled rule by priority should fire, unless a future
  "stacking" mode is explicitly added
- the rule engine should decrement remaining trigger count atomically when fired
- all firings should be observable via logs and optional system events

Recommended first-step supported effects:

- `RPC_ERROR` for place/cancel/query
- `REJECT_ORDER` for place order
- `DELAY_RESPONSE` for place/cancel/query

Defer until later:

- transport disconnect simulation
- dropped or duplicated downstream event publication
- stream-level partial failures

### 3. `ListInjectedErrors` / `ClearInjectedError`

Purpose:

- make fault state observable and removable during long-running integration tests

Recommended behavior:

- `ListInjectedErrors` returns all active rules, remaining trigger counts, and
  last-fired timestamps
- `ClearInjectedError` removes one rule by `error_id`
- optional `ClearAllInjectedErrors`

### 4. `ResetSimulator`

Purpose:

- deterministic reset between test cases without process restart

Recommended behavior:

- clears all open orders
- clears pending synthetic error rules
- resets balances/positions/reservations to configured initial state
- clears published-position tracking and any duplicate-delivery buffers
- returns counts of cleared objects

Important rule:

- `ResetSimulator` should be explicit and operator-driven
- it must not be triggered implicitly by reconnect or config reload

### 5. `SetAccountState`

Purpose:

- atomically seed or overwrite simulator-owned account state for test setup,
  recovery, and scripted scenario control

Recommended request shape:

- `account_id` or `exch_account_id`
- repeated `balance_mutations`
  - `asset`
  - `total_qty`
  - optional `avail_qty`
  - optional `frozen_qty`
  - optional mutation mode: `UPSERT` / `REMOVE`
- repeated `position_mutations`
  - `instrument_code`
  - optional `long_short_type`
  - `total_qty`
  - optional `avail_qty`
  - optional `frozen_qty`
  - optional mutation mode: `UPSERT` / `REMOVE`
- optional `clear_existing_balances`
- optional `clear_existing_positions`
- optional `publish_updates` default `true`
- optional `reason` / `operator_tag`

Recommended response:

- `accepted`
- resulting account-state summary
- applied balance count
- applied position count
- `message`

Semantics:

- this mutates simulator-owned account state directly
- balance and position changes are applied atomically within one serialized control path
- if `publish_updates=true`, the simulator should emit the same downstream
  `BalanceUpdate` and `PositionUpdate` shapes used by normal simulator state transitions
- if the simulator does not yet model positions separately, `position_mutations`
  may be rejected or ignored explicitly rather than silently mapped from balances

Why this is preferred:

- test harnesses usually need multi-field reseeding, not one-asset-at-a-time mutation
- one RPC avoids transient half-applied state
- it fits reset/reseed workflows better than separate `SetBalance` and `SetPosition` calls

### 6. Optional `GetSimulatorState`

Useful for test harnesses and manual debugging.

Recommended response fields:

- open orders
- current balances
- current positions if modeled
- active error rules
- current match policy
- last tick / last event timestamps

This is optional for the first cut, but useful if you want grpcurl-operable diagnostics.

### 7. `AdvanceTime`

Purpose:

- provide a deterministic future hook for time-driven simulation behavior

Recommended request shape:

- `advance_ms`
- optional `reason` / `operator_tag`

Recommended current behavior:

- noop placeholder for now
- returns success with the acknowledged time delta

Reason:

- keeps the admin API shape ready for future delayed-fill, timeout, and scheduled-fault semantics
- avoids redesigning the control plane later when simulator time becomes explicit

### 8. `SubmitSyntheticTick`

Purpose:

- inject one synthetic market-data event directly into the simulator without
  requiring the full RTMD subscription path

Recommended request shape:

- `tick` payload matching the simulator's supported normalized tick shape
- optional `source_tag`
- optional `reason`

Recommended response:

- `accepted`
- affected order count
- generated report count
- `message`

Semantics:

- must enter through the same simulator tick-processing path as subscribed RTMD ticks
- should trigger matching exactly as normal tick ingestion would
- should be serialized through the same simulator core lock/event loop

### 9. `ListOpenOrders`

Purpose:

- provide lightweight observability for current simulated order state

Recommended response fields:

- repeated open orders
- order ids
- account ids
- instrument
- side
- remaining qty
- limit price
- creation/update timestamps

Semantics:

- should be a read-only admin/query helper
- should not mutate simulator state

### 10. `PauseMatching` / `ResumeMatching`

Purpose:

- let tests accumulate open orders first, then release matching deterministically

Recommended behavior:

- `PauseMatching` prevents tick-driven and automatic matching from executing
- order placement and cancellation still succeed and update open-order state
- `ResumeMatching` re-enables normal matching behavior

Recommended response:

- current paused/unpaused state
- message

Important rule:

- pause state should affect matching only, not admin visibility or query RPCs
- force-match should still work while matching is paused, unless explicitly disabled later

### 11. `SetMatchPolicy`

Purpose:

- switch simulator matching behavior at runtime without restart

Recommended request shape:

- `policy_name`
  - `immediate`
  - `fcfs`
  - future: `delayed`, `partial`, custom named policies
- optional `policy_config`
- optional `reason` / `operator_tag`

Recommended response:

- previous policy
- current policy
- accepted
- message

Semantics:

- must affect only future matching decisions
- should not retroactively rewrite already open orders or emitted reports
- policy switch should be logged as an admin action

## Error Injection Decision Model

Recommended internal shape:

- `InjectedErrorRule`
  - `error_id`
  - `scope`
  - `criteria`
  - `effect`
  - `remaining_triggers`
  - `priority`
  - `enabled`
  - `created_at`
  - `last_fired_at`

Recommended evaluation flow on each RPC:

1. build a normalized `RequestContext`
2. scan active rules for matching scope + criteria
3. choose first rule by priority
4. atomically mark/decrement the rule trigger state
5. apply the synthetic effect
6. log and optionally publish a simulator system event

Recommended criteria matching:

- all populated criteria fields must match
- absent criteria fields mean wildcard
- string fields should use exact match first; avoid regex in the first version

This keeps the feature deterministic and easy to reason about.

## Force-Match Decision Model

Recommended internal flow:

1. resolve the target open order
2. validate requested fill quantity and fill mode
3. construct a simulator `MatchResult`
4. pass that through the same simulator-core update/report generation path as a normal fill
5. publish resulting order and balance/position events

Important rule:

- `ForceMatch` should reuse core matching/report plumbing
- it should not directly mutate balances or emit reports from the gRPC edge

That keeps behavior aligned with backtester and live-simulator semantics.

## Operability And Safety

Recommended operator rules:

- simulator admin RPC should bind separately from normal gateway RPC if practical
- non-dev environments should be able to disable admin controls entirely
- every admin action should be logged with:
  - caller identity if available
  - request payload summary
  - resulting action id / error id
- optional future control topic:
  - `zk.gw.<gw_id>.system`
  - for reset, injected fault fired, forced fill, disconnect simulation

Recommended config flags:

- `enable_admin_controls`
- `enable_force_match`
- `enable_error_injection`

## State Model Impact

The simulator runtime should gain two explicit internal stores:

- `manual_control_state`
  - active injected-error rules
  - recent force-match audit entries
  - pause state
  - current match policy
- `sim_account_state`
  - balances
  - positions if modeled
  - open orders

Recommended mutation rule:

- `SetAccountState` should update `sim_account_state` directly
- those mutations should still flow through one serialized control path so that
  publications and in-memory state stay ordered

The control state should be in-memory only by default.

Reason:

- these are test-harness controls, not durable business state
- deterministic tests usually prefer explicit re-seeding rather than persistent admin mutations

## Rollout Plan

### Phase 1: minimal operator surface

- add `GatewaySimulatorAdminService`
- implement `ForceMatch`
- implement `InjectError` with:
  - `PLACE_ORDER`
  - `CANCEL_ORDER`
  - `QUERY_ACCOUNT_BALANCE`
  - effects: `RPC_ERROR`, `REJECT_ORDER`, `DELAY_RESPONSE`
- implement `ListInjectedErrors`
- implement `ClearInjectedError`

### Phase 2: reset and diagnostics

- add `ResetSimulator`
- add `SetAccountState`
- add `SubmitSyntheticTick`
- add `ListOpenOrders`
- add `PauseMatching` / `ResumeMatching`
- add `SetMatchPolicy`
- add `AdvanceTime` as a noop placeholder
- add optional `GetSimulatorState`
- add structured audit logging for admin operations

### Phase 3: richer fault and stream semantics

- duplicate-report injection
- dropped-report injection
- disconnect/reconnect simulation
- query-position simulation once the simulator owns a meaningful position model

## Non-Goals

The simulator admin surface should not:

- redefine the real gateway trading contract
- become a production control surface for real gateways
- bypass the simulator core publication pipeline
- introduce nondeterministic, hidden mutation paths

## Adaptor Relationship

The simulator does not replace venue adaptors. It complements them.

Recommended approach:

- keep the simulator on the same gateway-owned normalized publication contract
- allow simulator-specific match policy modules
- keep adaptor-specific behavior isolated from simulator behavior

This also means the simulator should exercise the gateway service's semantic unification layer,
not bypass it with simulator-specific publication rules.

Recommended simulator relationship to the adaptor API:

- simulator may implement the same `VenueAdapter` boundary
- simulator emits `VenueEvent` facts into the gateway service layer
- simulator-specific match policy decides which venue facts are produced, not how final gateway
  publication semantics are enforced

## Related Docs

- [Gateway Service](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/services/gateway_service.md)
- [API Contracts](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/api_contracts.md)



[USER_COMMENT]
- simulator should be able to be configured with some initial states (balances+positions), and update those state during trading
- simulator should support the same set of order matching algos/policy as the backtester.  
