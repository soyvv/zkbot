/// Compile GatewayService (server) and GatewaySimulatorAdminService (server).
///
/// Message types are NOT regenerated here — they are already compiled by
/// `zk-proto-rs/build.rs` and referenced via `extern_path`.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../protos")
        .canonicalize()
        .expect("protos/ directory not found — run from zkbot/rust/");
    let proto_dir_str = proto_dir.to_str().unwrap();

    tonic_build::configure()
        // Reuse message types from zk-proto-rs
        .extern_path(".zk.common.v1", "::zk_proto_rs::zk::common::v1")
        .extern_path(".zk.oms.v1", "::zk_proto_rs::zk::oms::v1")
        .extern_path(".zk.exch_gw.v1", "::zk_proto_rs::zk::exch_gw::v1")
        .extern_path(".zk.rtmd.v1", "::zk_proto_rs::zk::rtmd::v1")
        // Generate server stubs only (this is a server crate)
        .build_server(true)
        .build_client(false)
        .compile_protos(
            &[
                &format!("{proto_dir_str}/zk/gateway/v1/gateway_service.proto"),
                &format!("{proto_dir_str}/zk/gateway/v1/gateway_simulator_admin.proto"),
            ],
            &[proto_dir_str],
        )?;

    println!("cargo:rerun-if-changed={proto_dir_str}/zk/gateway");
    Ok(())
}
