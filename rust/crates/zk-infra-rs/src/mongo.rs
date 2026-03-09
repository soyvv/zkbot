use std::time::{SystemTime, UNIX_EPOCH};

use mongodb::bson::{doc, to_bson, Document};
use mongodb::{Client, Collection};
use serde::Serialize;
use tracing::warn;

/// Connect to MongoDB at `url`.
pub async fn connect(url: &str) -> Result<Client, mongodb::error::Error> {
    Client::with_uri_str(url).await
}

/// Append-only writer for a single MongoDB collection.
///
/// All documents are wrapped in a common event envelope so every collection
/// is queryable by `event_type`, `correlation_id`, `account_id`, and time.
pub struct EventWriter {
    collection: Collection<Document>,
}

impl EventWriter {
    pub fn new(client: &Client, db: &str, collection: &str) -> Self {
        Self {
            collection: client.database(db).collection(collection),
        }
    }

    /// Append an event. `payload` is serialised to BSON and stored under `payload`.
    ///
    /// Envelope fields added automatically:
    /// - `event_type`, `event_version = 1`
    /// - `event_ts` = `ingest_ts` = current time (ms since epoch)
    /// - `correlation_id`, `account_id` (optional — pass 0 to omit)
    /// - `service_id`
    pub async fn append<T: Serialize>(
        &self,
        event_type: &str,
        service_id: &str,
        correlation_id: &str,
        account_id: i64,
        payload: &T,
    ) -> Result<(), mongodb::error::Error> {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let payload_bson = match to_bson(payload) {
            Ok(b) => b,
            Err(e) => {
                warn!(event_type, error = %e, "bson serialise failed");
                return Err(mongodb::error::Error::custom(e));
            }
        };

        let mut doc = doc! {
            "event_type":    event_type,
            "event_version": 1i32,
            "event_ts":      now_ms,
            "ingest_ts":     now_ms,
            "service_id":    service_id,
            "correlation_id": correlation_id,
            "payload":       payload_bson,
        };
        if account_id != 0 {
            doc.insert("account_id", account_id);
        }

        self.collection.insert_one(doc).await?;
        Ok(())
    }
}
