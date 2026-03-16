use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context};
use futures::StreamExt;
use prost::Message as ProstMessage;

use zk_infra_rs::nats::subject;
use zk_oms_svc::proto::gw_svc::{
    gateway_service_client::GatewayServiceClient,
    gateway_simulator_admin_service_client::GatewaySimulatorAdminServiceClient,
};
use zk_oms_svc::proto::oms_svc::oms_service_client::OmsServiceClient;
use zk_proto_rs::zk::exch_gw::v1::GatewaySystemEvent;
use zk_proto_rs::zk::gateway::v1::{
    AccountResponse as GwAccountResponse, FillMode, ForceMatchRequest, QueryAccountRequest,
};
use zk_proto_rs::zk::oms::v1::{
    OrderDetailResponse, OrderStatus, QueryBalancesRequest, QueryOpenOrderRequest,
    QueryPositionRequest,
};

use crate::cli::Cli;
use crate::report::ScenarioReport;
use crate::support::{
    connect_channel, connect_nats, gateway_response_ok, make_gw_place, make_oms_cancel,
    make_oms_place, next_order_id, oms_response_ok, resolve_gw_addr, resolve_oms_addr, timed_step,
    wait_for_any_gw_balance, wait_for_any_oms_balance, wait_for_any_oms_position,
    wait_for_gw_order_report, wait_for_latency_metric, wait_for_oms_order_update,
};

pub(crate) async fn run_gw_smoke(
    cli: &Cli,
    gw_id: &str,
    gw_addr: &Option<String>,
) -> ScenarioReport {
    let mut report = ScenarioReport::new("gw-smoke");

    let nats = match timed_step(&mut report, "connect_nats", async {
        let nats = connect_nats(&cli.nats_url).await?;
        Ok((nats, format!("connected to {}", cli.nats_url)))
    })
    .await
    {
        Ok(v) => v,
        Err(e) => {
            report.fail(e.to_string());
            return report;
        }
    };

    let gw_addr = match timed_step(&mut report, "resolve_gateway", async {
        let addr = resolve_gw_addr(&nats, &cli.grpc_host, gw_id, gw_addr).await?;
        Ok((addr.clone(), format!("resolved {gw_id} to {addr}")))
    })
    .await
    {
        Ok(v) => v,
        Err(e) => {
            report.fail(e.to_string());
            return report;
        }
    };
    report.observe("gw_addr", gw_addr.clone());

    let channel = match timed_step(&mut report, "connect_gateway_grpc", async {
        let channel = connect_channel(&gw_addr).await?;
        Ok((channel, "gateway gRPC channel established".to_string()))
    })
    .await
    {
        Ok(v) => v,
        Err(e) => {
            report.fail(e.to_string());
            return report;
        }
    };
    let mut gw_client = GatewayServiceClient::new(channel);

    let mut report_sub = nats
        .subscribe(subject::gw_report(gw_id))
        .await
        .expect("gateway report subscription failed");
    let mut balance_sub = nats
        .subscribe(subject::gw_balance(gw_id))
        .await
        .expect("gateway balance subscription failed");
    let mut system_sub = nats
        .subscribe(format!("zk.gw.{gw_id}.system"))
        .await
        .expect("gateway system subscription failed");

    let _ = timed_step(&mut report, "health_check", async {
        let resp = gw_client
            .health_check(tonic::Request::new(Default::default()))
            .await
            .map_err(|e| anyhow!("gateway health_check failed: {e}"))?
            .into_inner();
        Ok(((), format!("status={}", resp.status)))
    })
    .await;

    let _ = timed_step(&mut report, "query_balance", async {
        let resp: GwAccountResponse = gw_client
            .query_account_balance(tonic::Request::new(QueryAccountRequest::default()))
            .await
            .map_err(|e| anyhow!("gateway query_account_balance failed: {e}"))?
            .into_inner();
        let count = resp
            .balance_update
            .as_ref()
            .map(|b| b.balances.len())
            .unwrap_or(0);
        Ok(((), format!("returned {count} balance entries")))
    })
    .await;

    let order_id = next_order_id();
    let place_started = Instant::now();
    let place_resp = match timed_step(&mut report, "place_order", async {
        let resp = gw_client
            .place_order(tonic::Request::new(make_gw_place(cli, order_id)))
            .await
            .map_err(|e| anyhow!("gateway place_order failed: {e}"))?
            .into_inner();
        if !gateway_response_ok(&resp) {
            bail!("gateway place_order returned failure: {}", resp.message);
        }
        Ok((
            resp,
            format!(
                "place_order ack in {} ms",
                place_started.elapsed().as_millis()
            ),
        ))
    })
    .await
    {
        Ok(v) => v,
        Err(e) => {
            report.fail(e.to_string());
            return report;
        }
    };
    report.observe("gw_ack_ts_ms", place_resp.timestamp.to_string());

    let report_event = match timed_step(&mut report, "wait_order_report", async {
        let event = wait_for_gw_order_report(&mut report_sub, order_id, cli.timeout_ms).await?;
        Ok((
            event.clone(),
            format!(
                "received order report exch_order_ref={} update_ts={}",
                event.exch_order_ref, event.update_timestamp
            ),
        ))
    })
    .await
    {
        Ok(v) => v,
        Err(e) => {
            report.fail(e.to_string());
            return report;
        }
    };
    report.observe(
        "gw_order_report_latency_ms",
        place_started.elapsed().as_millis().to_string(),
    );
    report.observe("gw_exch_order_ref", report_event.exch_order_ref.clone());

    let _ = timed_step(&mut report, "optional_balance_push", async {
        match wait_for_any_gw_balance(&mut balance_sub, 1_000).await {
            Ok(update) => Ok((
                (),
                format!(
                    "received balance push with {} entries",
                    update.balances.len()
                ),
            )),
            Err(_) => Ok(((), "no balance push observed within 1s".to_string())),
        }
    })
    .await;

    let _ = timed_step(&mut report, "optional_system_event", async {
        match tokio::time::timeout(Duration::from_millis(500), system_sub.next()).await {
            Ok(Some(msg)) => {
                let event = GatewaySystemEvent::decode(msg.payload.as_ref())
                    .context("failed to decode GatewaySystemEvent")?;
                Ok(((), format!("system event type={}", event.event_type)))
            }
            _ => Ok((
                (),
                "no system event observed after subscription".to_string(),
            )),
        }
    })
    .await;

    report.summary = "gateway smoke completed".to_string();
    report
}

