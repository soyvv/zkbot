use std::path::PathBuf;

use zk_pyo3_bridge::manifest;
use zk_pyo3_bridge::py_runtime::PyRuntime;
use zk_pyo3_bridge::refdata_adapter::{PyRefdataLoader, RefdataLoader};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let venue = std::env::args()
        .nth(1)
        .expect("usage: refdata-stub <venue> [venue-root]");
    let venue_root = std::env::args()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("venue-integrations"));

    println!("Loading refdata for venue '{}' from {:?}", venue, venue_root);

    let rt = PyRuntime::initialize()?;
    let mf = manifest::load_manifest(&venue_root, &venue)?;
    let cap = manifest::resolve_capability(&mf, manifest::CAP_REFDATA)?;
    let ep = manifest::parse_python_entrypoint(&cap.entrypoint)?;

    println!(
        "Entrypoint: {}::{}",
        ep.module_path, ep.class_name
    );

    // Validate config against schema (empty config for stub).
    manifest::validate_config(&venue_root, &venue, cap, &serde_json::json!({}))?;

    let handle = rt.load_class(&ep, serde_json::json!({}))?;
    let loader = PyRefdataLoader::new(handle);
    let instruments = loader.load_instruments().await?;

    println!(
        "Loaded {} instruments:",
        instruments.len()
    );
    println!("{}", serde_json::to_string_pretty(&instruments)?);

    Ok(())
}
