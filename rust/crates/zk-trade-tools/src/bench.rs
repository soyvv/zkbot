use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use futures::{future::BoxFuture, stream::FuturesUnordered, StreamExt};
use tonic::transport::Channel;

use zk_infra_rs::nats::subject;
use zk_oms_svc::proto::gw_svc::gateway_service_client::GatewayServiceClient;
use zk_oms_svc::proto::oms_svc::oms_service_client::OmsServiceClient;

use crate::cli::Cli;
use crate::report::{
    GatewayBenchRun, GatewayBenchmarkReport, OmsBenchRun, OmsBenchmarkReport, ThresholdSummary,
};
use crate::support::{
    compute_percentiles, connect_channel, connect_nats, gateway_response_ok, next_order_id,
    oms_response_ok, resolve_gw_addr, resolve_oms_addr, GatewayReportTracker, GwWorkloadGenerator,
    OmsMetricObservation, OmsMetricTracker, OmsOrderUpdateTracker, OmsWorkloadGenerator,
};

enum AckResult {
    Success(f64),
    Failure,
    Timeout,
}

struct RequestOutcome {
    in_measurement: bool,
    ack_result: AckResult,
    report_latency_ms: Option<f64>,
}

struct OmsRequestOutcome {
    in_measurement: bool,
    ack_result: AckResult,
    oms_through_latency_ms: Option<f64>,
    writer_queue_latency_ms: Option<f64>,
    writer_core_latency_ms: Option<f64>,
    writer_post_core_latency_ms: Option<f64>,
    gw_exec_queue_latency_ms: Option<f64>,
    order_update_latency_ms: Option<f64>,
    metric_latency_ms: Option<f64>,
}

async fn run_place_request(
    channel: Channel,
    request: zk_proto_rs::zk::gateway::v1::SendOrderRequest,
    order_id: i64,
    scheduled_at: Instant,
    in_measurement: bool,
    timeout_ms: u64,
    report_rx: Option<tokio::sync::oneshot::Receiver<Instant>>,
    report_tracker: Option<std::sync::Arc<GatewayReportTracker>>,
) -> RequestOutcome {
    let timeout = Duration::from_millis(timeout_ms);
    let mut client = GatewayServiceClient::new(channel);
    let ack = match tokio::time::timeout(timeout, client.place_order(tonic::Request::new(request)))
        .await
    {
        Err(_) => AckResult::Timeout,
        Ok(Err(_)) => AckResult::Failure,
        Ok(Ok(resp)) => {
            let resp = resp.into_inner();
            if gateway_response_ok(&resp) {
                AckResult::Success(scheduled_at.elapsed().as_secs_f64() * 1_000.0)
            } else {
                AckResult::Failure
            }
        }
    };

    let report_latency_ms = if let AckResult::Success(_) = ack {
        if let Some(rx) = report_rx {
            match tokio::time::timeout(timeout, rx).await {
                Ok(Ok(received_at)) => {
                    Some(received_at.duration_since(scheduled_at).as_secs_f64() * 1_000.0)
                }
                _ => {
                    if let Some(tracker) = report_tracker {
                        tracker.clear(order_id).await;
                    }
                    None
                }
            }
        } else {
            None
        }
    } else {
        if let Some(tracker) = report_tracker {
            tracker.clear(order_id).await;
        }
        None
    };

    RequestOutcome {
        in_measurement,
        ack_result: ack,
        report_latency_ms,
    }
}