pub(crate) async fn run_oms_smoke(
    cli: &Cli,
    oms_id: &str,
    oms_addr: &Option<String>,
) -> ScenarioReport {
    let mut report = ScenarioReport::new("oms-smoke");

    let nats = match timed_step(&mut report, "connect_nats", async {
        let nats = connect_nats(&cli.nats_url).await?;
        Ok((nats, format!("connected to {}", cli.nats_url)))
    })
    .await
    {
        Ok(v) => v,
        Err(e) => {
            report.fail(e.to_string());
            return report;
        }
    };

    let oms_addr = match timed_step(&mut report, "resolve_oms", async {
        let addr = resolve_oms_addr(&nats, &cli.grpc_host, oms_id, oms_addr).await?;
        Ok((addr.clone(), format!("resolved {oms_id} to {addr}")))
    })
    .await
    {
        Ok(v) => v,
        Err(e) => {
            report.fail(e.to_string());
            return report;
        }
    };
    report.observe("oms_addr", oms_addr.clone());

    let channel = match timed_step(&mut report, "connect_oms_grpc", async {
        let channel = connect_channel(&oms_addr).await?;
        Ok((channel, "OMS gRPC channel established".to_string()))
    })
    .await
    {
        Ok(v) => v,
        Err(e) => {
            report.fail(e.to_string());
            return report;
        }
    };
    let mut oms_client = OmsServiceClient::new(channel);

    let mut order_sub = nats
        .subscribe(subject::oms_order_update(oms_id, cli.account_id))
        .await
        .expect("OMS order update subscription failed");
    let mut balance_sub = nats
        .subscribe(subject::oms_balance_update(oms_id))
        .await
        .expect("OMS balance update subscription failed");
    let mut position_sub = nats
        .subscribe(subject::oms_position_update(oms_id))
        .await
        .expect("OMS position update subscription failed");
    let mut latency_sub = nats
        .subscribe(format!("zk.oms.{oms_id}.metrics.latency"))
        .await
        .expect("OMS latency subscription failed");

    let _ = timed_step(&mut report, "health_check", async {
        let resp = oms_client
            .health_check(tonic::Request::new(Default::default()))
            .await
            .map_err(|e| anyhow!("OMS health_check failed: {e}"))?
            .into_inner();
        Ok(((), format!("status={}", resp.status)))
    })
    .await;

    let _ = timed_step(&mut report, "query_balances", async {
        let resp = oms_client
            .query_balances(tonic::Request::new(QueryBalancesRequest {
                account_id: cli.account_id,
                query_gw: false,
            }))
            .await
            .map_err(|e| anyhow!("OMS query_balances failed: {e}"))?
            .into_inner();
        Ok(((), format!("returned {} balances", resp.balances.len())))
    })
    .await;

    let _ = timed_step(&mut report, "query_positions", async {
        let resp = oms_client
            .query_position(tonic::Request::new(QueryPositionRequest {
                account_id: cli.account_id,
                query_gw: false,
            }))
            .await
            .map_err(|e| anyhow!("OMS query_position failed: {e}"))?
            .into_inner();
        Ok(((), format!("returned {} positions", resp.positions.len())))
    })
    .await;

    let open_before: OrderDetailResponse = oms_client
        .query_open_orders(tonic::Request::new(QueryOpenOrderRequest {
            account_id: cli.account_id,
            query_gw: false,
            pagination: None,
        }))
        .await
        .map(|r| r.into_inner())
        .unwrap_or_else(|_| OrderDetailResponse {
            orders: vec![],
            pagination: None,
        });
    report.observe("open_orders_before", open_before.orders.len().to_string());

    let order_id = next_order_id();
    let t0 = crate::support::now_ms();
    let place_resp = match timed_step(&mut report, "place_order", async {
        let resp = oms_client
            .place_order(tonic::Request::new(make_oms_place(cli, order_id)))
            .await
            .map_err(|e| anyhow!("OMS place_order failed: {e}"))?
            .into_inner();
        if !oms_response_ok(&resp) {
            bail!("OMS place_order returned failure: {}", resp.message);
        }
        Ok((resp, "OMS accepted place_order".to_string()))
    })
    .await
    {
        Ok(v) => v,
        Err(e) => {
            report.fail(e.to_string());
            return report;
        }
    };
    report.observe("oms_ack_ts_ms", place_resp.timestamp.to_string());
    report.observe("client_place_ts_ms", t0.to_string());

    let booked = match timed_step(&mut report, "wait_order_update", async {
        let event =
            wait_for_oms_order_update(&mut order_sub, order_id, None, cli.timeout_ms).await?;
        let status = event
            .order_snapshot
            .as_ref()
            .and_then(|o| OrderStatus::try_from(o.order_status).ok())
            .map(|s| s.as_str_name().to_string())
            .unwrap_or_else(|| "UNKNOWN".to_string());
        Ok((event, format!("received OMS order update status={status}")))
    })
    .await
    {
        Ok(v) => v,
        Err(e) => {
            report.fail(e.to_string());
            return report;
        }
    };
    report.observe(
        "oms_first_update_latency_ms",
        (crate::support::now_ms() - t0).to_string(),
    );

    let status = booked
        .order_snapshot
        .as_ref()
        .and_then(|o| OrderStatus::try_from(o.order_status).ok());
    if matches!(
        status,
        Some(OrderStatus::Booked) | Some(OrderStatus::Pending) | Some(OrderStatus::PartiallyFilled)
    ) {
        let _ = timed_step(&mut report, "cancel_order", async {
            let resp = oms_client
                .cancel_order(tonic::Request::new(make_oms_cancel(order_id)))
                .await
                .map_err(|e| anyhow!("OMS cancel_order failed: {e}"))?
                .into_inner();
            if !oms_response_ok(&resp) {
                bail!("OMS cancel_order returned failure: {}", resp.message);
            }
            Ok(((), "OMS accepted cancel_order".to_string()))
        })
        .await;

        let _ = timed_step(&mut report, "wait_cancelled_update", async {
            let event = wait_for_oms_order_update(
                &mut order_sub,
                order_id,
                Some(OrderStatus::Cancelled),
                cli.timeout_ms,
            )
            .await?;
            Ok((
                (),
                format!("received OMS cancelled update ts={}", event.timestamp),
            ))
        })
        .await;
    } else {
        report.note("cancel step skipped because first OMS update was already terminal");
    }

    let _ = timed_step(&mut report, "optional_balance_push", async {
        match wait_for_any_oms_balance(&mut balance_sub, 1_000).await {
            Ok(update) => Ok((
                (),
                format!(
                    "received OMS balance push with {} balances",
                    update.balance_snapshots.len()
                ),
            )),
            Err(_) => Ok(((), "no OMS balance push observed within 1s".to_string())),
        }
    })
    .await;

    let _ = timed_step(&mut report, "optional_position_push", async {
        match wait_for_any_oms_position(&mut position_sub, 1_000).await {
            Ok(update) => Ok((
                (),
                format!(
                    "received OMS position push with {} positions",
                    update.position_snapshots.len()
                ),
            )),
            Err(_) => Ok(((), "no OMS position push observed within 1s".to_string())),
        }
    })
    .await;

    let _ = timed_step(&mut report, "optional_latency_metric", async {
        match wait_for_latency_metric(&mut latency_sub, order_id, 12_000).await? {
            Some(tags) => Ok((
                (),
                format!("received OMS latency metric tags={}", tags.len()),
            )),
            None => Ok((
                (),
                "no OMS latency metric batch observed for this order".to_string(),
            )),
        }
    })
    .await;

    report.summary = "OMS smoke completed".to_string();
    report
}

