# OKX Integration Design

## Positioning

OKX is the primary native Rust venue in the current architecture.

Recommended capability split:

- `gw`: Rust
- `rtmd`: Rust
- `refdata`: Python

Reasoning:

- OKX is stream-heavy and fits the generic gateway and RTMD service hosts well
- low-latency and steady streaming matter more for OKX than for OANDA or IBKR
- the architecture already expects refdata to be Python-loaded, so there is no reason to force OKX
  refdata into Rust

This venue should be the reference implementation for the native path described in
[venue_integration.md](/Users/zzk/workspace/zklab/zkbot/docs/system-arch/venue_integration.md).

## Module shape

Recommended module shape:

```text
venue-integrations/okx/
  manifest.yaml
  rust/
    gw.rs
    rtmd.rs
  python/
    refdata.py
  schemas/
    gw_config.schema.json
    rtmd_config.schema.json
    refdata_config.schema.json
```

Manifest language choices:

- `gw.language = rust`
- `rtmd.language = rust`
- `refdata.language = python`

## Recommended library choice

Production recommendation:

- implement `gw` and `rtmd` natively in Rust against OKX REST and WebSocket protocols
- use:
  - `reqwest` for REST
  - `tokio-tungstenite` for WebSocket

Secondary tooling / prototype option:

- `python-okx`

Reasoning:

- OKX is the best fit for the native path in this architecture
- the service design already expects OKX to be the performance-oriented venue
- a Python SDK is useful for examples and operator tooling, but it should not drive the production
  gateway design

Why this stack:

- `reqwest` is the standard async HTTP client in the Tokio ecosystem
- `tokio-tungstenite` keeps ping/pong, reconnect, resubscribe, and login flow explicit inside the adaptor
- explicit transport ownership is preferable for a native venue where correctness and recovery behavior
  are part of the adaptor contract

## Trading gateway design

Host:

- `zk-gw-svc`

Implementation path:

- native Rust adaptor resolved by the venue integration registry

Recommended adaptor mode:

- `hybrid`, with streaming as primary and query-based recovery as backstop

Gateway-owned responsibilities remain unchanged:

- command ACK semantics
- internal execution queueing
- semantic pipeline
- at-least-once order publication
- exactly-once trade publication
- trust-window handling for balance/position causality
- NATS publishing

OKX adaptor-owned responsibilities:

- REST signing and request translation
- WebSocket private/public session handling
- rate-limit behavior
- reconnect mechanics local to transport/session
- mapping raw OKX payloads into venue facts/events

Recommended behavior:

- `place_order` and `cancel_order` go through REST or exchange-supported command path
- private WebSocket stream is primary for order/trade/balance/position events
- `query_order`, `query_trades`, `query_balance`, and `query_positions` are used on reconnect,
  on stream-gap suspicion, and inside trust windows
- batch order features may be enabled if the normalized gateway contract can expose them cleanly

### Trading API mapping

Recommended initial mapping:

- `PlaceOrder`
  - OKX `POST / Place order`
- `BatchPlaceOrders`
  - OKX `POST / Place multiple orders`
- `CancelOrder`
  - OKX `POST / Cancel order`
- `BatchCancelOrders`
  - OKX `POST / Cancel multiple orders`
- `QueryOrder`
  - OKX `GET / Order details`
- `QueryTrades`
  - OKX `GET / Transaction details`
- `QueryBalance`
  - OKX `Get balance`
- `QueryPositions`
  - OKX `Get positions`

Primary event source:

- private WebSocket `Order channel`
- account, positions, and balance-and-position channels as account-state feeds

Recovery source:

- REST queries for orders, fills, balances, and positions

## RTMD design

Host:

- `zk-rtmd-gw-svc`

Implementation path:

- native Rust adaptor resolved from the venue manifest

Recommended scope:

- standalone venue-scoped RTMD gateway

Recommended capabilities:

- `tick`
- `orderbook`
- `kline`
- `funding`
- current snapshot queries
- bounded recent-history kline queries

Recommended runtime behavior:

- dedupe upstream subscriptions aggressively
- aggregate live interest from `zk.rtmd.subs.v1`
- use one upstream subscription per effective stream key
- publish `tick` and `orderbook` on plain NATS subjects
- publish `kline` on JetStream-backed flow where replay matters

Important OKX-specific note:

- orderbook semantics should stay adaptor-specific
- the host should not pretend all venues have identical depth/update guarantees

### RTMD API mapping

Recommended initial mapping:

- `tick`
  - WS `Tickers channel`
- `orderbook`
  - WS `Order book channel`
- `kline`
  - WS `Candlesticks channel`
- `funding`
  - public-data funding endpoints/channels
- current snapshot queries
  - REST `Get ticker`, `Get order book`
