use std::collections::HashSet;

use zk_proto_rs::zk::exch_gw::v1::{
    order_report_entry::Report, BalanceUpdate, OrderReport, OrderReportType, PositionReport,
};

use crate::nats_publisher::NatsPublisher;
use crate::venue_adapter::{VenueBalanceFact, VenueEvent, VenuePositionFact};

/// Processes VenueEvents through the gateway semantic pipeline and publishes
/// normalized events to NATS.
///
/// Responsibilities:
/// - Trade dedup: track `exch_trade_id` set for exactly-once trade publication
/// - Order state: at-least-once (re-publish is safe)
/// - Balance/position: track published-position set for explicit zero-position transitions
pub struct SemanticPipeline {
    /// Set of published trade IDs for exactly-once dedup.
    published_trade_ids: HashSet<String>,
    /// Set of (instrument, side) keys with non-zero published positions.
    /// Used to detect → 0 transitions.
    published_position_keys: HashSet<String>,
    /// Exchange-side account code for balance/position publications.
    /// OMS uses this to link exchange-owned state to the managed account.
    exch_account_code: String,
}

impl SemanticPipeline {
    pub fn new(exch_account_code: String) -> Self {
        Self {
            published_trade_ids: HashSet::new(),
            published_position_keys: HashSet::new(),
            exch_account_code,
        }
    }

    /// Process a venue event and publish normalized results to NATS.
    pub async fn process(&mut self, event: VenueEvent, publisher: &NatsPublisher) {
        match event {
            VenueEvent::OrderReport(report) => {
                self.process_order_report(report, publisher).await;
            }
            VenueEvent::Balance(facts) => {
                self.process_balance(facts, publisher).await;
            }
            VenueEvent::Position(facts) => {
                self.process_position(facts, publisher).await;
            }
            VenueEvent::System(sys) => {
                let event = zk_proto_rs::zk::exch_gw::v1::GatewaySystemEvent {
                    gw_name: publisher.gw_id().to_string(),
                    event_type: match sys.event_type {
                        crate::venue_adapter::VenueSystemEventType::Connected => {
                            zk_proto_rs::zk::exch_gw::v1::GatewayEventType::GwEventStarted as i32
                        }
                        crate::venue_adapter::VenueSystemEventType::Disconnected => {
                            zk_proto_rs::zk::exch_gw::v1::GatewayEventType::GwEventExchDisconnected
                                as i32
                        }
                        _ => 0,
                    },
                    service_endpoint: String::new(),
                    event_timestamp: sys.timestamp,
                };
                publisher.publish_system_event(&event).await;
            }
        }
    }

    /// Process an OrderReport: dedup trades, publish at-least-once for state.
    async fn process_order_report(&mut self, report: OrderReport, publisher: &NatsPublisher) {
        let mut deduped_entries = Vec::new();

        for entry in &report.order_report_entries {
            if entry.report_type == OrderReportType::OrderRepTypeTrade as i32 {
                // Exactly-once for trades: skip if we've already published this trade_id.
                if let Some(Report::TradeReport(trade)) = &entry.report {
                    if !trade.exch_trade_id.is_empty()
                        && !self.published_trade_ids.insert(trade.exch_trade_id.clone())
                    {
                        // Already published — skip this trade entry.
                        continue;
                    }
                }
            }
            deduped_entries.push(entry.clone());
        }

        if deduped_entries.is_empty() {
            return;
        }

        let mut deduped_report = report;
        deduped_report.order_report_entries = deduped_entries;
        publisher.publish_order_report(&deduped_report).await;
    }

    /// Process balance facts: convert to proto and publish.
    async fn process_balance(&self, facts: Vec<VenueBalanceFact>, publisher: &NatsPublisher) {
        let balances: Vec<PositionReport> = facts
            .iter()
            .map(|f| PositionReport {
                instrument_code: f.asset.clone(),
                instrument_type: zk_proto_rs::zk::common::v1::InstrumentType::InstTypeSpot as i32,
                qty: f.total_qty,
                avail_qty: f.avail_qty,
                exch_account_code: self.exch_account_code.clone(),
                ..Default::default()
            })
            .collect();

        let update = BalanceUpdate { balances };
        publisher.publish_balance_update(&update).await;
    }

    /// Process position facts: track published keys for zero-position transitions.
    async fn process_position(&mut self, facts: Vec<VenuePositionFact>, publisher: &NatsPublisher) {
        // Build current position key set from facts.
        let mut current_keys: HashSet<String> = HashSet::new();
        let mut position_reports: Vec<PositionReport> = Vec::new();

        for fact in &facts {
            let key = format!("{}:{}", fact.instrument, fact.long_short_type);
            current_keys.insert(key.clone());

            if fact.qty != 0.0 {
                self.published_position_keys.insert(key);
            }

            position_reports.push(PositionReport {
                instrument_code: fact.instrument.clone(),
                qty: fact.qty,
                avail_qty: fact.avail_qty,
                exch_account_code: self.exch_account_code.clone(),
                ..Default::default()
            });
        }

        // Detect → 0 transitions: keys we previously published as non-zero
        // that are now absent from the current facts.
        let zero_keys: Vec<String> = self
            .published_position_keys
            .iter()
            .filter(|k| !current_keys.contains(*k))
            .cloned()
            .collect();

        for key in &zero_keys {
            let parts: Vec<&str> = key.splitn(2, ':').collect();
            if parts.len() == 2 {
                position_reports.push(PositionReport {
                    instrument_code: parts[0].to_string(),
                    qty: 0.0,
                    avail_qty: 0.0,
                    exch_account_code: self.exch_account_code.clone(),
                    ..Default::default()
                });
            }
            self.published_position_keys.remove(key);
        }

        if !position_reports.is_empty() {
            let update = BalanceUpdate {
                balances: position_reports,
            };
            publisher.publish_balance_update(&update).await;
        }
    }

    /// Clear all pipeline state (for ResetSimulator).
    pub fn reset(&mut self) {
        self.published_trade_ids.clear();
        self.published_position_keys.clear();
    }
}
