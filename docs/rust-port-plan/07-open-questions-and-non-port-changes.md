# Open Questions And Non-Port Changes

## Open questions

- [ ] final naming for `TokkaQuant` replacement
- [ ] exact worker transport choice: current NATS subjects only or narrower IPC for some deployments
- [ ] whether Redis query schema stays fully compatible or moves to versioned read models
- [ ] which strategy set becomes the mandatory compatibility suite
- [ ] which production traces become the canonical replay corpus
- [ ] whether the local-data ETS reference workload should live as a copied script, wrapper, or test harness entrypoint

## Non-port changes worth capturing

### Naming cleanup
- [ ] reduce `tq_*` vs `zk_*` drift
- [ ] standardize runtime/service/package naming
- [ ] **Proto file renames** (requires `buf generate` re-run and downstream updates):
  - `tqrpc_exch_gw.proto` → `zk_exch_gw_rpc.proto` (generated module `tqrpc_exch_gw` → `zk_exch_gw_rpc`)
  - `tqrpc_ods.proto`, `tqrpc_oms.proto`, `tqrpc_ref.proto`, `tqrpc_rtmd.proto`, `tqrpc_strategy.proto` — same pattern
  - `OrderSourceType::OrderSourceTq` / `OrderSourceNonTq` → `OrderSourceZk` / `OrderSourceExternal`
  - Rust code-side renames already done: `nontq_order_dict` → `external_order_dict`, `handle_non_tq_orders` → `handle_external_orders`
- [ ] **Performance / arch follow-ups identified during Phase 2 OMS port**:
  - `OmsCore::calc_balance_changes_for_report` is a stub (returns `None`); full spot/margin bookkeeping deferred to Phase 2b
  - `pending_order_reports` cache has no max-size bound; consider capping to prevent unbounded growth
  - `order_id_queue` LRU eviction only removes from `exch_ref_to_order_id` but not `context_cache` — clean up both on eviction
  - Batch order/cancel collapsing builds intermediate `Vec<OmsAction>` then re-iterates; can be done in a single pass

### Performance / arch follow-ups identified during Phase 3 strategy SDK + backtester port

- **OMS balance updates not wired end-to-end in backtest**: `OmsCore::calc_balance_changes_for_report` (stub from Phase 2) means `PublishBalanceUpdate` is never emitted after fills. `StrategyContext.get_position()` therefore stays empty unless position updates are injected manually. Full bookkeeping is blocked on Phase 2b OmsCore work.
- **`advance_time` sets `current_ts_ms` twice per event**: `advance_time(strategy, event.ts_ms)` sets it, then `on_tick` sets it again to `tick.original_timestamp`. These should always agree in well-formed data but could diverge if events are mis-stamped. Consider asserting they match.
- **`StrategyContext.account_states` is keyed at startup**: Accounts must be declared at `StrategyRunner::new` time. Orders from accounts not in the initial set are silently dropped by `on_order_update`/`on_position_update`. Live engine will need a dynamic account registration path.
- **`TimerManager` heap has no deduplication**: Subscribing the same key twice creates two heap entries. The second entry fires spuriously. Add key-uniqueness enforcement if strategies re-subscribe on reinit.

### Runtime descriptors
- [ ] define explicit strategy runtime metadata
- [ ] define deployment/resource model hints

### Replayability by design
- [ ] ensure every critical service can emit replayable traces
- [ ] ensure parity comparison mode is first-class

### Query/read separation
- [ ] decide whether OMS reads move to a dedicated materialized view or remain snapshot-backed plus Redis projection

### Documentation hygiene
- [ ] keep this folder updated as implementation changes the plan
- [ ] link implementation PRs/changes back to the relevant workstream and phase