- bounded recent-history queries
  - REST `Get candlesticks`, `Get candlesticks history`

## Refdata design

Host:

- `zk-refdata-svc`

Implementation path:

- Python loader through the shared bridge described in
  [13-python-venue-bridge.md](/Users/zzk/workspace/zklab/zkbot/docs/system-redesign-plan/plan/13-python-venue-bridge.md)

Recommended source material:

- existing OKX metadata fetchers if already present
- otherwise one Python loader module inside the refdata runtime or venue module

Refresh characteristics:

- crypto metadata refresh every 5-15 minutes
- canonical write into `cfg.instrument_refdata`
- no blind overwrite; lifecycle-aware diffing

Recommended fields:

- spot and derivative instrument identity
- base/quote/settlement assets
- lot size, tick size, precision
- contract size
- lifecycle status
- extra venue properties needed by RTMD and trading adaptors

OKX does not need TradFi market-session ownership. For this venue:

- `supports_tradfi_sessions = false`
- `QueryMarketStatus` can remain minimal or venue-level

### Refdata API mapping

Recommended initial mapping:

- canonical instrument refresh
  - public `Get instruments`
- funding-related metadata
  - public-data funding endpoints where applicable
- additional venue metadata
  - public-data endpoints needed for contract size, lot size, tick size, and settlement details

## Venue config

Recommended gateway config fields:

- `api_key`
- `secret_key`
- `passphrase`
- `flag` or explicit environment selector for live vs demo
- `api_base_url`
- `ws_public_url`
- `ws_private_url`
- `account_mode`
- `secret_ref`
- `passphrase_ref`

Recommended RTMD config fields:

- `api_base_url`
- `ws_public_url`
- optional `ws_private_url` when account-scoped channels are needed

Recommended refdata config fields:

- `api_base_url`
- optional environment selector if demo and live differ operationally

Operational notes:

- API key permissions should be explicit: `Read`, `Trade`, and never `Withdraw` unless truly needed
- IP binding should be strongly preferred

## ID linkage mechanism

Recommended rule:

- use OKX `clOrdId` as the upstream `client_order_id`
- persist `clOrdId <-> ordId` on the first acknowledgment or event carrying both

Why:

- `clOrdId` is user-defined and is the cleanest venue-visible correlation key for the OMS mapping
- `ordId` is the venue-native durable order identifier for downstream venue queries and recovery

Gateway rule:

- never generate `clOrdId` in the adaptor
- accept the upstream-generated identity and propagate it unchanged

## Rate limiting handling

Recommended design:

- implement limiter buckets by API family rather than one flat limiter
- separate order-entry limits from account-query and market-data limits
- treat REST and WS trading operation limits as shared budget where OKX documents them that way

Recommended buckets:

- order placement / amend / cancel
- account and position queries
- public market-data queries
- WebSocket login / subscribe / unsubscribe

Operational notes:

- OKX documents trading-related limits and sub-account limits explicitly
- sub-account new/amend order limits are high enough for this design, but they still require explicit
  limiter ownership in the adaptor
- WS connection and channel-count limits should be enforced before the exchange rejects or drops the client

## Reconnect handling

Recommended design:

- treat reconnect as normal runtime behavior
- resubscribe deterministically
- run compensating order/account queries before declaring the gateway healthy

Recommended sequence:

1. reconnect transport
2. re-login on private WS if needed
3. resubscribe required channels
4. query open/recent orders and recent trades
5. query balances and positions
6. reconcile back into the gateway semantic pipeline

RTMD-specific rule:

- rebuild the upstream subscription set from effective live interest, not from local best guess

## References

- OKX API docs: https://app.okx.com/docs-v5/en/
- `python-okx` package: https://pypi.org/project/python-okx/

## Runtime ownership split

Service hosts own:

- bootstrap
- Vault integration
- registration
- query/publication contracts
- supervision

OKX modules own:

- API translation
- streaming/session details
- normalization quirks
- venue-specific config

## Initial rollout order

1. Finish native Rust OKX gateway adaptor.
2. Finish native Rust OKX RTMD adaptor.
3. Add Python OKX refdata loader into `zk-refdata-svc`.
4. Validate end-to-end discovery and registration from manifest-driven loading.

## Main risks

- reconnect and resubscribe correctness around stream gaps
- orderbook normalization drift if the adaptor overgeneralizes venue semantics
- leakage of venue-specific logic into the generic host instead of keeping it inside the adaptor

## Recommended tests

- gateway reconnect triggers compensating queries before final account-state publication
- private-stream order/trade events preserve linkage between `client_order_id` and venue order id
- RTMD subscription dedupe produces one upstream stream per effective key
- refdata refresh updates canonical rows without churning unchanged instruments
