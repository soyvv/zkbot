# Audit Panel

## Purpose

Control-plane review and traceability.

## Subpages

- Operator Audit
- Registration Audit
- Reconciliation Audit

## Operator Audit

Main table columns:

- time
- user
- role
- action
- target
- result
- request id

### Sketch

```text
+----------------------------------------------------------------------------------+
| Operator Audit                 Filters: [User] [Action] [Result] [Time]           |
+----------------------------------------------------------------------------------+
| time       user       role       action          target         result    req_id   |
| 10:14      ops_a      ops        restart         oms_okx        OK        r_101    |
+----------------------------------------------------------------------------------+
```

## Registration Audit

Main table columns:

- time
- logical id
- instance type
- decision
- reason
- owner session id
- source

### Sketch

```text
+----------------------------------------------------------------------------------+
| Registration Audit            Filters: [Type] [Decision] [Logical ID]             |
+----------------------------------------------------------------------------------+
| time     logical_id   type   decision   reason        owner_session   source      |
| 10:12    gw_okx       gw     ACCEPTED   -             sess_abc        10.0.0.2    |
+----------------------------------------------------------------------------------+
```

## Reconciliation Audit

Main table columns:

- time
- reconciliation type
- scope
- status
- mismatch count
- summary

### Sketch

```text
+----------------------------------------------------------------------------------+
| Reconciliation Audit        Filters: [Type] [Status] [Scope] [Time]               |
+----------------------------------------------------------------------------------+
| time     recon_type   scope        status     mismatch_count   summary             |
| 10:00    TRADE        acct_123     COMPLETE   0                matched all         |
+----------------------------------------------------------------------------------+
```