pub(crate) async fn run_sim_full_cycle(
    cli: &Cli,
    gw_id: &str,
    oms_id: &str,
    gw_addr: &Option<String>,
    oms_addr: &Option<String>,
    gw_admin_addr: &Option<String>,
) -> ScenarioReport {
    let mut report = ScenarioReport::new("sim-full-cycle");

    let nats = match timed_step(&mut report, "connect_nats", async {
        let nats = connect_nats(&cli.nats_url).await?;
        Ok((nats, format!("connected to {}", cli.nats_url)))
    })
    .await
    {
        Ok(v) => v,
        Err(e) => {
            report.fail(e.to_string());
            return report;
        }
    };

    let gw_addr = match timed_step(&mut report, "resolve_gateway", async {
        let addr = resolve_gw_addr(&nats, &cli.grpc_host, gw_id, gw_addr).await?;
        Ok((addr.clone(), format!("resolved {gw_id} to {addr}")))
    })
    .await
    {
        Ok(v) => v,
        Err(e) => {
            report.fail(e.to_string());
            return report;
        }
    };

    let oms_addr = match timed_step(&mut report, "resolve_oms", async {
        let addr = resolve_oms_addr(&nats, &cli.grpc_host, oms_id, oms_addr).await?;
        Ok((addr.clone(), format!("resolved {oms_id} to {addr}")))
    })
    .await
    {
        Ok(v) => v,
        Err(e) => {
            report.fail(e.to_string());
            return report;
        }
    };

    let admin_addr = gw_admin_addr
        .clone()
        .unwrap_or_else(|| format!("http://{}:51052", cli.grpc_host));
    report.observe("gw_addr", gw_addr.clone());
    report.observe("oms_addr", oms_addr.clone());
    report.observe("gw_admin_addr", admin_addr.clone());

    let oms_channel = match connect_channel(&oms_addr).await {
        Ok(c) => c,
        Err(e) => {
            report.fail(e.to_string());
            return report;
        }
    };
    let admin_channel = match connect_channel(&admin_addr).await {
        Ok(c) => c,
        Err(e) => {
            report.fail(e.to_string());
            return report;
        }
    };

    let mut oms_client = OmsServiceClient::new(oms_channel);
    let mut admin_client = GatewaySimulatorAdminServiceClient::new(admin_channel);

    let mut oms_order_sub = nats
        .subscribe(subject::oms_order_update(oms_id, cli.account_id))
        .await
        .expect("OMS order update subscription failed");
    let mut oms_balance_sub = nats
        .subscribe(subject::oms_balance_update(oms_id))
        .await
        .expect("OMS balance subscription failed");

    let order_id = next_order_id();

    let _ = timed_step(&mut report, "place_order_via_oms", async {
        let resp = oms_client
            .place_order(tonic::Request::new(make_oms_place(cli, order_id)))
            .await
            .map_err(|e| anyhow!("OMS place_order failed: {e}"))?
            .into_inner();
        if !oms_response_ok(&resp) {
            bail!("OMS place_order returned failure: {}", resp.message);
        }
        Ok(((), "OMS accepted order".to_string()))
    })
    .await;

    let _ = match timed_step(&mut report, "wait_booked_update", async {
        let event =
            wait_for_oms_order_update(&mut oms_order_sub, order_id, None, cli.timeout_ms).await?;
        let status = event
            .order_snapshot
            .as_ref()
            .and_then(|o| OrderStatus::try_from(o.order_status).ok())
            .map(|s| s.as_str_name().to_string())
            .unwrap_or_else(|| "UNKNOWN".to_string());
        Ok(((), format!("received initial OMS update status={status}")))
    })
    .await
    {
        Ok(v) => v,
        Err(e) => {
            report.fail(e.to_string());
            return report;
        }
    };

    let _ = timed_step(&mut report, "force_match", async {
        let req = ForceMatchRequest {
            order_id,
            fill_mode: FillMode::Full as i32,
            fill_qty: 0.0,
            fill_price: cli.price,
            publish_balance_update: true,
            ..Default::default()
        };
        let resp = admin_client
            .force_match(tonic::Request::new(req))
            .await
            .map_err(|e| anyhow!("gateway admin force_match failed: {e}"))?
            .into_inner();
        if !resp.accepted {
            bail!("force_match was not accepted: {}", resp.message);
        }
        Ok((
            (),
            format!("force matched order_id={}", resp.matched_order_id),
        ))
    })
    .await;

    let _ = match timed_step(&mut report, "wait_filled_update", async {
        let event = wait_for_oms_order_update(
            &mut oms_order_sub,
            order_id,
            Some(OrderStatus::Filled),
            cli.timeout_ms,
        )
        .await?;
        Ok(((), format!("received filled update ts={}", event.timestamp)))
    })
    .await
    {
        Ok(v) => v,
        Err(e) => {
            report.fail(e.to_string());
            return report;
        }
    };

    let _ = timed_step(&mut report, "wait_balance_update", async {
        let update = wait_for_any_oms_balance(&mut oms_balance_sub, cli.timeout_ms).await?;
        Ok((
            (),
            format!(
                "received OMS balance update with {} balances",
                update.balance_snapshots.len()
            ),
        ))
    })
    .await;

    let _ = timed_step(&mut report, "query_open_orders_after_fill", async {
        let resp = oms_client
            .query_open_orders(tonic::Request::new(QueryOpenOrderRequest {
                account_id: cli.account_id,
                query_gw: false,
                pagination: None,
            }))
            .await
            .map_err(|e| anyhow!("OMS query_open_orders failed: {e}"))?
            .into_inner();
        Ok(((), format!("open orders after fill: {}", resp.orders.len())))
    })
    .await;

    report.summary = "sim full-cycle completed".to_string();
    report
}
