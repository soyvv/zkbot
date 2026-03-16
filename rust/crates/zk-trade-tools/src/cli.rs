use std::collections::HashMap;

use anyhow::{anyhow, bail, Context, Result};

#[derive(Debug)]
pub(crate) struct Cli {
    pub(crate) nats_url: String,
    pub(crate) grpc_host: String,
    pub(crate) account_id: i64,
    pub(crate) exch_account_id: Option<String>,
    pub(crate) instrument: String,
    pub(crate) price: f64,
    pub(crate) qty: f64,
    pub(crate) timeout_ms: u64,
    pub(crate) json_out: Option<String>,
    pub(crate) command: Command,
}

#[derive(Debug)]
pub(crate) enum Command {
    GwSmoke {
        gw_id: String,
        gw_addr: Option<String>,
    },
    OmsSmoke {
        oms_id: String,
        oms_addr: Option<String>,
    },
    SimFullCycle {
        gw_id: String,
        oms_id: String,
        gw_addr: Option<String>,
        oms_addr: Option<String>,
        gw_admin_addr: Option<String>,
    },
    GwLatencyBench {
        gw_id: String,
        gw_addr: Option<String>,
        rates: Vec<f64>,
        warmup_s: f64,
        measure_s: f64,
        mode: String,
        track_report_latency: bool,
        instrument_pool: Vec<String>,
        exch_account_pool: Vec<String>,
    },
    OmsLatencyBench {
        oms_id: String,
        oms_addr: Option<String>,
        rates: Vec<f64>,
        warmup_s: f64,
        measure_s: f64,
        mode: String,
        track_order_update_latency: bool,
        track_metric_latency: bool,
        instrument_pool: Vec<String>,
    },
}

impl Cli {
    pub(crate) fn parse() -> Result<Self> {
        let mut args = std::env::args().skip(1);
        let Some(cmd) = args.next() else {
            bail!(Self::usage());
        };

        let mut kv = HashMap::<String, String>::new();
        while let Some(arg) = args.next() {
            if !arg.starts_with("--") {
                bail!("unexpected positional argument: {arg}\n\n{}", Self::usage());
            }
            let key = arg.trim_start_matches("--").to_string();
            let value = args
                .next()
                .ok_or_else(|| anyhow!("missing value for --{key}\n\n{}", Self::usage()))?;
            kv.insert(key, value);
        }

        let cli = Self {
            nats_url: kv
                .remove("nats-url")
                .unwrap_or_else(|| "nats://127.0.0.1:4222".to_string()),
            grpc_host: kv
                .remove("grpc-host")
                .unwrap_or_else(|| "127.0.0.1".to_string()),
            account_id: kv
                .remove("account-id")
                .map(|v| v.parse())
                .transpose()
                .context("invalid --account-id")?
                .unwrap_or(9001),
            exch_account_id: kv.remove("exch-account-id"),
            instrument: kv
                .remove("instrument")
                .unwrap_or_else(|| "BTCUSDT_SIM".to_string()),
            price: kv
                .remove("price")
                .map(|v| v.parse())
                .transpose()
                .context("invalid --price")?
                .unwrap_or(50_000.0),
            qty: kv
                .remove("qty")
                .map(|v| v.parse())
                .transpose()
                .context("invalid --qty")?
                .unwrap_or(0.001),
            timeout_ms: kv
                .remove("timeout-ms")
                .map(|v| v.parse())
                .transpose()
                .context("invalid --timeout-ms")?
                .unwrap_or(5_000),
            json_out: kv.remove("json-out"),
            command: match cmd.as_str() {
                "gw-smoke" => Command::GwSmoke {
                    gw_id: kv
                        .remove("gw-id")
                        .ok_or_else(|| anyhow!("--gw-id is required\n\n{}", Self::usage()))?,
                    gw_addr: kv.remove("gw-addr"),
                },
                "oms-smoke" => Command::OmsSmoke {
                    oms_id: kv
                        .remove("oms-id")
                        .ok_or_else(|| anyhow!("--oms-id is required\n\n{}", Self::usage()))?,
                    oms_addr: kv.remove("oms-addr"),
                },
                "sim-full-cycle" => Command::SimFullCycle {
                    gw_id: kv
                        .remove("gw-id")
                        .ok_or_else(|| anyhow!("--gw-id is required\n\n{}", Self::usage()))?,
                    oms_id: kv
                        .remove("oms-id")
                        .ok_or_else(|| anyhow!("--oms-id is required\n\n{}", Self::usage()))?,
                    gw_addr: kv.remove("gw-addr"),
                    oms_addr: kv.remove("oms-addr"),
                    gw_admin_addr: kv.remove("gw-admin-addr"),
                },
                "gw-latency-bench" => {
                    let mode = kv
                        .remove("mode")
                        .unwrap_or_else(|| "place_only".to_string());
                    if mode != "place_only" {
                        bail!("unsupported --mode {mode}; only place_only is implemented today");
                    }
                    Command::GwLatencyBench {
                        gw_id: kv
                            .remove("gw-id")
                            .ok_or_else(|| anyhow!("--gw-id is required\n\n{}", Self::usage()))?,
                        gw_addr: kv.remove("gw-addr"),
                        rates: parse_csv_f64(
                            kv.remove("rates")
                                .unwrap_or_else(|| "100,1000,5000,10000".to_string())
                                .as_str(),
                        )
                        .context("invalid --rates")?,
                        warmup_s: kv
                            .remove("warmup-s")
                            .map(|v| v.parse())
                            .transpose()
                            .context("invalid --warmup-s")?
                            .unwrap_or(15.0),
                        measure_s: kv
                            .remove("measure-s")
                            .map(|v| v.parse())
                            .transpose()
                            .context("invalid --measure-s")?
                            .unwrap_or(60.0),
                        mode,
                        track_report_latency: kv
                            .remove("track-report-latency")
                            .map(|v| parse_bool(&v))
                            .transpose()
                            .context("invalid --track-report-latency")?
                            .unwrap_or(false),
                        instrument_pool: parse_csv_string(
                            kv.remove("instrument-pool").unwrap_or_default().as_str(),
                        ),
                        exch_account_pool: parse_csv_string(
                            kv.remove("exch-account-pool").unwrap_or_default().as_str(),
                        ),
                    }
                }
                "oms-latency-bench" => {
                    let mode = kv
                        .remove("mode")
                        .unwrap_or_else(|| "place_only".to_string());
                    if mode != "place_only" {
                        bail!("unsupported --mode {mode}; only place_only is implemented today");
                    }
                    Command::OmsLatencyBench {
                        oms_id: kv
                            .remove("oms-id")
                            .ok_or_else(|| anyhow!("--oms-id is required\n\n{}", Self::usage()))?,
                        oms_addr: kv.remove("oms-addr"),
                        rates: parse_csv_f64(
                            kv.remove("rates")
                                .unwrap_or_else(|| "100,1000,5000,10000".to_string())
                                .as_str(),
                        )
                        .context("invalid --rates")?,
                        warmup_s: kv
                            .remove("warmup-s")
                            .map(|v| v.parse())
                            .transpose()
                            .context("invalid --warmup-s")?
                            .unwrap_or(15.0),
                        measure_s: kv
                            .remove("measure-s")
                            .map(|v| v.parse())
                            .transpose()
                            .context("invalid --measure-s")?
                            .unwrap_or(60.0),
                        mode,
                        track_order_update_latency: kv
                            .remove("track-order-update-latency")
                            .map(|v| parse_bool(&v))
                            .transpose()
                            .context("invalid --track-order-update-latency")?
                            .unwrap_or(true),
                        track_metric_latency: kv
                            .remove("track-metric-latency")
                            .map(|v| parse_bool(&v))
                            .transpose()
                            .context("invalid --track-metric-latency")?
                            .unwrap_or(false),
                        instrument_pool: parse_csv_string(
                            kv.remove("instrument-pool").unwrap_or_default().as_str(),
                        ),
                    }
                }
                _ => bail!("unknown command: {cmd}\n\n{}", Self::usage()),
            },
        };

        if !kv.is_empty() {
            let mut unknown: Vec<_> = kv.into_keys().collect();
            unknown.sort();
            bail!("unknown flags: {}\n\n{}", unknown.join(", "), Self::usage());
        }

        Ok(cli)
    }

