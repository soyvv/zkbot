# Ops Panel

## Purpose

Bounded runtime operations and runtime-definition management.

## Subpages

- Gateways
- OMS
- RTMD Gateways
- Bootstrap Tokens
- Runtime Actions

Important note:

- deployment is not a UI function
- the UI should only expose bounded runtime operations through Pilot

## Gateways

Main table columns:

- gw id
- venue
- account
- enabled
- live status
- adaptor type
- adaptor mode
- last seen

Primary actions:

- onboard
- view config
- reload
- start
- stop
- restart

### Sketch

```text
+----------------------------------------------------------------------------------+
| Ops / Gateways                 Filters: [Venue] [Status] [Mode]                   |
+----------------------------------------------------------------------------------+
| gw_id    venue   account   enabled   live       adaptor   mode      last_seen     |
| gw_okx   OKX     123       YES       LIVE       okx       hybrid    10:14         |
| gw_bin   BIN     456       YES       DEGRADED   binance   hybrid    10:13         |
+----------------------------------------------------------------------------------+
| [Onboard] [View Config] [Reload] [Start] [Stop] [Restart]                         |
+----------------------------------------------------------------------------------+
```

## OMS

Main table columns:

- oms id
- account count
- bound gateways
- enabled
- live status
- last seen

### Sketch

```text
+----------------------------------------------------------------------------------+
| Ops / OMS                                                                       |
+----------------------------------------------------------------------------------+
| oms_id    account_count   bound_gateways     enabled   live     last_seen         |
| oms_okx   4               gw_okx, gw_okx2    YES       LIVE     10:14             |
+----------------------------------------------------------------------------------+
```

## RTMD Gateways

Main table columns:

- logical id
- venue
- publisher mode
- subscription scope
- live status
- query capability
- last seen

### Sketch

```text
+----------------------------------------------------------------------------------+
| Ops / RTMD Gateways                                                               |
+----------------------------------------------------------------------------------+
| logical_id   venue   mode        scope         live      query     last_seen      |
| mdgw_okx     OKX     standalone  global        LIVE      YES       10:14          |
+----------------------------------------------------------------------------------+
```

## Bootstrap Tokens

Purpose:

- issue and rotate startup credentials

Main table columns:

- logical id
- instance type
- token jti / reference
- status
- expires at
- last issued

Primary actions:

- issue
- rotate
- revoke

### Sketch

```text
+----------------------------------------------------------------------------------+
| Bootstrap Tokens                                                                  |
+----------------------------------------------------------------------------------+
| logical_id   type    token_ref     status    expires_at      last_issued          |
| gw_okx       gw      jti_abc       ACTIVE    2026-03-18      2026-03-14           |
+----------------------------------------------------------------------------------+
| [Issue] [Rotate] [Revoke]                                                         |
+----------------------------------------------------------------------------------+
```

## Runtime Actions

Purpose:

- bounded runtime control routed through Pilot

Action patterns:

- single target action
- selected-scope action
- confirmation dialog with exact target scope

### Sketch

```text
+----------------------------------------------------------------------------------+
| Runtime Actions                                                                   |
+----------------------------------------------+-----------------------------------+
| Target Selector                                | Confirmation / Result            |
| service type [gw v]                            | action: restart                  |
| logical id   [gw_okx v]                        | target: gw_okx                   |
| action       [restart v]                       | scope shown explicitly           |
+----------------------------------------------+-----------------------------------+
| [Execute Action]                                                                 |
+----------------------------------------------------------------------------------+
```
