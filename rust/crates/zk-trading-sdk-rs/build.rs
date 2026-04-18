/// Compile tonic service client stubs for the trading SDK.
///
/// - OMS service client: reuses message types from `zk-proto-rs` via `extern_path`
/// - Refdata service client: compiled locally (no `zk-proto-rs` module for refdata yet)
/// - RTMD query client: reuses message types from `zk-proto-rs`
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../protos")
        .canonicalize()
        .expect("protos/ directory not found — run from zkbot/rust/");
    let proto_dir_str = proto_dir.to_str().unwrap();

    // OMS service client — message types reused from zk-proto-rs
    tonic_build::configure()
        .extern_path(".zk.common.v1", "::zk_proto_rs::zk::common::v1")
        .extern_path(".zk.oms.v1", "::zk_proto_rs::zk::oms::v1")
        .build_server(false)
        .build_client(true)
        .compile_protos(
            &[&format!("{proto_dir_str}/zk/oms/v1/oms_service.proto")],
            &[proto_dir_str],
        )?;

    // Refdata service client — message types compiled locally
    tonic_build::configure()
        .build_server(false)
        .build_client(true)
        .compile_protos(
            &[&format!("{proto_dir_str}/zk/refdata/v1/refdata.proto")],
            &[proto_dir_str],
        )?;

    tonic_build::configure()
        .extern_path(".zk.rtmd.v1", "::zk_proto_rs::zk::rtmd::v1")
        .build_server(false)
        .build_client(true)
        .compile_protos(
            &[&format!("{proto_dir_str}/zk/rtmd/v1/rtmd_query_service.proto")],
            &[proto_dir_str],
        )?;

    println!("cargo:rerun-if-changed={proto_dir_str}/zk/oms");
    println!("cargo:rerun-if-changed={proto_dir_str}/zk/refdata");
    println!("cargo:rerun-if-changed={proto_dir_str}/zk/rtmd");
    Ok(())
}
