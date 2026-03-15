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