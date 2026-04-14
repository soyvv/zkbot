//! JetStream stream helpers for zkbot services.

use async_nats::jetstream;

/// Stream name for recorder OMS events.
pub const RECORDER_STREAM_NAME: &str = "zk-recorder-oms";

/// Ensure the recorder JetStream stream exists.
///
/// Creates the stream if it does not exist, or returns the existing one.
/// Safe to call from both OMS (publisher) and recorder (consumer).
pub async fn ensure_recorder_stream(
    js: &jetstream::Context,
) -> Result<
    jetstream::stream::Stream,
    async_nats::error::Error<jetstream::context::CreateStreamErrorKind>,
> {
    js.get_or_create_stream(jetstream::stream::Config {
        name: RECORDER_STREAM_NAME.into(),
        subjects: vec!["zk.recorder.oms.>".into()],
        retention: jetstream::stream::RetentionPolicy::Limits,
        max_age: std::time::Duration::from_secs(7 * 24 * 3600), // 7 days
        storage: jetstream::stream::StorageType::File,
        num_replicas: 1,
        ..Default::default()
    })
    .await
}
