use prost::Message;
use tracing::warn;

/// Publishes protobuf-encoded gateway events to NATS subjects.
pub struct NatsPublisher {
    nats: async_nats::Client,
    gw_id: String,
}

impl NatsPublisher {
    pub fn new(nats: async_nats::Client, gw_id: String) -> Self {
        Self { nats, gw_id }
    }

    /// Publish an OrderReport to `zk.gw.<gw_id>.report`.
    ///
    /// Stamps `report.exchange` with `gw_id` so the OMS can identify the
    /// originating gateway (OMS uses `exchange` as `gw_key`).
    pub async fn publish_order_report(
        &self,
        report: &zk_proto_rs::zk::exch_gw::v1::OrderReport,
    ) {
        let subject = format!("zk.gw.{}.report", self.gw_id);
        let mut stamped = report.clone();
        stamped.exchange = self.gw_id.clone();
        let bytes = stamped.encode_to_vec();
        if let Err(e) = self.nats.publish(subject.clone(), bytes.into()).await {
            warn!(subject, error = %e, "failed to publish order report");
        }
    }

    /// Publish a BalanceUpdate to `zk.gw.<gw_id>.balance`.
    pub async fn publish_balance_update(
        &self,
        update: &zk_proto_rs::zk::exch_gw::v1::BalanceUpdate,
    ) {
        let subject = format!("zk.gw.{}.balance", self.gw_id);
        let bytes = update.encode_to_vec();
        if let Err(e) = self.nats.publish(subject.clone(), bytes.into()).await {
            warn!(subject, error = %e, "failed to publish balance update");
        }
    }

    /// Publish a GatewaySystemEvent to `zk.gw.<gw_id>.system`.
    pub async fn publish_system_event(
        &self,
        event: &zk_proto_rs::zk::exch_gw::v1::GatewaySystemEvent,
    ) {
        let subject = format!("zk.gw.{}.system", self.gw_id);
        let bytes = event.encode_to_vec();
        if let Err(e) = self.nats.publish(subject.clone(), bytes.into()).await {
            warn!(subject, error = %e, "failed to publish system event");
        }
    }

    pub fn gw_id(&self) -> &str {
        &self.gw_id
    }
}