async fn run_oms_place_request(
    channel: Channel,
    request: zk_proto_rs::zk::oms::v1::PlaceOrderRequest,
    order_id: i64,
    scheduled_at: Instant,
    in_measurement: bool,
    timeout_ms: u64,
    order_update_rx: Option<tokio::sync::oneshot::Receiver<Instant>>,
    order_update_tracker: Option<std::sync::Arc<OmsOrderUpdateTracker>>,
    metric_rx: Option<tokio::sync::oneshot::Receiver<OmsMetricObservation>>,
    metric_tracker: Option<std::sync::Arc<OmsMetricTracker>>,
) -> OmsRequestOutcome {
    let timeout = Duration::from_millis(timeout_ms);
    let mut client = OmsServiceClient::new(channel);
    let ack = match tokio::time::timeout(timeout, client.place_order(tonic::Request::new(request)))
        .await
    {
        Err(_) => AckResult::Timeout,
        Ok(Err(_)) => AckResult::Failure,
        Ok(Ok(resp)) => {
            let resp = resp.into_inner();
            if oms_response_ok(&resp) {
                AckResult::Success(scheduled_at.elapsed().as_secs_f64() * 1_000.0)
            } else {
                AckResult::Failure
            }
        }
    };

    let order_update_latency_ms = if let AckResult::Success(_) = ack {
        if let Some(rx) = order_update_rx {
            match tokio::time::timeout(timeout, rx).await {
                Ok(Ok(received_at)) => {
                    Some(received_at.duration_since(scheduled_at).as_secs_f64() * 1_000.0)
                }
                _ => {
                    if let Some(tracker) = order_update_tracker {
                        tracker.clear(order_id).await;
                    }
                    None
                }
            }
        } else {
            None
        }
    } else {
        if let Some(tracker) = order_update_tracker {
            tracker.clear(order_id).await;
        }
        None
    };

    let (
        metric_latency_ms,
        oms_through_latency_ms,
        writer_queue_latency_ms,
        writer_core_latency_ms,
        writer_post_core_latency_ms,
        gw_exec_queue_latency_ms,
    ) = if let AckResult::Success(_) = ack {
        if let Some(rx) = metric_rx {
            match tokio::time::timeout(timeout, rx).await {
                Ok(Ok(observation)) => {
                    let t1 = observation.tagged_timestamps.get("t1").copied();
                    let t2 = observation.tagged_timestamps.get("t2").copied();
                    let tw_core = observation.tagged_timestamps.get("tw_core").copied();
                    let tw_dispatch = observation.tagged_timestamps.get("tw_dispatch").copied();
                    let t3 = observation.tagged_timestamps.get("t3").copied();
                    let oms_through_latency_ms = match (t1, t3) {
                        (Some(t1), Some(t3)) if t1 > 0 && t3 >= t1 => {
                            Some((t3 - t1) as f64 / 1_000_000.0)
                        }
                        _ => None,
                    };
                    let writer_queue_latency_ms = match (t1, t2) {
                        (Some(t1), Some(t2)) if t1 > 0 && t2 >= t1 => {
                            Some((t2 - t1) as f64 / 1_000_000.0)
                        }
                        _ => None,
                    };
                    let writer_core_latency_ms = match (t2, tw_core) {
                        (Some(t2), Some(tw_core)) if t2 > 0 && tw_core >= t2 => {
                            Some((tw_core - t2) as f64 / 1_000_000.0)
                        }
                        _ => None,
                    };
                    let writer_post_core_latency_ms = match (tw_core, tw_dispatch) {
                        (Some(tw_core), Some(tw_dispatch))
                            if tw_core > 0 && tw_dispatch >= tw_core =>
                        {
                            Some((tw_dispatch - tw_core) as f64 / 1_000_000.0)
                        }
                        _ => None,
                    };
                    let gw_exec_queue_latency_ms = match (tw_dispatch, t3) {
                        (Some(tw_dispatch), Some(t3)) if tw_dispatch > 0 && t3 >= tw_dispatch => {
                            Some((t3 - tw_dispatch) as f64 / 1_000_000.0)
                        }
                        _ => None,
                    };
                    (
                        Some(
                            observation
                                .received_at
                                .duration_since(scheduled_at)
                                .as_secs_f64()
                                * 1_000.0,
                        ),
                        oms_through_latency_ms,
                        writer_queue_latency_ms,
                        writer_core_latency_ms,
                        writer_post_core_latency_ms,
                        gw_exec_queue_latency_ms,
                    )
                }
                _ => {
                    if let Some(tracker) = metric_tracker {
                        tracker.clear(order_id).await;
                    }
                    (None, None, None, None, None, None)
                }
            }
        } else {
            (None, None, None, None, None, None)
        }
    } else {
        if let Some(tracker) = metric_tracker {
            tracker.clear(order_id).await;
        }
        (None, None, None, None, None, None)
    };

    OmsRequestOutcome {
        in_measurement,
        ack_result: ack,
        oms_through_latency_ms,
        writer_queue_latency_ms,
        writer_core_latency_ms,
        writer_post_core_latency_ms,
        gw_exec_queue_latency_ms,
        order_update_latency_ms,
        metric_latency_ms,
    }
}

fn summarize_thresholds<T, F>(runs: &[T], accessor: F) -> Vec<ThresholdSummary>
where
    F: Fn(&T) -> (u64, u64, f64, f64),
{
    [1.0, 5.0, 20.0]
        .into_iter()
        .map(|threshold_ms| ThresholdSummary {
            threshold_ms,
            highest_qps: runs
                .iter()
                .filter(|run| {
                    let (failures, timeouts, p99_ms, _target_qps) = accessor(run);
                    failures == 0 && timeouts == 0 && p99_ms < threshold_ms
                })
                .map(|run| accessor(run).3)
                .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)),
        })
        .collect()
}

fn bench_phase_label(now: Instant, measure_start: Instant, measure_end: Instant) -> &'static str {
    if now < measure_start {
        "warmup"
    } else if now < measure_end {
        "measure"
    } else {
        "drain"
    }
}

fn log_progress(
    scope: &str,
    target_qps: f64,
    started_at: Instant,
    now: Instant,
    measure_start: Instant,
    measure_end: Instant,
    sent_total: u64,
    measured_total: u64,
    completed_success: u64,
    failures: u64,
    timeouts: u64,
    in_flight: u64,
) {
    let phase = bench_phase_label(now, measure_start, measure_end);
    eprintln!(
        "[{scope}] target={target_qps:.0}/s phase={phase} elapsed={:.1}s sent={} measured={} ok={} fail={} timeout={} inflight={}",
        started_at.elapsed().as_secs_f64(),
        sent_total,
        measured_total,
        completed_success,
        failures,
        timeouts,
        in_flight
    );
}

