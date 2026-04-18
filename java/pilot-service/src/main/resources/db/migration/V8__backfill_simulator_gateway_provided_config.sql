-- Backfill the local dev simulator GW config so Pilot bootstrap returns a usable payload.
-- Simulator GW config is stored inline at the top level for schema/UI compatibility.

UPDATE cfg.gateway_instance
SET provided_config = jsonb_build_object(
        'venue', 'simulator',
        'account_id', 9001,
        'mock_balances', 'BTC:10,USDT:100000,ETH:50',
        'fill_delay_ms', 0,
        'match_policy', 'fcfs',
        'admin_grpc_port', 51052,
        'enable_admin_controls', true
    )
WHERE gw_id = 'gw_sim_1'
  AND venue = 'simulator'
  AND (
      provided_config = '{}'::jsonb
      OR NOT (provided_config ? 'account_id')
  );
