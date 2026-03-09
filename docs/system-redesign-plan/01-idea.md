

After zk core rust port, considering redesign the system.

# Scope & Objective
- the system should be versatile and composable, and can adapt all types of strategy and configurations, from distributed micro-services to all-in-one latency optimized instances. 
- the core (oms/strategy core) should be stable
- transport can be plugged if needed. 


# Tech stack 
- program-lang:
    - hot-path components: rust
    - other: python + rust/golang/java (whichever suits the best)
- Middleware:
    - gRPC + NATS
        - gRPC for internal rpc; NATS for messaging
        - in-process/inter-thread queues for co-located components (2-layer / all-in-one modes)
        - may consider Aeron in the future for tick-to-trade path
    - wire format: protobuf
    - Redis: OMS warm-start cache + non-critical read replica (open orders, positions); not system of record
    - Postgresql for DB (storing refdata, config, account, trades (raw + reconciled), balance/position snapshots)
        - live order/position state is NOT stored in DB; OMS holds in memory + reconciles from gateway on startup
    - mongodb for event store (storing strategy events/logs/orders etc)
    - service registry and discovery: NATS KV (lease + heartbeat)
    - secret store: Vault
        - only for exchange API keys and private keys (gateway use only)
        - registration tokens for other services delivered via env or args

# Deployment mode

Three deploymode should be supported:
- 3-layer deployment: StrategyEngine + OMS + Gateway
    - for latency insenstive trading
    - like Kline based CTA, portfolio rebalancing etc.
- 2-layer deployment: StrategyEngine + OMS with GW plugins
    - for latency senstive trading
    - like normal market making, arbitrage etc. 
- All-in-one deployment: strategy, oms and connectivity in one process
    - latency priority trading


# Major services

## Strategy Engine

internal gRPC service for each strategy instance
Mode: embedded or standalone

support strategy start/stop/pause/resume/cmds 



## ZK Pilot Service

UI facing rest service + internal gRPC service(query only)
global instance

Functionalities:
- controls all OMSes + all strategy instances
- manage accounts (register/modify/delete etc)
- manage rtmd global subscriptions
- manage OMS configs (including risk params)
- strategy engine start/stop
- manage cron jobs (refdata loading, recon etc. )
- manage recording/recon configs

reference: 
- zkbot/services/zk-service/src/tq_service_app/tq_app.py
- zkbot/services/zk-service/src/tq_service_api/api_service_instrument_refdata.py
- zkbot/services/zk-service/src/tq_service_oms/ods_server.py
- zkbot/services/zk-service/src/tq_service_app/tq_rtmd_registry_mgr.py



## OMS (Order Management & Risk Control Service)
Multiple instances
gRPC service

each OMS will manage one or more accounts (via GWs)
place/cancel orders; queries;

reference:
- zkbot/services/zk-service/src/tq_service_oms/oms_server.py

## Trading Gateway
One instance per account

Trading Simulator is a type of Trading Gw.


## MarketData Gateway
One instance per venue

subscription info stored in NATS kv (or redis?)
Dynamically subscribe for each symbol;

## Trading Data Recording & Recon Service

Global Service (can be multiple-instance)

records
- trades / balance / position
- signals / mktdata / fundingrate etc

recons
- oms trades vs gw/account trades

reference:
- zkbot/services/zk-service/src/tq_service_app/tq_strat_recorder.py
- zkbot/services/zk-service/src/tq_service_app/tq_rtmd_recorder.py
- zkbot/services/zk-service/src/tq_service_app/tq_signal_recorder.py
- zkbot/services/zk-service/src/tq_service_oms/oms_recorder.py
- zkbot/services/zk-service/src/tq_service_oms/order_event_recorder.py
- zkbot/services/zk-service/src/tq_service_oms/post_trade_handler.py
- zkbot/services/zk-service/src/tq_service_oms/oms_balance_collector.py

## Refdata Loader

Periodic refdata loader (to DB)

## Trading Monitor

Global 

functionality:
- read risk monitor config, and monitor related events
- send risk notification if needed

refence:
- zkbot/services/zk-service/src/tq_service_riskmonitor/pnl_monitor.py
- zkbot/services/zk-service/src/tq_service_riskmonitor/order_updates_monitor.py
- zkbot/services/zk-service/src/tq_service_riskmonitor/open_order_monitor.py
- zkbot/services/zk-service/src/tq_service_riskmonitor/nats_channel_monitor.py
- zkbot/services/zk-service/src/tq_service_riskmonitor/balance_updates_monitor.py
- zkbot/services/zk-service/src/tq_service_helper/notification_hub.py