async fn run_gw_latency_point(
    cli: &Cli,
    channel: &Channel,
    report_tracker: Option<std::sync::Arc<GatewayReportTracker>>,
    target_qps: f64,
    warmup_s: f64,
    measure_s: f64,
    mode: &str,
    generator: &mut GwWorkloadGenerator,
) -> Result<GatewayBenchRun> {
    let interval = Duration::from_secs_f64(1.0 / target_qps);
    let start = Instant::now() + Duration::from_millis(50);
    let started_at = Instant::now();
    let measure_start = start + Duration::from_secs_f64(warmup_s);
    let measure_end = measure_start + Duration::from_secs_f64(measure_s);
    let mut next_send = start;
    let mut next_order_id = next_order_id();
    let mut current_in_flight = 0_u64;
    let mut peak_in_flight = 0_u64;
    let mut end_of_run_in_flight = 0_u64;
    let mut measurement_closed = false;
    let mut total_requests = 0_u64;
    let mut failure_count = 0_u64;
    let mut timeout_count = 0_u64;
    let mut ack_latencies = Vec::new();
    let mut report_latencies = Vec::new();
    let mut pending: FuturesUnordered<BoxFuture<'static, RequestOutcome>> = FuturesUnordered::new();
    let mut sent_total = 0_u64;
    let mut last_progress = started_at;

    eprintln!(
        "[gw-bench] start target={target_qps:.0}/s warmup={warmup_s:.1}s measure={measure_s:.1}s mode={mode}"
    );

    loop {
        let now = Instant::now();
        if !measurement_closed && now >= measure_end {
            end_of_run_in_flight = current_in_flight;
            measurement_closed = true;
        }

        while next_send <= now && next_send < measure_end {
            let in_measurement = next_send >= measure_start;
            if in_measurement {
                total_requests += 1;
            }
            sent_total += 1;
            let order_id = next_order_id;
            next_order_id += 1;
            let request = generator.next_request(order_id);
            let report_rx = if let Some(tracker) = report_tracker.as_ref() {
                Some(tracker.register(order_id).await)
            } else {
                None
            };
            pending.push(Box::pin(run_place_request(
                channel.clone(),
                request,
                order_id,
                next_send,
                in_measurement,
                cli.timeout_ms,
                report_rx,
                report_tracker.clone(),
            )));
            current_in_flight += 1;
            peak_in_flight = peak_in_flight.max(current_in_flight);
            next_send += interval;
        }

        if next_send >= measure_end && pending.is_empty() {
            if !measurement_closed {
                end_of_run_in_flight = 0;
            }
            break;
        }

        tokio::select! {
            outcome = pending.next(), if !pending.is_empty() => {
                if let Some(outcome) = outcome {
                    current_in_flight = current_in_flight.saturating_sub(1);
                    if outcome.in_measurement {
                        match outcome.ack_result {
                            AckResult::Success(latency_ms) => ack_latencies.push(latency_ms),
                            AckResult::Failure => failure_count += 1,
                            AckResult::Timeout => timeout_count += 1,
                        }
                        if let Some(latency_ms) = outcome.report_latency_ms {
                            report_latencies.push(latency_ms);
                        }
                    }
                }
            }
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(next_send)), if next_send < measure_end => {}
        }

        let now = Instant::now();
        if now.duration_since(last_progress) >= Duration::from_secs(1) {
            log_progress(
                "gw-bench",
                target_qps,
                started_at,
                now,
                measure_start,
                measure_end,
                sent_total,
                total_requests,
                ack_latencies.len() as u64,
                failure_count,
                timeout_count,
                current_in_flight,
            );
            last_progress = now;
        }
    }

    if ack_latencies.is_empty() {
        bail!("no successful requests completed during measurement window at {target_qps}/s");
    }

    let success_count = ack_latencies.len() as u64;
    let achieved_qps = success_count as f64 / measure_s;
    let error_rate = if total_requests == 0 {
        0.0
    } else {
        (failure_count + timeout_count) as f64 / total_requests as f64
    };

    let mut degradation_reasons = Vec::new();
    if failure_count > 0 {
        degradation_reasons.push(format!("{failure_count} request failures"));
    }
    if timeout_count > 0 {
        degradation_reasons.push(format!("{timeout_count} request timeouts"));
    }
    if achieved_qps < target_qps * 0.95 {
        degradation_reasons.push(format!(
            "achieved_qps {:.1} below target {:.1}",
            achieved_qps, target_qps
        ));
    }
    let backlog_limit = (target_qps * 0.10).ceil().max(10.0) as u64;
    if end_of_run_in_flight > backlog_limit {
        degradation_reasons.push(format!(
            "end_of_run_in_flight {} exceeds backlog limit {}",
            end_of_run_in_flight, backlog_limit
        ));
    }

    let run = GatewayBenchRun {
        mode: mode.to_string(),
        target_qps,
        achieved_qps,
        total_requests,
        success_count,
        failure_count,
        timeout_count,
        peak_in_flight,
        end_of_run_in_flight,
        error_rate,
        degraded: !degradation_reasons.is_empty(),
        degradation_reasons,
        ack_latency_ms: compute_percentiles(ack_latencies),
        report_latency_ms: if report_latencies.is_empty() {
            None
        } else {
            Some(compute_percentiles(report_latencies))
        },
    };
    eprintln!(
        "[gw-bench] done target={target_qps:.0}/s achieved={:.1}/s ok={} fail={} timeout={} peak_inflight={}",
        run.achieved_qps,
        run.success_count,
        run.failure_count,
        run.timeout_count,
        run.peak_in_flight
    );
    Ok(run)
}

