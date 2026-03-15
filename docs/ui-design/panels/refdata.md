# Refdata Panel

## Purpose

Operational browsing and lifecycle management.

## Subpages

- Instrument Browser
- Instrument Detail
- Market Status
- Market Calendar
- Refresh Actions

## Instrument Browser

Main table columns:

- instrument id
- venue
- venue symbol
- instrument type
- base / quote
- lifecycle status
- updated at
- market/session hint

Filters:

- venue
- instrument type
- lifecycle status
- asset
- search by symbol or instrument id

Row actions:

- open detail
- block / disable / deprecate
- targeted refresh

### Sketch

```text
+----------------------------------------------------------------------------------+
| Refdata / Instruments            Filters: [Venue] [Type] [Status] [Search]        |
+----------------------------------------------------------------------------------+
| instrument_id   venue   symbol      type   status      updated_at                 |
| BTC-USDT.OKX    OKX     BTC-USDT    SPOT   ACTIVE      10:03                      |
| ES.CME.M26      CME     ESM26       FUT    BLOCKED     09:40                      |
+----------------------------------------------------------------------------------+
| [Open Detail] [Block] [Disable] [Deprecate] [Refresh]                            |
+----------------------------------------------------------------------------------+
```

## Instrument Detail

Recommended layout:

- header
  - instrument id
  - venue
  - lifecycle status
- tabs
  - Basic
  - Venue Mapping
  - Trading Params
  - Session Context
  - Audit

Primary fields:

- canonical identity
- venue symbol mapping
- tick/lot/min notional
- lifecycle status
- last update watermark
- market/session status

Action group:

- block
- disable
- deprecate
- refresh

### Sketch

```text
+----------------------------------------------------------------------------------+
| Instrument BTC-USDT.OKX                ACTIVE                                     |
+----------------------------------------------------------------------------------+
| Tabs: Basic | Venue Mapping | Trading Params | Session Context | Audit            |
+----------------------------------------------+-----------------------------------+
| Canonical Fields                              | Actions                          |
| venue: OKX                                    | [Block] [Disable]               |
| symbol: BTC-USDT                              | [Deprecate] [Refresh]           |
| tick size: 0.1                                |                                  |
| lot size: 0.0001                              |                                  |
| watermark: 10211                              |                                  |
+----------------------------------------------+-----------------------------------+
```

## Market Status

Main table columns:

- venue
- market
- current session state
- effective at
- updated at
- freshness

Filters:

- venue
- session state
- stale / fresh

### Sketch

```text
+----------------------------------------------------------------------------------+
| Market Status                 Filters: [Venue] [State] [Freshness]                |
+----------------------------------------------------------------------------------+
| venue   market   session_state   effective_at      updated_at      freshness      |
| NYSE    CASH     OPEN            09:30             10:14           FRESH          |
| HKEX    CASH     CLOSED          16:00             16:01           FRESH          |
+----------------------------------------------------------------------------------+
```

## Market Calendar

Purpose:

- operational calendar inspection for TradFi markets

Recommended layout:

- market selector
- calendar/list hybrid
- next session summary panel

### Sketch

```text
+----------------------------------------------------------------------------------+
| Market Calendar / NYSE                                                            |
+----------------------------------------------+-----------------------------------+
| Calendar / session list                        | Next Session Summary             |
| 14 Mon  OPEN                                   | next open: 09:30 EDT            |
| 15 Tue  OPEN                                   | next close: 16:00 EDT           |
| 16 Wed  HOLIDAY                                | special notes: -                |
+----------------------------------------------+-----------------------------------+
```

## Refresh Actions

Purpose:

- explicit operator workflow for refdata refresh

Action cards:

- refresh one instrument
- refresh one venue
- refresh market calendar
- view recent refresh outcomes

### Sketch

```text
+----------------------------------------------------------------------------------+
| Refdata Refresh Actions                                                           |
+----------------------------------------------------------------------------------+
| [Refresh Instrument]  [Refresh Venue]  [Refresh Market Calendar]                  |
+----------------------------------------------------------------------------------+
| Recent Refresh Outcomes                                                           |
| time        scope              result      watermark                              |
| 10:12       instrument BTC...  OK          10211                                 |
| 09:55       venue OKX          OK          10202                                 |
+----------------------------------------------------------------------------------+
```
