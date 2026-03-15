# Overview Panel

## Purpose

Landing page for current environment state.

## Main Blocks

- system health summary
- active services by type
- active strategies / executions
- active alerts
- recent operator actions
- quick links to failed or degraded areas

## Suggested Cards

- `OMS live / degraded / down`
- `GW live / degraded / down`
- `MDGW live / degraded / down`
- `Strategies running / stopped / fenced`
- `Refdata freshness`
- `Risk alerts`

## Recommended Layout

- top summary strip
  - environment
  - data freshness
  - last refresh time
  - active alerts
- left column
  - service health summary
  - strategy execution summary
- right column
  - recent operator actions
  - degraded services
  - latest risk alerts

## Recommended Widgets

- health heatmap by service family
- small topology summary card
- recent runtime events list
- “needs attention” queue

## Sketch

```text
+----------------------------------------------------------------------------------+
| Overview                           PROD        Fresh 8s ago        Alerts: 4     |
+----------------------------------------------------------------------------------+
| [OMS 12/1/0] [GW 18/2/0] [MDGW 4/1/0] [Strategies 22/3/1] [Refdata OK] [Risk 4] |
+----------------------------------------------+-----------------------------------+
| Service Health Heatmap                        | Needs Attention                   |
| OMS   GW   MDGW  REF   PILOT                 | - GW okx_123 degraded            |
| G G G A  G G G A  G A  G   G                | - Strategy arb_01 fenced        |
|                                              | - Refdata NYSE stale            |
+----------------------------------------------+-----------------------------------+
| Active Strategy Summary                      | Active Alerts                    |
| key        exec_id     status   oms          | sev  type        scope           |
| mm_btc     exec_101    RUNNING  oms_okx      | hi   risk        acct_123        |
| arb_sol    exec_222    DEGRADED oms_bin      | med  service     gw_okx_123      |
+----------------------------------------------+-----------------------------------+
| Recent Operator Actions                                                          |
| time         user        action                      result                       |
| 10:14:33     ops_a       restart oms_okx             OK                           |
| 10:12:10     trader_b    panic acct_123              ACCEPTED                     |
+----------------------------------------------------------------------------------+
```

## Primary Actions

- open degraded service detail
- open active alert
- jump to topology
- jump to running strategies

## Notes

- keep this page operational and compact
- do not turn it into a decorative dashboard