pub(crate) async fn run_gw_latency_bench(
    cli: &Cli,
    gw_id: &str,
    gw_addr_override: &Option<String>,
    rates: &[f64],
    warmup_s: f64,
    measure_s: f64,
    mode: &str,
    track_report_latency: bool,
    instrument_pool: &[String],
    exch_account_pool: &[String],
) -> GatewayBenchmarkReport {
    let mut notes = Vec::new();
    eprintln!(
        "[gw-bench] preparing benchmark gw_id={} points={} rates={:?}",
        gw_id,
        rates.len(),
        rates
    );
    let nats = if gw_addr_override.is_none() || track_report_latency {
        match connect_nats(&cli.nats_url).await {
            Ok(client) => Some(client),
            Err(err) => {
                return GatewayBenchmarkReport {
                    scenario: "gw-latency-bench".to_string(),
                    success: false,
                    summary: err.to_string(),
                    gw_id: gw_id.to_string(),
                    gw_addr: gw_addr_override.clone().unwrap_or_default(),
                    mode: mode.to_string(),
                    scheduling: "open_loop_fixed_interval".to_string(),
                    warmup_s,
                    measure_s,
                    timeout_ms: cli.timeout_ms,
                    track_report_latency,
                    runs: Vec::new(),
                    threshold_views: Vec::new(),
                    highest_zero_error_qps: None,
                    highest_steady_qps: None,
                    notes,
                };
            }
        }
    } else {
        None
    };

    let gw_addr = if let Some(addr) = gw_addr_override {
        addr.clone()
    } else {
        let Some(nats) = nats.as_ref() else {
            return GatewayBenchmarkReport {
                scenario: "gw-latency-bench".to_string(),
                success: false,
                summary: "NATS connection required to resolve gateway address".to_string(),
                gw_id: gw_id.to_string(),
                gw_addr: String::new(),
                mode: mode.to_string(),
                scheduling: "open_loop_fixed_interval".to_string(),
                warmup_s,
                measure_s,
                timeout_ms: cli.timeout_ms,
                track_report_latency,
                runs: Vec::new(),
                threshold_views: Vec::new(),
                highest_zero_error_qps: None,
                highest_steady_qps: None,
                notes,
            };
        };
        match resolve_gw_addr(nats, &cli.grpc_host, gw_id, gw_addr_override).await {
            Ok(addr) => addr,
            Err(err) => {
                return GatewayBenchmarkReport {
                    scenario: "gw-latency-bench".to_string(),
                    success: false,
                    summary: err.to_string(),
                    gw_id: gw_id.to_string(),
                    gw_addr: String::new(),
                    mode: mode.to_string(),
                    scheduling: "open_loop_fixed_interval".to_string(),
                    warmup_s,
                    measure_s,
                    timeout_ms: cli.timeout_ms,
                    track_report_latency,
                    runs: Vec::new(),
                    threshold_views: Vec::new(),
                    highest_zero_error_qps: None,
                    highest_steady_qps: None,
                    notes,
                };
            }
        }
    };

    let channel = match connect_channel(&gw_addr).await {
        Ok(channel) => channel,
        Err(err) => {
            return GatewayBenchmarkReport {
                scenario: "gw-latency-bench".to_string(),
                success: false,
                summary: err.to_string(),
                gw_id: gw_id.to_string(),
                gw_addr,
                mode: mode.to_string(),
                scheduling: "open_loop_fixed_interval".to_string(),
                warmup_s,
                measure_s,
                timeout_ms: cli.timeout_ms,
                track_report_latency,
                runs: Vec::new(),
                threshold_views: Vec::new(),
                highest_zero_error_qps: None,
                highest_steady_qps: None,
                notes,
            };
        }
    };

    let report_tracker = if track_report_latency {
        match nats.as_ref() {
            Some(nats) => match nats.subscribe(subject::gw_report(gw_id)).await {
                Ok(sub) => Some(std::sync::Arc::new(GatewayReportTracker::new(sub))),
                Err(err) => {
                    notes.push(format!(
                        "failed to subscribe to gateway report subject: {err}"
                    ));
                    None
                }
            },
            None => {
                notes.push("report latency tracking requested but NATS is unavailable".to_string());
                None
            }
        }
    } else {
        None
    };

    let mut runs = Vec::new();
    let mut generator =
        GwWorkloadGenerator::new(cli, instrument_pool.to_vec(), exch_account_pool.to_vec());
    for rate in rates {
        eprintln!("[gw-bench] running point target={rate:.0}/s");
        match run_gw_latency_point(
            cli,
            &channel,
            report_tracker.clone(),
            *rate,
            warmup_s,
            measure_s,
            mode,
            &mut generator,
        )
        .await
        {
            Ok(run) => runs.push(run),
            Err(err) => {
                if let Some(tracker) =
                    report_tracker.and_then(|t| std::sync::Arc::try_unwrap(t).ok())
                {
                    tracker.shutdown();
                }
                return GatewayBenchmarkReport {
                    scenario: "gw-latency-bench".to_string(),
                    success: false,
                    summary: err.to_string(),
                    gw_id: gw_id.to_string(),
                    gw_addr,
                    mode: mode.to_string(),
                    scheduling: "open_loop_fixed_interval".to_string(),
                    warmup_s,
                    measure_s,
                    timeout_ms: cli.timeout_ms,
                    track_report_latency,
                    runs,
                    threshold_views: Vec::new(),
                    highest_zero_error_qps: None,
                    highest_steady_qps: None,
                    notes,
                };
            }
        }
    }

    if let Some(tracker) = report_tracker.and_then(|t| std::sync::Arc::try_unwrap(t).ok()) {
        tracker.shutdown();
    }

    let threshold_views = summarize_thresholds(&runs, |run: &GatewayBenchRun| {
        (
            run.failure_count,
            run.timeout_count,
            run.ack_latency_ms.p99_ms,
            run.target_qps,
        )
    });
    let highest_zero_error_qps = runs
        .iter()
        .filter(|run| run.failure_count == 0 && run.timeout_count == 0)
        .map(|run| run.target_qps)
        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let highest_steady_qps = runs
        .iter()
        .filter(|run| !run.degraded)
        .map(|run| run.target_qps)
        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    if instrument_pool.is_empty() {
        notes.push("instrument variation defaults to the single --instrument value".to_string());
    }
    if exch_account_pool.is_empty() {
        notes.push("account variation defaults to one exchange account unless --exch-account-pool is provided".to_string());
    }

    GatewayBenchmarkReport {
        scenario: "gw-latency-bench".to_string(),
        success: true,
        summary: format!("completed {} benchmark points", runs.len()),
        gw_id: gw_id.to_string(),
        gw_addr,
        mode: mode.to_string(),
        scheduling: "open_loop_fixed_interval".to_string(),
        warmup_s,
        measure_s,
        timeout_ms: cli.timeout_ms,
        track_report_latency,
        runs,
        threshold_views,
        highest_zero_error_qps,
        highest_steady_qps,
        notes,
    }
}

