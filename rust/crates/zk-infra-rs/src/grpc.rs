use std::time::Duration;
use tonic::transport::{Channel, Endpoint};

/// Build a tonic `Channel` to `addr` (e.g. `"http://localhost:51051"`).
///
/// - Connect timeout: 5 s
/// - HTTP/2 keep-alive: 30 s interval, 10 s timeout
/// - Lazy connect — the channel connects on first use, not during `build_channel`.
pub async fn build_channel(addr: &str) -> Result<Channel, tonic::transport::Error> {
    Endpoint::new(addr.to_string())?
        .connect_timeout(Duration::from_secs(5))
        .tcp_keepalive(Some(Duration::from_secs(30)))
        .http2_keep_alive_interval(Duration::from_secs(30))
        .keep_alive_timeout(Duration::from_secs(10))
        .connect_lazy()
        .into_ok()
}

// Tonic's `connect_lazy()` returns `Channel` directly (not a Result), so wrap.
trait IntoOk<T> {
    fn into_ok(self) -> Result<T, tonic::transport::Error>;
}

impl IntoOk<Channel> for Channel {
    fn into_ok(self) -> Result<Channel, tonic::transport::Error> {
        Ok(self)
    }
}
