# Protobuf Evolution Plan

## Rules

- prefer additive changes
- keep wire compatibility where practical
- document every new field/message with rollout notes

## Strategy protocol changes

### Add
- [ ] strategy runtime type
- [ ] strategy category
- [ ] market data requirements
- [ ] execution profile
- [ ] instrument universe metadata

## Worker runtime protocol

### Add
- [ ] strategy event envelope
- [ ] strategy action envelope
- [ ] snapshot request/response
- [ ] heartbeat
- [ ] runtime error/reporting messages

## OMS protocol changes

### Add
- [ ] amend/replace intent
- [ ] parent-child linkage
- [ ] basket or multi-leg intent where required
- [ ] richer execution instructions
- [ ] strategy/portfolio metadata tags

## Common instrument metadata changes

### Add
- [ ] option right
- [ ] strike price
- [ ] underlying instrument id
- [ ] richer multiplier/deliverable fields
- [ ] shortability/borrowability metadata where relevant
- [ ] trading session/calendar metadata
- [ ] broader ETF/stock/futures/options descriptors

## Acceptance criteria

- protocol set can represent CTA, market making, arbitrage, rebalance, and vol-related strategies without side channels
- protocol set can represent ETF, stock, crypto spot, crypto perp, CFD, and planned futures/options support coherently