async fn run_oms_latency_point(
    cli: &Cli,
    channel: &Channel,
    order_update_tracker: Option<std::sync::Arc<OmsOrderUpdateTracker>>,
    metric_tracker: Option<std::sync::Arc<OmsMetricTracker>>,
    target_qps: f64,
    warmup_s: f64,
    measure_s: f64,
    mode: &str,
    generator: &mut OmsWorkloadGenerator,
) -> Result<OmsBenchRun> {
    let interval = Duration::from_secs_f64(1.0 / target_qps);
    let start = Instant::now() + Duration::from_millis(50);
    let started_at = Instant::now();
    let measure_start = start + Duration::from_secs_f64(warmup_s);
    let measure_end = measure_start + Duration::from_secs_f64(measure_s);
    let mut next_send = start;
    let mut next_order_id = next_order_id();
    let mut current_in_flight = 0_u64;
    let mut peak_in_flight = 0_u64;
    let mut end_of_run_in_flight = 0_u64;
    let mut measurement_closed = false;
    let mut total_requests = 0_u64;
    let mut failure_count = 0_u64;
    let mut timeout_count = 0_u64;
    let mut ack_latencies = Vec::new();
    let mut oms_through_latencies = Vec::new();
    let mut writer_queue_latencies = Vec::new();
    let mut writer_core_latencies = Vec::new();
    let mut writer_post_core_latencies = Vec::new();
    let mut gw_exec_queue_latencies = Vec::new();
    let mut order_update_latencies = Vec::new();
    let mut metric_latencies = Vec::new();
    let mut pending: FuturesUnordered<BoxFuture<'static, OmsRequestOutcome>> =
        FuturesUnordered::new();
    let mut sent_total = 0_u64;
    let mut last_progress = started_at;

    eprintln!(
        "[oms-bench] start target={target_qps:.0}/s warmup={warmup_s:.1}s measure={measure_s:.1}s mode={mode}"
    );

    loop {
        let now = Instant::now();
        if !measurement_closed && now >= measure_end {
            end_of_run_in_flight = current_in_flight;
            measurement_closed = true;
        }

        while next_send <= now && next_send < measure_end {
            let in_measurement = next_send >= measure_start;
            if in_measurement {
                total_requests += 1;
            }
            sent_total += 1;
            let order_id = next_order_id;
            next_order_id += 1;
            let request = generator.next_request(order_id);
            let order_update_rx = if let Some(tracker) = order_update_tracker.as_ref() {
                Some(tracker.register(order_id).await)
            } else {
                None
            };
            let metric_rx = if let Some(tracker) = metric_tracker.as_ref() {
                Some(tracker.register(order_id).await)
            } else {
                None
            };
            pending.push(Box::pin(run_oms_place_request(
                channel.clone(),
                request,
                order_id,
                next_send,
                in_measurement,
                cli.timeout_ms,
                order_update_rx,
                order_update_tracker.clone(),
                metric_rx,
                metric_tracker.clone(),
            )));
            current_in_flight += 1;
            peak_in_flight = peak_in_flight.max(current_in_flight);
            next_send += interval;
        }

        if next_send >= measure_end && pending.is_empty() {
            if !measurement_closed {
                end_of_run_in_flight = 0;
            }
            break;
        }

        tokio::select! {
            outcome = pending.next(), if !pending.is_empty() => {
                if let Some(outcome) = outcome {
                    current_in_flight = current_in_flight.saturating_sub(1);
                    if outcome.in_measurement {
                        match outcome.ack_result {
                            AckResult::Success(latency_ms) => ack_latencies.push(latency_ms),
                            AckResult::Failure => failure_count += 1,
                            AckResult::Timeout => timeout_count += 1,
                        }
                        if let Some(latency_ms) = outcome.oms_through_latency_ms {
                            oms_through_latencies.push(latency_ms);
                        }
                        if let Some(latency_ms) = outcome.writer_queue_latency_ms {
                            writer_queue_latencies.push(latency_ms);
                        }
                        if let Some(latency_ms) = outcome.writer_core_latency_ms {
                            writer_core_latencies.push(latency_ms);
                        }
                        if let Some(latency_ms) = outcome.writer_post_core_latency_ms {
                            writer_post_core_latencies.push(latency_ms);
                        }
                        if let Some(latency_ms) = outcome.gw_exec_queue_latency_ms {
                            gw_exec_queue_latencies.push(latency_ms);
                        }
                        if let Some(latency_ms) = outcome.order_update_latency_ms {
                            order_update_latencies.push(latency_ms);
                        }
                        if let Some(latency_ms) = outcome.metric_latency_ms {
                            metric_latencies.push(latency_ms);
                        }
                    }
                }
            }
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(next_send)), if next_send < measure_end => {}
        }

        let now = Instant::now();
        if now.duration_since(last_progress) >= Duration::from_secs(1) {
            log_progress(
                "oms-bench",
                target_qps,
                started_at,
                now,
                measure_start,
                measure_end,
                sent_total,
                total_requests,
                ack_latencies.len() as u64,
                failure_count,
                timeout_count,
                current_in_flight,
            );
            last_progress = now;
        }
    }

    if ack_latencies.is_empty() {
        bail!("no successful requests completed during measurement window at {target_qps}/s");
    }

    let success_count = ack_latencies.len() as u64;
    let achieved_qps = success_count as f64 / measure_s;
    let error_rate = if total_requests == 0 {
        0.0
    } else {
        (failure_count + timeout_count) as f64 / total_requests as f64
    };

    let mut degradation_reasons = Vec::new();
    if failure_count > 0 {
        degradation_reasons.push(format!("{failure_count} request failures"));
    }
    if timeout_count > 0 {
        degradation_reasons.push(format!("{timeout_count} request timeouts"));
    }
    if achieved_qps < target_qps * 0.95 {
        degradation_reasons.push(format!(
            "achieved_qps {:.1} below target {:.1}",
            achieved_qps, target_qps
        ));
    }
    let backlog_limit = (target_qps * 0.10).ceil().max(10.0) as u64;
    if end_of_run_in_flight > backlog_limit {
        degradation_reasons.push(format!(
            "end_of_run_in_flight {} exceeds backlog limit {}",
            end_of_run_in_flight, backlog_limit
        ));
    }

    let run = OmsBenchRun {
        mode: mode.to_string(),
        target_qps,
        achieved_qps,
        total_requests,
        success_count,
        failure_count,
        timeout_count,
        peak_in_flight,
        end_of_run_in_flight,
        error_rate,
        degraded: !degradation_reasons.is_empty(),
        degradation_reasons,
        ack_latency_ms: compute_percentiles(ack_latencies),
        oms_through_latency_ms: if oms_through_latencies.is_empty() {
            None
        } else {
            Some(compute_percentiles(oms_through_latencies))
        },
        writer_queue_latency_ms: if writer_queue_latencies.is_empty() {
            None
        } else {
            Some(compute_percentiles(writer_queue_latencies))
        },
        writer_core_latency_ms: if writer_core_latencies.is_empty() {
            None
        } else {
            Some(compute_percentiles(writer_core_latencies))
        },
        writer_post_core_latency_ms: if writer_post_core_latencies.is_empty() {
            None
        } else {
            Some(compute_percentiles(writer_post_core_latencies))
        },
        gw_exec_queue_latency_ms: if gw_exec_queue_latencies.is_empty() {
            None
        } else {
            Some(compute_percentiles(gw_exec_queue_latencies))
        },
        order_update_latency_ms: if order_update_latencies.is_empty() {
            None
        } else {
            Some(compute_percentiles(order_update_latencies))
        },
        metric_latency_ms: if metric_latencies.is_empty() {
            None
        } else {
            Some(compute_percentiles(metric_latencies))
        },
    };
    eprintln!(
        "[oms-bench] done target={target_qps:.0}/s achieved={:.1}/s ok={} fail={} timeout={} peak_inflight={}",
        run.achieved_qps,
        run.success_count,
        run.failure_count,
        run.timeout_count,
        run.peak_in_flight
    );
    Ok(run)
}

