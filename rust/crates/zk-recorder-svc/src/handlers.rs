//! Message handlers — decode proto payloads, enrich, and call db operations.

use sqlx::PgPool;
use tracing::{debug, warn};

use zk_proto_rs::zk::recorder::v1::{RecorderTerminalOrder, RecorderTradeEvent};

use crate::db;

/// Handle a terminal-order message from JetStream.
pub async fn handle_terminal_order(
    pool: &PgPool,
    payload: &[u8],
    subject: &str,
) -> anyhow::Result<()> {
    use prost::Message;
    let msg = RecorderTerminalOrder::decode(payload)?;
    let oms_id = parse_oms_id_from_subject(subject);

    debug!(
        order_id = msg.order_id,
        account_id = msg.account_id,
        oms_id,
        status = msg.order_status,
        "recording terminal order"
    );

    let enrichment = db::try_enrich_from_source_binding(pool, &msg.source_id).await;
    db::upsert_order_oms(pool, &msg, &oms_id, &enrichment).await?;
    Ok(())
}

/// Handle a trade message from JetStream.
pub async fn handle_trade(
    pool: &PgPool,
    payload: &[u8],
    subject: &str,
) -> anyhow::Result<()> {
    use prost::Message;
    let msg = RecorderTradeEvent::decode(payload)?;
    let oms_id = parse_oms_id_from_subject(subject);

    debug!(
        order_id = msg.order_id,
        account_id = msg.account_id,
        oms_id,
        ext_trade_id = %msg.ext_trade_id,
        "recording trade"
    );

    let enrichment = db::try_enrich_from_source_binding(pool, &msg.source_id).await;
    db::insert_trade_oms(pool, &msg, &oms_id, &enrichment).await?;
    Ok(())
}

/// Parse `oms_id` from a NATS subject like
/// `zk.recorder.oms.<oms_id>.terminal_order.<account_id>` or
/// `zk.recorder.oms.<oms_id>.trade.<account_id>`.
fn parse_oms_id_from_subject(subject: &str) -> String {
    // Split: ["zk", "recorder", "oms", "<oms_id>", ...]
    let parts: Vec<&str> = subject.split('.').collect();
    if parts.len() >= 4 {
        parts[3].to_string()
    } else {
        warn!(subject, "unexpected subject format — cannot parse oms_id");
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_oms_id() {
        assert_eq!(
            parse_oms_id_from_subject("zk.recorder.oms.oms-1.terminal_order.42"),
            "oms-1"
        );
        assert_eq!(
            parse_oms_id_from_subject("zk.recorder.oms.my-oms.trade.99"),
            "my-oms"
        );
        assert_eq!(parse_oms_id_from_subject("bad.subject"), "");
    }
}
