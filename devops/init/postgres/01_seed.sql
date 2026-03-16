-- Minimal seed data for local development and integration tests.

-- cfg.logical_instance (required before account_binding and mon.active_session)
insert into cfg.logical_instance (logical_id, instance_type, env) values
  ('oms_dev_1',  'OMS', 'dev'),
  ('gw_sim_1',   'GW',  'dev');

-- cfg.oms_instance
insert into cfg.oms_instance (oms_id, namespace, description) values
  ('oms_dev_1', 'dev', 'Local dev OMS instance');

-- cfg.gateway_instance
insert into cfg.gateway_instance (
  gw_id, venue, broker_type, account_type,
  supports_batch_order, supports_batch_cancel
) values (
  'gw_sim_1', 'SIM', 'SIM', 'SPOT',
  true, true
);

-- cfg.account
insert into cfg.account (account_id, exch_account_id, venue, broker_type, account_type, base_currency) values
  (9001, 'sim_account_9001', 'SIM', 'SIM', 'SPOT', 'USDT');

-- cfg.account_binding
insert into cfg.account_binding (account_id, oms_id, gw_id) values
  (9001, 'oms_dev_1', 'gw_sim_1');

-- cfg.instrument_refdata
insert into cfg.instrument_refdata (
  instrument_id, instrument_exch, venue, instrument_type,
  base_asset, quote_asset, settlement_asset,
  price_tick_size, qty_lot_size, price_precision, qty_precision,
  min_order_qty, max_order_qty
) values (
  'BTCUSDT_SIM', 'BTC-USDT', 'SIM', 'SPOT',
  'BTC', 'USDT', 'USDT',
  0.01, 0.00001, 2, 5,
  0.00001, 100.0
);

-- cfg.strategy_definition
insert into cfg.strategy_definition (strategy_id, runtime_type, code_ref, description) values
  ('strat_test', 'RUST', 'zk_engine_rs::strategies::test_strategy', 'Local dev test strategy');
