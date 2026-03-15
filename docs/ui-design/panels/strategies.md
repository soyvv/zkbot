# Strategies Panel

## Purpose

Strategy definition management and live execution control.

## Subpages

- Strategy List
- Strategy Detail
- Strategy Config Editor
- Execution History
- Execution Detail / Logs

## Strategy List

Main table columns:

- `strategy_key`
- status
- runtime type
- bound account scope
- bound OMS
- current execution id
- last start time
- last stop time
- owner / updated by

Filters:

- enabled / disabled
- running / stopped / fenced
- runtime type
- OMS
- account

Row actions:

- open detail
- validate
- start
- stop
- restart

### Sketch

```text
+----------------------------------------------------------------------------------+
| Strategies                      Filters: [Status] [OMS] [Runtime] [Search]       |
+----------------------------------------------------------------------------------+
| strategy_key   status    runtime   oms       exec_id      last_start             |
| mm_btc         RUNNING   RUST      oms_okx   exec_101     10:12                  |
| arb_sol        FENCED    PY        oms_bin   exec_222     09:55                  |
+----------------------------------------------------------------------------------+
| [Open] [Validate] [Start] [Stop] [Restart]                                       |
+----------------------------------------------------------------------------------+
```

## Strategy Detail

Recommended layout:

- header
  - strategy key
  - current status
  - runtime type
  - last operator action
- top summary strip
  - current execution
  - bound accounts
  - OMS binding
  - RTMD mode
  - refdata/session notes
- content split
  - left: desired config and bindings
  - right: current runtime state and recent lifecycle events

Tabs:

- Summary
- Config
- Execution
- Logs
- Audit

Primary actions:

- validate config
- start execution
- stop execution
- restart execution
- edit config

### Sketch

```text
+----------------------------------------------------------------------------------+
| Strategy mm_btc                 RUNNING                 exec_101                  |
+----------------------------------------------------------------------------------+
| [Execution RUNNING] [Accounts 2] [OMS oms_okx] [RTMD embedded] [Session OK]      |
+----------------------------------------------+-----------------------------------+
| Desired Config / Bindings                     | Runtime State / Lifecycle        |
| runtime: RUST                                 | state: RUNNING                   |
| accounts: 123, 456                            | started: 10:12:11                |
| oms: oms_okx                                  | host: engine_mm_btc              |
| rtmd: embedded                                | recent events...                 |
+----------------------------------------------+-----------------------------------+
| Tabs: Summary | Config | Execution | Logs | Audit                                |
+----------------------------------------------------------------------------------+
| [Validate] [Start] [Stop] [Restart] [Edit Config]                                |
+----------------------------------------------------------------------------------+
```

## Strategy Config Editor

Recommended shape:

- form-first page with side-by-side config preview
- explicit sections:
  - metadata
  - runtime type
  - account bindings
  - OMS binding
  - RTMD mode/profile
  - strategy config payload

Editor behavior:

- client-side schema validation
- diff view against currently active desired config
- save draft or apply update

### Sketch

```text
+----------------------------------------------------------------------------------+
| Strategy Config Editor                                                            |
+----------------------------------------------+-----------------------------------+
| Form Sections                                 | Diff / Preview                    |
| metadata                                      | current vs pending                |
| runtime type                                  | - oms binding changed             |
| account bindings                              | - rtmd mode changed               |
| oms binding                                   | - config payload changed          |
| rtmd profile                                  |                                   |
| strategy payload json                         |                                   |
+----------------------------------------------+-----------------------------------+
| Validation messages: ...                                                          |
+----------------------------------------------------------------------------------+
| [Save Draft] [Apply Update] [Cancel]                                              |
+----------------------------------------------------------------------------------+
```

## Execution History

Main table columns:

- execution id
- strategy key
- start time
- stop time
- result
- stop reason
- runtime host/logical instance

Filters:

- strategy
- time range
- result

### Sketch

```text
+----------------------------------------------------------------------------------+
| Execution History                Filters: [Strategy] [Result] [Time Range]        |
+----------------------------------------------------------------------------------+
| execution_id   strategy    start      stop       result     runtime host          |
| exec_101       mm_btc      10:12      -          RUNNING    engine_mm_btc         |
| exec_089       mm_btc      09:00      09:48      STOPPED    engine_mm_btc_old     |
+----------------------------------------------------------------------------------+
```

## Execution Detail / Logs

Recommended layout:

- execution summary card
- lifecycle timeline
- recent runtime state
- log stream panel

Key fields:

- execution id
- strategy key
- started at / ended at
- runtime mode
- current control status
- latest degradation/fencing reason if any

### Sketch

```text
+----------------------------------------------------------------------------------+
| Execution exec_101                 RUNNING                                        |
+----------------------------------------------+-----------------------------------+
| Summary                                       | Log Stream                        |
| strategy: mm_btc                              | 10:14:01 ...                     |
| runtime: RUST                                 | 10:14:04 ...                     |
| control status: healthy                       | 10:14:08 ...                     |
| fencing reason: -                             | ...                              |
+----------------------------------------------+-----------------------------------+
| Lifecycle Timeline                                                                 |
| STARTING ---- RUNNING ---------------------------------------------------->       |
+----------------------------------------------------------------------------------+
```
