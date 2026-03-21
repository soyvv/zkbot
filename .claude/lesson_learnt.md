# Lessons Learnt

## 2026-03-16: gw-sim devops enablement

### OrderReport.exchange field must be stamped by the gateway
- OMS uses `OrderReport.exchange` as `gw_key` to map reports to accounts
- If empty, reports are silently discarded (`no account mapping for gw_key; discarding`)
- Old `zk-mock-gw` hardcoded `exchange: "MOCK"` in fill reports
- Fix: `nats_publisher.rs` clones report and stamps `exchange = gw_id` before publishing

### Sim order lifecycle must always emit Booked before Fill
- OMS expects full lifecycle: Pending → Booked → Filled/Cancelled
- `ImmediateMatchPolicy` was skipping Booked when matches existed, going straight to Fill
- OMS never published a Booked `OrderUpdateEvent`, causing bench timeouts
- Fix: `simulator.rs` always emits `make_booked_report()` before processing match results

### Match policy matters for E2E test flows
- `immediate` policy fills orders in the same `on_new_order` call — cancel flow (Flow 2) can't work because the order is already filled before cancel arrives
- Solution: use `fcfs` policy (fills only on tick data), use admin `ForceMatch` RPC for deterministic fills in Flow 3
- Default dev stack match policy changed from `immediate` to `fcfs`

### Redis flush kills running OMS
- `docker exec redis-cli flushall` removes OMS's KV fencing/heartbeat state
- OMS becomes unresponsive — must be restarted after Redis flush
- Use `scripts/clear_oms_redis.sh` (pattern-based delete) instead of `flushall`

### Docker DNS vs localhost for rpc_endpoint
- When OMS runs on host (not Docker), Docker DNS names like `gw-sim:51051` don't resolve
- `02_oms_svc_compat.sql` must use `localhost:51051` for `make dev-up + make oms-run` workflow
- For `dev-up-full` (OMS in Docker), Docker DNS works but localhost doesn't
