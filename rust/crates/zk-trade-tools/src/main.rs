use anyhow::{bail, Context, Result};

mod bench;
mod cli;
mod report;
mod smoke;
mod support;

use bench::{run_gw_latency_bench, run_oms_latency_bench};
use cli::{Cli, Command};
use report::{maybe_write_json, print_output, ToolOutput};
use smoke::{run_gw_smoke, run_oms_smoke, run_sim_full_cycle};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse()?;

    let output = match &cli.command {
        Command::GwSmoke { gw_id, gw_addr } => {
            ToolOutput::Scenario(run_gw_smoke(&cli, gw_id, gw_addr).await)
        }
        Command::OmsSmoke { oms_id, oms_addr } => {
            ToolOutput::Scenario(run_oms_smoke(&cli, oms_id, oms_addr).await)
        }
        Command::SimFullCycle {
            gw_id,
            oms_id,
            gw_addr,
            oms_addr,
            gw_admin_addr,
        } => ToolOutput::Scenario(
            run_sim_full_cycle(&cli, gw_id, oms_id, gw_addr, oms_addr, gw_admin_addr).await,
        ),
        Command::GwLatencyBench {
            gw_id,
            gw_addr,
            rates,
            warmup_s,
            measure_s,
            mode,
            track_report_latency,
            instrument_pool,
            exch_account_pool,
        } => ToolOutput::GatewayBenchmark(
            run_gw_latency_bench(
                &cli,
                gw_id,
                gw_addr,
                rates,
                *warmup_s,
                *measure_s,
                mode,
                *track_report_latency,
                instrument_pool,
                exch_account_pool,
            )
            .await,
        ),
        Command::OmsLatencyBench {
            oms_id,
            oms_addr,
            rates,
            warmup_s,
            measure_s,
            mode,
            track_order_update_latency,
            track_metric_latency,
            instrument_pool,
        } => ToolOutput::OmsBenchmark(
            run_oms_latency_bench(
                &cli,
                oms_id,
                oms_addr,
                rates,
                *warmup_s,
                *measure_s,
                mode,
                *track_order_update_latency,
                *track_metric_latency,
                instrument_pool,
            )
            .await,
        ),
    };

    print_output(&output);

    if let Some(path) = &cli.json_out {
        maybe_write_json(path, &output)
            .with_context(|| format!("failed to write json report to {path}"))?;
    }

    if output.success() {
        Ok(())
    } else {
        bail!("scenario failed");
    }
}
