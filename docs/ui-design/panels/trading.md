# Trading Panel

## Purpose

Manual trading and account inspection.

## Subpages

- Accounts
- Account Detail
- Manual Order Ticket
- Recent Orders / Trades

## Accounts

Purpose:

- list operator-visible trading accounts and current high-level status

Main table columns:

- `account_id`
- venue
- account type
- bound OMS
- bound gateway
- trading status
- balance summary
- position count
- open order count
- latest risk state

Filters:

- venue
- account type
- enabled / disabled
- risk state
- OMS

Row actions:

- open account detail
- open manual order ticket
- open risk detail

### Sketch

```text
+----------------------------------------------------------------------------------+
| Trading / Accounts              Filters: [Venue] [OMS] [Risk] [Search]           |
+----------------------------------------------------------------------------------+
| account    venue   oms       gw        status    balances   pos   orders   risk  |
| 123        OKX     oms_okx   gw_okx    ACTIVE    1.2m       4     12       WARN  |
| 456        BIN     oms_bin   gw_bin    ACTIVE    0.8m       2     3        OK    |
+----------------------------------------------------------------------------------+
| [Open Detail] [Manual Order] [Open Risk]                                         |
+----------------------------------------------------------------------------------+
```

## Account Detail

Recommended layout:

- header
  - account identity
  - venue
  - OMS / gateway bindings
  - current trading state
  - risk status
- top metrics strip
  - total balance summary
  - open orders
  - positions
  - recent fills
- tabset
  - Balances
  - Positions
  - Open Orders
  - Recent Trades
  - Runtime / Binding

Balances grid:

- asset
- total
- available
- frozen
- updated at

Positions grid:

- instrument
- side
- quantity
- avg price
- unrealized PnL
- updated at

Open orders grid:

- order id
- client order id
- instrument
- side
- type
- price
- quantity
- status
- OMS
- created at

Recent trades grid:

- fill id
- order id
- instrument
- side
- price
- quantity
- fee
- time

Action area:

- manual order
- cancel selected orders
- panic
- clear panic
- open risk config

### Sketch

```text
+----------------------------------------------------------------------------------+
| Account 123 / OKX                    ACTIVE                     Risk: WARN        |
+----------------------------------------------------------------------------------+
| [Balance 1.2m] [Positions 4] [Open Orders 12] [Recent Fills 18]                  |
+----------------------------------------------------------------------------------+
| Tabs: Balances | Positions | Open Orders | Recent Trades | Runtime / Binding     |
+----------------------------------------------------------------------------------+
| Grid / tab content                                                               |
| asset/instrument ...                                                             |
| ...                                                                              |
+---------------------------------------------------+------------------------------+
| OMS / Gateway Binding                             | Actions                      |
| oms_okx -> gw_okx                                 | [Manual Order]               |
| last update 10:14:22                              | [Cancel Selected]            |
| risk flags: margin warn                           | [Panic] [Clear Panic]        |
|                                                   | [Open Risk Config]           |
+---------------------------------------------------+------------------------------+
```

## Manual Order Ticket

Recommended layout:

- left panel
  - order form
- right panel
  - account summary
  - instrument refdata snapshot
  - recent orders/trades for the account
  - warnings

Order form fields:

- account
- instrument
- side
- order type
- price
- quantity
- time in force if needed
- optional tags/notes

Pre-submit review block:

- OMS / gateway route
- account state
- lifecycle / refdata status
- risk warnings
- confirmation text for dangerous orders

Primary actions:

- preview
- submit
- reset

### Sketch

```text
+----------------------------------------------------------------------------------+
| Manual Order Ticket                                                              |
+----------------------------------------------+-----------------------------------+
| Form                                         | Account / Instrument Context      |
| account        [123        v]                | account status: ACTIVE            |
| instrument     [BTC-USDT   v]                | oms route: oms_okx                |
| side           [BUY   v]                     | gw route: gw_okx                  |
| type           [LIMIT v]                     | lifecycle: ACTIVE                 |
| price          [________]                    | risk warnings: 1                  |
| quantity       [________]                    | recent orders / trades            |
| tif            [GTC    v]                    | ...                               |
| note           [______________]              |                                   |
+----------------------------------------------+-----------------------------------+
| Review: route / refdata / risk / confirmation text                               |
+----------------------------------------------------------------------------------+
| [Preview] [Submit Order] [Reset]                                                 |
+----------------------------------------------------------------------------------+
```

## Recent Orders / Trades

Purpose:

- cross-account operational inspection

Recommended layout:

- split view with orders grid and trades grid
- shared global filter bar

Filters:

- account
- venue
- OMS
- instrument
- order status
- time range
- manual vs bot origin if exposed

Useful row actions:

- open account detail
- open order detail drawer
- open related strategy execution if present

### Sketch

```text
+----------------------------------------------------------------------------------+
| Orders / Trades                   Filters: [Acct] [OMS] [Status] [Time]          |
+----------------------------------------------+-----------------------------------+
| Orders Grid                                   | Trades Grid                      |
| order_id  acct  instr   status   qty          | fill_id  acct  instr  qty price |
| ...                                           | ...                              |
+----------------------------------------------+-----------------------------------+
| Drawer / bottom detail: selected order or trade                                  |
+----------------------------------------------------------------------------------+
```