pub(crate) async fn run_oms_latency_bench(
    cli: &Cli,
    oms_id: &str,
    oms_addr_override: &Option<String>,
    rates: &[f64],
    warmup_s: f64,
    measure_s: f64,
    mode: &str,
    track_order_update_latency: bool,
    track_metric_latency: bool,
    instrument_pool: &[String],
) -> OmsBenchmarkReport {
    let mut notes = Vec::new();
    eprintln!(
        "[oms-bench] preparing benchmark oms_id={} points={} rates={:?}",
        oms_id,
        rates.len(),
        rates
    );
    let nats = if oms_addr_override.is_none() || track_order_update_latency || track_metric_latency
    {
        match connect_nats(&cli.nats_url).await {
            Ok(client) => Some(client),
            Err(err) => {
                return OmsBenchmarkReport {
                    scenario: "oms-latency-bench".to_string(),
                    success: false,
                    summary: err.to_string(),
                    oms_id: oms_id.to_string(),
                    oms_addr: oms_addr_override.clone().unwrap_or_default(),
                    mode: mode.to_string(),
                    scheduling: "open_loop_fixed_interval".to_string(),
                    warmup_s,
                    measure_s,
                    timeout_ms: cli.timeout_ms,
                    track_order_update_latency,
                    track_metric_latency,
                    runs: Vec::new(),
                    threshold_views: Vec::new(),
                    highest_zero_error_qps: None,
                    highest_steady_qps: None,
                    notes,
                };
            }
        }
    } else {
        None
    };

    let oms_addr = if let Some(addr) = oms_addr_override {
        addr.clone()
    } else {
        let Some(nats) = nats.as_ref() else {
            return OmsBenchmarkReport {
                scenario: "oms-latency-bench".to_string(),
                success: false,
                summary: "NATS connection required to resolve OMS address".to_string(),
                oms_id: oms_id.to_string(),
                oms_addr: String::new(),
                mode: mode.to_string(),
                scheduling: "open_loop_fixed_interval".to_string(),
                warmup_s,
                measure_s,
                timeout_ms: cli.timeout_ms,
                track_order_update_latency,
                track_metric_latency,
                runs: Vec::new(),
                threshold_views: Vec::new(),
                highest_zero_error_qps: None,
                highest_steady_qps: None,
                notes,
            };
        };
        match resolve_oms_addr(nats, &cli.grpc_host, oms_id, oms_addr_override).await {
            Ok(addr) => addr,
            Err(err) => {
                return OmsBenchmarkReport {
                    scenario: "oms-latency-bench".to_string(),
                    success: false,
                    summary: err.to_string(),
                    oms_id: oms_id.to_string(),
                    oms_addr: String::new(),
                    mode: mode.to_string(),
                    scheduling: "open_loop_fixed_interval".to_string(),
                    warmup_s,
                    measure_s,
                    timeout_ms: cli.timeout_ms,
                    track_order_update_latency,
                    track_metric_latency,
                    runs: Vec::new(),
                    threshold_views: Vec::new(),
                    highest_zero_error_qps: None,
                    highest_steady_qps: None,
                    notes,
                };
            }
        }
    };

    let channel = match connect_channel(&oms_addr).await {
        Ok(channel) => channel,
        Err(err) => {
            return OmsBenchmarkReport {
                scenario: "oms-latency-bench".to_string(),
                success: false,
                summary: err.to_string(),
                oms_id: oms_id.to_string(),
                oms_addr,
                mode: mode.to_string(),
                scheduling: "open_loop_fixed_interval".to_string(),
                warmup_s,
                measure_s,
                timeout_ms: cli.timeout_ms,
                track_order_update_latency,
                track_metric_latency,
                runs: Vec::new(),
                threshold_views: Vec::new(),
                highest_zero_error_qps: None,
                highest_steady_qps: None,
                notes,
            };
        }
    };

    let order_update_tracker = if track_order_update_latency {
        match nats.as_ref() {
            Some(nats) => match nats
                .subscribe(subject::oms_order_update(oms_id, cli.account_id))
                .await
            {
                Ok(sub) => Some(std::sync::Arc::new(OmsOrderUpdateTracker::new(sub))),
                Err(err) => {
                    notes.push(format!(
                        "failed to subscribe to OMS order-update subject: {err}"
                    ));
                    None
                }
            },
            None => {
                notes.push(
                    "order-update latency tracking requested but NATS is unavailable".to_string(),
                );
                None
            }
        }
    } else {
        None
    };

    let metric_tracker = if track_metric_latency {
        match nats.as_ref() {
            Some(nats) => match nats
                .subscribe(format!("zk.oms.{oms_id}.metrics.latency"))
                .await
            {
                Ok(sub) => Some(std::sync::Arc::new(OmsMetricTracker::new(sub))),
                Err(err) => {
                    notes.push(format!(
                        "failed to subscribe to OMS latency-metric subject: {err}"
                    ));
                    None
                }
            },
            None => {
                notes.push("metric latency tracking requested but NATS is unavailable".to_string());
                None
            }
        }
    } else {
        None
    };

    let mut runs = Vec::new();
    let mut generator = OmsWorkloadGenerator::new(cli, instrument_pool.to_vec());
    for rate in rates {
        eprintln!("[oms-bench] running point target={rate:.0}/s");
        match run_oms_latency_point(
            cli,
            &channel,
            order_update_tracker.clone(),
            metric_tracker.clone(),
            *rate,
            warmup_s,
            measure_s,
            mode,
            &mut generator,
        )
        .await
        {
            Ok(run) => runs.push(run),
            Err(err) => {
                if let Some(tracker) =
                    order_update_tracker.and_then(|t| std::sync::Arc::try_unwrap(t).ok())
                {
                    tracker.shutdown();
                }
                if let Some(tracker) =
                    metric_tracker.and_then(|t| std::sync::Arc::try_unwrap(t).ok())
                {
                    tracker.shutdown();
                }
                return OmsBenchmarkReport {
                    scenario: "oms-latency-bench".to_string(),
                    success: false,
                    summary: err.to_string(),
                    oms_id: oms_id.to_string(),
                    oms_addr,
                    mode: mode.to_string(),
                    scheduling: "open_loop_fixed_interval".to_string(),
                    warmup_s,
                    measure_s,
                    timeout_ms: cli.timeout_ms,
                    track_order_update_latency,
                    track_metric_latency,
                    runs,
                    threshold_views: Vec::new(),
                    highest_zero_error_qps: None,
                    highest_steady_qps: None,
                    notes,
                };
            }
        }
    }

    if let Some(tracker) = order_update_tracker.and_then(|t| std::sync::Arc::try_unwrap(t).ok()) {
        tracker.shutdown();
    }
    if let Some(tracker) = metric_tracker.and_then(|t| std::sync::Arc::try_unwrap(t).ok()) {
        tracker.shutdown();
    }

    let threshold_views = summarize_thresholds(&runs, |run: &OmsBenchRun| {
        (
            run.failure_count,
            run.timeout_count,
            run.ack_latency_ms.p99_ms,
            run.target_qps,
        )
    });
    let highest_zero_error_qps = runs
        .iter()
        .filter(|run| run.failure_count == 0 && run.timeout_count == 0)
        .map(|run| run.target_qps)
        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let highest_steady_qps = runs
        .iter()
        .filter(|run| !run.degraded)
        .map(|run| run.target_qps)
        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    if instrument_pool.is_empty() {
        notes.push("instrument variation defaults to the single --instrument value".to_string());
    }
    if !track_metric_latency {
        notes.push(
            "oms_through latency was not collected because --track-metric-latency=false"
                .to_string(),
        );
    } else if runs.iter().all(|run| run.oms_through_latency_ms.is_none()) {
        notes.push(
            "OMS metric tracking was enabled but no usable latency metric samples were observed; check OMS metrics flush timing and NATS subscription health"
                .to_string(),
        );
    }

    OmsBenchmarkReport {
        scenario: "oms-latency-bench".to_string(),
        success: true,
        summary: format!("completed {} benchmark points", runs.len()),
        oms_id: oms_id.to_string(),
        oms_addr,
        mode: mode.to_string(),
        scheduling: "open_loop_fixed_interval".to_string(),
        warmup_s,
        measure_s,
        timeout_ms: cli.timeout_ms,
        track_order_update_latency,
        track_metric_latency,
        runs,
        threshold_views,
        highest_zero_error_qps,
        highest_steady_qps,
        notes,
    }
}
