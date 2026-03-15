# Risk Panel

## Purpose

Risk config and risk-state visibility.

## Subpages

- Risk Summary
- Account Risk Detail
- Alerts
- Alert Detail

## Risk Summary

Main table columns:

- account
- venue
- OMS
- current risk state
- disable state
- max order rate
- max position
- latest alert

### Sketch

```text
+----------------------------------------------------------------------------------+
| Risk Summary                    Filters: [Venue] [OMS] [State] [Account]          |
+----------------------------------------------------------------------------------+
| account   venue   oms      risk_state   disabled   max_rate   max_pos   alert     |
| 123       OKX     oms_okx  WARN         NO         20/s       5 BTC     margin    |
| 456       BIN     oms_bin  OK           NO         10/s       2 BTC     -         |
+----------------------------------------------------------------------------------+
```

## Account Risk Detail

Recommended layout:

- current risk state card
- editable config panel
- recent alerts panel
- linked account/runtime context

Primary actions:

- save config
- disable account
- enable account

### Sketch

```text
+----------------------------------------------------------------------------------+
| Account Risk Detail / 123                                                         |
+----------------------------------------------+-----------------------------------+
| Current Risk State                             | Editable Config                  |
| state: WARN                                    | max order rate [20]             |
| disabled: NO                                   | max position  [5 BTC]           |
| latest alert: margin warning                   | panic threshold [3]             |
+----------------------------------------------+-----------------------------------+
| Recent Alerts / Account Runtime Context                                            |
| ...                                                                              |
+----------------------------------------------------------------------------------+
| [Save Config] [Disable Account] [Enable Account]                                  |
+----------------------------------------------------------------------------------+
```

## Alerts

Main table columns:

- alert id
- severity
- category
- account
- OMS / service
- summary
- first seen
- latest seen
- status

Filters:

- severity
- category
- open / acknowledged / cleared
- account

### Sketch

```text
+----------------------------------------------------------------------------------+
| Alerts                           Filters: [Severity] [Category] [Status]          |
+----------------------------------------------------------------------------------+
| alert_id   severity   category   account   summary                 status         |
| al_101     HIGH       risk       123       max margin exceeded     OPEN           |
| al_102     MED        service    gw_okx    reconnect storm         ACK            |
+----------------------------------------------------------------------------------+
```

## Alert Detail

Recommended layout:

- alert summary
- related entity links
- event timeline
- operator notes/actions

### Sketch

```text
+----------------------------------------------------------------------------------+
| Alert Detail / al_101                                                             |
+----------------------------------------------+-----------------------------------+
| Summary                                       | Related Links                    |
| severity: HIGH                                | account 123                      |
| category: risk                                | oms_okx                          |
| status: OPEN                                  | strategy mm_btc                  |
+----------------------------------------------+-----------------------------------+
| Event Timeline                                                                    |
| 10:11 detected -> 10:12 repeated -> 10:14 still active                            |
+----------------------------------------------------------------------------------+
| Notes / Actions: [Acknowledge] [Open Account] [Disable Account]                   |
+----------------------------------------------------------------------------------+
```