    fn usage() -> &'static str {
        "Usage:
  zk-trade-tools gw-smoke --gw-id <id> [--gw-addr <http://host:port>] [common flags]
  zk-trade-tools oms-smoke --oms-id <id> [--oms-addr <http://host:port>] [common flags]
  zk-trade-tools sim-full-cycle --gw-id <id> --oms-id <id> [--gw-addr <...>] [--oms-addr <...>] [--gw-admin-addr <...>] [common flags]
  zk-trade-tools gw-latency-bench --gw-id <id> [--gw-addr <http://host:port>] [--rates 100,1000,5000,10000] [--warmup-s 15] [--measure-s 60] [--mode place_only] [--track-report-latency true] [common flags]
  zk-trade-tools oms-latency-bench --oms-id <id> [--oms-addr <http://host:port>] [--rates 100,1000,5000,10000] [--warmup-s 15] [--measure-s 60] [--mode place_only] [--track-order-update-latency true] [common flags]

Common flags:
  --nats-url <url>                default: nats://127.0.0.1:4222
  --grpc-host <host>              default: 127.0.0.1
  --account-id <id>               default: 9001
  --exch-account-id <id>          default: account-id as string
  --instrument <symbol>           default: BTCUSDT_SIM
  --price <px>                    default: 50000
  --qty <qty>                     default: 0.001
  --timeout-ms <ms>               default: 5000
  --json-out <path>

Benchmark flags:
  --rates <csv>                   target QPS sweep, default: 100,1000,5000,10000
  --warmup-s <seconds>            default: 15
  --measure-s <seconds>           default: 60
  --mode <name>                   default: place_only
  --track-report-latency <bool>   default: false
  --track-order-update-latency <bool> default: true
  --track-metric-latency <bool>   default: false
  --instrument-pool <csv>         optional realistic instrument variation
  --exch-account-pool <csv>       optional realistic account variation"
    }
}

fn parse_bool(value: &str) -> Result<bool> {
    match value {
        "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON" => Ok(true),
        "0" | "false" | "FALSE" | "no" | "NO" | "off" | "OFF" => Ok(false),
        _ => bail!("expected boolean, got {value}"),
    }
}

fn parse_csv_f64(value: &str) -> Result<Vec<f64>> {
    let values = value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| part.parse::<f64>().context("invalid float in CSV"))
        .collect::<Result<Vec<_>>>()?;
    if values.is_empty() {
        bail!("expected at least one numeric value");
    }
    if values.iter().any(|v| *v <= 0.0) {
        bail!("all rates must be > 0");
    }
    Ok(values)
}

fn parse_csv_string(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}
