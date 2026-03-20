use tracing_subscriber::EnvFilter;

/// Initialise the global tracing subscriber.
///
/// - Output format: JSON when `RUST_LOG_FORMAT=json`, otherwise pretty-print.
/// - Filter: read from `RUST_LOG` env var; defaults to `{service_name}=info,warn`.
pub fn init_tracing(service_name: &str) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("{service_name}=info,warn")));

    let use_json = std::env::var("RUST_LOG_FORMAT")
        .map(|v| v.to_lowercase() == "json")
        .unwrap_or(false);

    if use_json {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .with_current_span(false)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
}
