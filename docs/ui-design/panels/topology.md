# Topology Panel

## Purpose

Inspect services, bindings, sessions, and runtime liveness.

## Subpages

- Topology Graph
- Services
- Service Detail
- Sessions
- Runtime Config Drift

## Topology Graph

Purpose:

- operator-readable relationship view, not a giant network diagram

Recommended graph rules:

- group by service family
- show only key edges:
  - strategy -> OMS
  - OMS -> GW
  - strategy / shared scope -> MDGW
  - refdata shared service
- use status coloring on nodes
- show counts on collapsed groups

Right-side detail pane on selection:

- logical id
- registration kind
- live session state
- bindings
- latest change

### Sketch

```text
+----------------------------------------------------------------------------------+
| Topology Graph                                                                    |
+----------------------------------------------+-----------------------------------+
| Graph canvas                                   | Selected Node                    |
| [strategy mm_btc] --> [oms_okx] --> [gw_okx]   | logical id: oms_okx             |
| [strategy arb_sol] -> [oms_bin] -> [gw_bin]    | kind: oms                       |
| [shared mdgw_okx]                               | live: healthy                   |
| [refdata]                                       | bindings: gw_okx, acct 123      |
+----------------------------------------------+-----------------------------------+
```

## Services

Main table columns:

- logical id
- service type
- registration kind
- desired enabled state
- live status
- runtime endpoint
- version/build
- last seen
- config drift

Filters:

- service family
- live / degraded / down
- env scope
- registration kind

Bulk actions:

- reload selected
- restart selected

### Sketch

```text
+----------------------------------------------------------------------------------+
| Services                       Filters: [Type] [Status] [Kind] [Search]           |
+----------------------------------------------------------------------------------+
| logical_id   type   kind      live      endpoint         version   drift          |
| oms_okx      oms    oms       LIVE      10.0.0.1:5100    1.2.0     NO             |
| gw_okx       gw     gw        DEGRADED  10.0.0.2:5200    1.1.4     YES            |
+----------------------------------------------------------------------------------+
| [Reload Selected] [Restart Selected]                                              |
+----------------------------------------------------------------------------------+
```

## Service Detail

Recommended layout:

- header
  - logical id
  - service type
  - live state
  - desired enabled state
- summary cards
  - latest session
  - runtime endpoint
  - build/version
  - ownership/session metadata
- tabs
  - Runtime
  - Config
  - Bindings
  - Sessions
  - Audit

Action group:

- reload
- start
- stop
- restart
- issue bootstrap token

### Sketch

```text
+----------------------------------------------------------------------------------+
| Service Detail / oms_okx                                                          |
+----------------------------------------------------------------------------------+
| [LIVE] [Desired Enabled] [Last Seen 10:14:22] [Version 1.2.0]                    |
+----------------------------------------------+-----------------------------------+
| Runtime Summary                                | Actions                          |
| endpoint: 10.0.0.1:5100                        | [Reload] [Start] [Stop]          |
| session: sess_abc                              | [Restart] [Issue Token]          |
| ownership: active                              |                                  |
+----------------------------------------------+-----------------------------------+
| Tabs: Runtime | Config | Bindings | Sessions | Audit                             |
+----------------------------------------------------------------------------------+
```

## Sessions

Purpose:

- inspect bootstrap and live runtime ownership state

Main table columns:

- owner session id
- logical id
- instance type
- status
- kv key
- last seen
- source
- lease ttl

Filters:

- active / fenced / expired / deregistered
- service family
- logical id

### Sketch

```text
+----------------------------------------------------------------------------------+
| Sessions                        Filters: [Status] [Type] [Logical ID]             |
+----------------------------------------------------------------------------------+
| owner_session_id  logical_id   type   status   kv_key             last_seen       |
| sess_abc          oms_okx      oms    active   svc.oms.oms_okx    10:14:22        |
| sess_xyz          gw_okx       gw     fenced   svc.gw.gw_okx      10:12:01        |
+----------------------------------------------------------------------------------+
```

## Runtime Config Drift

Purpose:

- show desired-vs-observed config mismatch where supported

Main table columns:

- logical id
- service type
- drift status
- desired config version/hash
- observed config version/hash
- detected at

Primary actions:

- open diff
- reload
- restart

### Sketch

```text
+----------------------------------------------------------------------------------+
| Runtime Config Drift                                                              |
+----------------------------------------------------------------------------------+
| logical_id   type   drift   desired_hash   observed_hash   detected_at            |
| gw_okx       gw     YES     abc123         def456          10:11:02               |
+----------------------------------------------------------------------------------+
| Diff Drawer: desired vs observed summary                                          |
| [Open Diff] [Reload] [Restart]                                                    |
+----------------------------------------------------------------------------------+
```
