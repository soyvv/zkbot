//! JetStream durable pull consumers for the recorder service.
//!
//! Two consumers on the `zk-recorder-oms` stream:
//! - `recorder-terminal-order` — filters `zk.recorder.oms.*.terminal_order.>`
//! - `recorder-trade` — filters `zk.recorder.oms.*.trade.>`
//!
//! Each consumer runs in its own tokio task, decodes proto, calls the handler,
//! and acks on success. Shutdown is coordinated via `CancellationToken`.

use std::time::Duration;

use async_nats::jetstream;
use futures::StreamExt;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use zk_infra_rs::nats_js::RECORDER_STREAM_NAME;

use crate::handlers;

#[derive(Clone, Copy)]
enum ConsumerKind {
    TerminalOrder,
    Trade,
}

/// Spawn the terminal-order consumer task.
pub async fn spawn_terminal_order_consumer(
    js: &jetstream::Context,
    pool: PgPool,
    ack_wait_secs: u64,
    shutdown: CancellationToken,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let consumer = get_or_create_consumer(
        js,
        "recorder-terminal-order",
        "zk.recorder.oms.*.terminal_order.>",
        ack_wait_secs,
    )
    .await?;

    Ok(tokio::spawn(consumer_loop(
        consumer, pool, shutdown, ConsumerKind::TerminalOrder,
    )))
}

/// Spawn the trade consumer task.
pub async fn spawn_trade_consumer(
    js: &jetstream::Context,
    pool: PgPool,
    ack_wait_secs: u64,
    shutdown: CancellationToken,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let consumer = get_or_create_consumer(
        js,
        "recorder-trade",
        "zk.recorder.oms.*.trade.>",
        ack_wait_secs,
    )
    .await?;

    Ok(tokio::spawn(consumer_loop(
        consumer, pool, shutdown, ConsumerKind::Trade,
    )))
}

// ── Internal ────────────────────────────────────────────────────────────────

async fn get_or_create_consumer(
    js: &jetstream::Context,
    consumer_name: &str,
    filter_subject: &str,
    ack_wait_secs: u64,
) -> anyhow::Result<jetstream::consumer::PullConsumer> {
    let stream = js.get_stream(RECORDER_STREAM_NAME).await?;
    let consumer = stream
        .get_or_create_consumer(
            consumer_name,
            jetstream::consumer::pull::Config {
                durable_name: Some(consumer_name.into()),
                filter_subject: filter_subject.into(),
                ack_wait: Duration::from_secs(ack_wait_secs),
                ..Default::default()
            },
        )
        .await?;
    info!(consumer_name, filter_subject, "JetStream consumer ready");
    Ok(consumer)
}

async fn consumer_loop(
    consumer: jetstream::consumer::PullConsumer,
    pool: PgPool,
    shutdown: CancellationToken,
    kind: ConsumerKind,
) {
    let label = match kind {
        ConsumerKind::TerminalOrder => "terminal_order",
        ConsumerKind::Trade => "trade",
    };

    let mut messages = match consumer.messages().await {
        Ok(m) => m,
        Err(e) => {
            error!(label, error = %e, "failed to open message stream");
            return;
        }
    };

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                info!(label, "consumer shutting down");
                break;
            }
            msg = messages.next() => {
                let Some(msg) = msg else {
                    warn!(label, "message stream ended unexpectedly");
                    break;
                };
                match msg {
                    Ok(msg) => {
                        let subject = msg.subject.as_str().to_string();
                        let payload = msg.payload.as_ref();

                        let result = match kind {
                            ConsumerKind::TerminalOrder => {
                                handlers::handle_terminal_order(&pool, payload, &subject).await
                            }
                            ConsumerKind::Trade => {
                                handlers::handle_trade(&pool, payload, &subject).await
                            }
                        };

                        match result {
                            Ok(()) => {
                                if let Err(e) = msg.ack().await {
                                    warn!(label, error = %e, "failed to ack message");
                                }
                            }
                            Err(e) => {
                                error!(label, subject, error = %e, "handler failed — message will be redelivered");
                            }
                        }
                    }
                    Err(e) => {
                        warn!(label, error = %e, "error receiving message from stream");
                    }
                }
            }
        }
    }
}
