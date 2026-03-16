use std::collections::HashMap;

use anyhow::Result;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub(crate) struct ScenarioReport {
    pub(crate) scenario: String,
    pub(crate) success: bool,
    pub(crate) summary: String,
    pub(crate) steps: Vec<StepReport>,
    pub(crate) notes: Vec<String>,
    pub(crate) observations: HashMap<String, String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct StepReport {
    pub(crate) name: String,
    pub(crate) ok: bool,
    pub(crate) duration_ms: u128,
    pub(crate) detail: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct PercentileSummary {
    pub(crate) avg_ms: f64,
    pub(crate) p50_ms: f64,
    pub(crate) p90_ms: f64,
    pub(crate) p99_ms: f64,
    pub(crate) p99_9_ms: f64,
    pub(crate) max_ms: f64,
}

#[derive(Debug, Serialize)]
pub(crate) struct ThresholdSummary {
    pub(crate) threshold_ms: f64,
    pub(crate) highest_qps: Option<f64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct GatewayBenchRun {
    pub(crate) mode: String,
    pub(crate) target_qps: f64,
    pub(crate) achieved_qps: f64,
    pub(crate) total_requests: u64,
    pub(crate) success_count: u64,
    pub(crate) failure_count: u64,
    pub(crate) timeout_count: u64,
    pub(crate) peak_in_flight: u64,
    pub(crate) end_of_run_in_flight: u64,
    pub(crate) error_rate: f64,
    pub(crate) degraded: bool,
    pub(crate) degradation_reasons: Vec<String>,
    pub(crate) ack_latency_ms: PercentileSummary,
    pub(crate) report_latency_ms: Option<PercentileSummary>,
}

#[derive(Debug, Serialize)]
pub(crate) struct GatewayBenchmarkReport {
    pub(crate) scenario: String,
    pub(crate) success: bool,
    pub(crate) summary: String,
    pub(crate) gw_id: String,
    pub(crate) gw_addr: String,
    pub(crate) mode: String,
    pub(crate) scheduling: String,
    pub(crate) warmup_s: f64,
    pub(crate) measure_s: f64,
    pub(crate) timeout_ms: u64,
    pub(crate) track_report_latency: bool,
    pub(crate) runs: Vec<GatewayBenchRun>,
    pub(crate) threshold_views: Vec<ThresholdSummary>,
    pub(crate) highest_zero_error_qps: Option<f64>,
    pub(crate) highest_steady_qps: Option<f64>,
    pub(crate) notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct OmsBenchRun {
    pub(crate) mode: String,
    pub(crate) target_qps: f64,
    pub(crate) achieved_qps: f64,
    pub(crate) total_requests: u64,
    pub(crate) success_count: u64,
    pub(crate) failure_count: u64,
    pub(crate) timeout_count: u64,
    pub(crate) peak_in_flight: u64,
    pub(crate) end_of_run_in_flight: u64,
    pub(crate) error_rate: f64,
    pub(crate) degraded: bool,
    pub(crate) degradation_reasons: Vec<String>,
    pub(crate) ack_latency_ms: PercentileSummary,
    pub(crate) oms_through_latency_ms: Option<PercentileSummary>,
    pub(crate) order_update_latency_ms: Option<PercentileSummary>,
    pub(crate) metric_latency_ms: Option<PercentileSummary>,
}

#[derive(Debug, Serialize)]
pub(crate) struct OmsBenchmarkReport {
    pub(crate) scenario: String,
    pub(crate) success: bool,
    pub(crate) summary: String,
    pub(crate) oms_id: String,
    pub(crate) oms_addr: String,
    pub(crate) mode: String,
    pub(crate) scheduling: String,
    pub(crate) warmup_s: f64,
    pub(crate) measure_s: f64,
    pub(crate) timeout_ms: u64,
    pub(crate) track_order_update_latency: bool,
    pub(crate) track_metric_latency: bool,
    pub(crate) runs: Vec<OmsBenchRun>,
    pub(crate) threshold_views: Vec<ThresholdSummary>,
    pub(crate) highest_zero_error_qps: Option<f64>,
    pub(crate) highest_steady_qps: Option<f64>,
    pub(crate) notes: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ToolOutput {
    Scenario(ScenarioReport),
    GatewayBenchmark(GatewayBenchmarkReport),
    OmsBenchmark(OmsBenchmarkReport),
}

impl ToolOutput {
    pub(crate) fn success(&self) -> bool {
        match self {
            Self::Scenario(report) => report.success,
            Self::GatewayBenchmark(report) => report.success,
            Self::OmsBenchmark(report) => report.success,
        }
    }
}

impl ScenarioReport {
    pub(crate) fn new(name: impl Into<String>) -> Self {
        Self {
            scenario: name.into(),
            success: true,
            summary: String::new(),
            steps: Vec::new(),
            notes: Vec::new(),
            observations: HashMap::new(),
        }
    }

    pub(crate) fn fail(&mut self, msg: impl Into<String>) {
        self.success = false;
        self.summary = msg.into();
    }

    pub(crate) fn note(&mut self, msg: impl Into<String>) {
        self.notes.push(msg.into());
    }

    pub(crate) fn observe(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.observations.insert(key.into(), value.into());
    }
}

fn print_scenario_report(report: &ScenarioReport) {
    println!("Scenario: {}", report.scenario);
    println!("Result:   {}", if report.success { "PASS" } else { "FAIL" });
    if !report.summary.is_empty() {
        println!("Summary:  {}", report.summary);
    }
    if !report.observations.is_empty() {
        println!();
        println!("Observations:");
        for (k, v) in &report.observations {
            println!("  {k}: {v}");
        }
    }
    println!();
    println!("Steps:");
    for step in &report.steps {
        println!(
            "  [{}] {} ({} ms) - {}",
            if step.ok { "ok" } else { "fail" },
            step.name,
            step.duration_ms,
            step.detail
        );
    }
    if !report.notes.is_empty() {
        println!();
        println!("Notes:");
        for note in &report.notes {
            println!("  - {note}");
        }
    }
}

fn print_gateway_benchmark_report(report: &GatewayBenchmarkReport) {
    println!("Scenario: {}", report.scenario);
    println!("Result:   {}", if report.success { "PASS" } else { "FAIL" });
    println!("Gateway:  {} ({})", report.gw_id, report.gw_addr);
    println!(
        "Config:   mode={} scheduling={} warmup={}s measure={}s timeout={}ms report_latency={}",
        report.mode,
        report.scheduling,
        report.warmup_s,
        report.measure_s,
        report.timeout_ms,
        report.track_report_latency
    );
    if !report.summary.is_empty() {
        println!("Summary:  {}", report.summary);
    }
    if report.runs.is_empty() {
        return;
    }

    println!();
    println!(
        "{:>8} {:>9} {:>9} {:>10} {:>8} {:>8} {:>8} {:>7} {:>7} {:>7} {:>7} {:>7}",
        "target",
        "achieved",
        "requests",
        "err_rate",
        "peak_if",
        "end_if",
        "p50",
        "p90",
        "p99",
        "p99.9",
        "max",
        "avg"
    );
    for run in &report.runs {
        println!(
            "{:>8.0} {:>9.1} {:>9} {:>9.2}% {:>8} {:>8} {:>8.3} {:>7.3} {:>7.3} {:>7.3} {:>7.3} {:>7.3}",
            run.target_qps,
            run.achieved_qps,
            run.total_requests,
            run.error_rate * 100.0,
            run.peak_in_flight,
            run.end_of_run_in_flight,
            run.ack_latency_ms.p50_ms,
            run.ack_latency_ms.p90_ms,
            run.ack_latency_ms.p99_ms,
            run.ack_latency_ms.p99_9_ms,
            run.ack_latency_ms.max_ms,
            run.ack_latency_ms.avg_ms
        );
        if run.degraded {
            println!("         degraded: {}", run.degradation_reasons.join("; "));
        }
    }

    if report.track_report_latency {
        println!();
        println!("Order-report latency:");
        for run in &report.runs {
            if let Some(latency) = &run.report_latency_ms {
                println!(
                    "  {:>8.0}/s p50={:.3} p90={:.3} p99={:.3} p99.9={:.3} max={:.3} avg={:.3}",
                    run.target_qps,
                    latency.p50_ms,
                    latency.p90_ms,
                    latency.p99_ms,
                    latency.p99_9_ms,
                    latency.max_ms,
                    latency.avg_ms
                );
            }
        }
    }

    println!();
    println!(
        "Thresholds: zero_error_qps={} steady_qps={}",
        report
            .highest_zero_error_qps
            .map(|v| format!("{v:.0}"))
            .unwrap_or_else(|| "n/a".to_string()),
        report
            .highest_steady_qps
            .map(|v| format!("{v:.0}"))
            .unwrap_or_else(|| "n/a".to_string())
    );
    for threshold in &report.threshold_views {
        println!(
            "  highest_qps with p99 < {:>4.1} ms: {}",
            threshold.threshold_ms,
            threshold
                .highest_qps
                .map(|v| format!("{v:.0}"))
                .unwrap_or_else(|| "n/a".to_string())
        );
    }
    if !report.notes.is_empty() {
        println!();
        println!("Notes:");
        for note in &report.notes {
            println!("  - {note}");
        }
    }
}

fn print_oms_benchmark_report(report: &OmsBenchmarkReport) {
    println!("Scenario: {}", report.scenario);
    println!("Result:   {}", if report.success { "PASS" } else { "FAIL" });
    println!("OMS:      {} ({})", report.oms_id, report.oms_addr);
    println!(
        "Config:   mode={} scheduling={} warmup={}s measure={}s timeout={}ms order_update_latency={} metric_latency={}",
        report.mode,
        report.scheduling,
        report.warmup_s,
        report.measure_s,
        report.timeout_ms,
        report.track_order_update_latency,
        report.track_metric_latency
    );
    if !report.summary.is_empty() {
        println!("Summary:  {}", report.summary);
    }
    if report.runs.is_empty() {
        return;
    }

    println!();
    println!(
        "{:>8} {:>9} {:>9} {:>10} {:>8} {:>8} {:>8} {:>7} {:>7} {:>7} {:>7} {:>7}",
        "target",
        "achieved",
        "requests",
        "err_rate",
        "peak_if",
        "end_if",
        "p50",
        "p90",
        "p99",
        "p99.9",
        "max",
        "avg"
    );
    for run in &report.runs {
        println!(
            "{:>8.0} {:>9.1} {:>9} {:>9.2}% {:>8} {:>8} {:>8.3} {:>7.3} {:>7.3} {:>7.3} {:>7.3} {:>7.3}",
            run.target_qps,
            run.achieved_qps,
            run.total_requests,
            run.error_rate * 100.0,
            run.peak_in_flight,
            run.end_of_run_in_flight,
            run.ack_latency_ms.p50_ms,
            run.ack_latency_ms.p90_ms,
            run.ack_latency_ms.p99_ms,
            run.ack_latency_ms.p99_9_ms,
            run.ack_latency_ms.max_ms,
            run.ack_latency_ms.avg_ms
        );
        if run.degraded {
            println!("         degraded: {}", run.degradation_reasons.join("; "));
        }
    }

    if report.track_order_update_latency {
        println!();
        println!("First order-update latency:");
        for run in &report.runs {
            if let Some(latency) = &run.order_update_latency_ms {
                println!(
                    "  {:>8.0}/s p50={:.3} p90={:.3} p99={:.3} p99.9={:.3} max={:.3} avg={:.3}",
                    run.target_qps,
                    latency.p50_ms,
                    latency.p90_ms,
                    latency.p99_ms,
                    latency.p99_9_ms,
                    latency.max_ms,
                    latency.avg_ms
                );
            }
        }
    }

    if report.track_metric_latency {
        println!();
        println!("Through-OMS latency from metric tags (t3-t1):");
        for run in &report.runs {
            if let Some(latency) = &run.oms_through_latency_ms {
                println!(
                    "  {:>8.0}/s p50={:.3} p90={:.3} p99={:.3} p99.9={:.3} max={:.3} avg={:.3}",
                    run.target_qps,
                    latency.p50_ms,
                    latency.p90_ms,
                    latency.p99_ms,
                    latency.p99_9_ms,
                    latency.max_ms,
                    latency.avg_ms
                );
            }
        }
    }

    if report.track_metric_latency {
        println!();
        println!("Latency-metric batch arrival:");
        for run in &report.runs {
            if let Some(latency) = &run.metric_latency_ms {
                println!(
                    "  {:>8.0}/s p50={:.3} p90={:.3} p99={:.3} p99.9={:.3} max={:.3} avg={:.3}",
                    run.target_qps,
                    latency.p50_ms,
                    latency.p90_ms,
                    latency.p99_ms,
                    latency.p99_9_ms,
                    latency.max_ms,
                    latency.avg_ms
                );
            }
        }
    }

    println!();
    println!(
        "Thresholds: zero_error_qps={} steady_qps={}",
        report
            .highest_zero_error_qps
            .map(|v| format!("{v:.0}"))
            .unwrap_or_else(|| "n/a".to_string()),
        report
            .highest_steady_qps
            .map(|v| format!("{v:.0}"))
            .unwrap_or_else(|| "n/a".to_string())
    );
    for threshold in &report.threshold_views {
        println!(
            "  highest_qps with p99 < {:>4.1} ms: {}",
            threshold.threshold_ms,
            threshold
                .highest_qps
                .map(|v| format!("{v:.0}"))
                .unwrap_or_else(|| "n/a".to_string())
        );
    }
    if !report.notes.is_empty() {
        println!();
        println!("Notes:");
        for note in &report.notes {
            println!("  - {note}");
        }
    }
}

pub(crate) fn print_output(output: &ToolOutput) {
    match output {
        ToolOutput::Scenario(report) => print_scenario_report(report),
        ToolOutput::GatewayBenchmark(report) => print_gateway_benchmark_report(report),
        ToolOutput::OmsBenchmark(report) => print_oms_benchmark_report(report),
    }
}

pub(crate) fn maybe_write_json(path: &str, output: &ToolOutput) -> Result<()> {
    let data = serde_json::to_vec_pretty(output)?;
    std::fs::write(path, data)?;
    Ok(())
}
