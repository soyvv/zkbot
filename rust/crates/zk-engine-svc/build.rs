/// Compile EngineService server stubs.
///
/// Message types are NOT regenerated here — they are already compiled by
/// `zk-proto-rs/build.rs` (via prost-build) and referenced via `extern_path`.
/// This crate only generates the tonic service traits and server wrappers.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../protos")
        .canonicalize()
        .expect("protos/ directory not found — run from zkbot/rust/");
    let proto_dir_str = proto_dir.to_str().unwrap();

    tonic_build::configure()
        // Reuse message types from zk-proto-rs (no duplication).
        .extern_path(".zk.common.v1", "::zk_proto_rs::zk::common::v1")
        .extern_path(".zk.engine.v1", "::zk_proto_rs::zk::engine::v1")
        // Generate server stubs for EngineService.
        .build_server(true)
        .build_client(false)
        .compile_protos(
            &[&format!(
                "{proto_dir_str}/zk/engine/v1/engine_service.proto"
            )],
            &[proto_dir_str],
        )?;

    println!("cargo:rerun-if-changed={proto_dir_str}/zk/engine");
    Ok(())
}
