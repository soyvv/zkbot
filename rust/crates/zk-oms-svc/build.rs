/// Compile OMS service stubs (server) and Gateway + Admin service stubs (client).
///
/// Message types are NOT regenerated here — they are already compiled by
/// `zk-proto-rs/build.rs` (via prost-build) and referenced via `extern_path`.
/// This crate only generates the tonic service traits and client/server wrappers.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../protos")
        .canonicalize()
        .expect("protos/ directory not found — run from zkbot/rust/");
    let proto_dir_str = proto_dir.to_str().unwrap();

    tonic_build::configure()
        // ── Reuse message types from zk-proto-rs (no duplication) ──────────
        // These extern_path entries tell tonic/prost NOT to regenerate message
        // types for these packages — all references will resolve to zk-proto-rs.
        .extern_path(".zk.common.v1", "::zk_proto_rs::zk::common::v1")
        .extern_path(".zk.oms.v1", "::zk_proto_rs::zk::oms::v1")
        .extern_path(".zk.exch_gw.v1", "::zk_proto_rs::zk::exch_gw::v1")
        .extern_path(".zk.gateway.v1", "::zk_proto_rs::zk::gateway::v1")
        // ── Generate server stubs for OMSService ────────────────────────────
        .build_server(true)
        // ── Generate client stubs for GatewayService ────────────────────────
        .build_client(true)
        .compile_protos(
            &[
                &format!("{proto_dir_str}/zk/oms/v1/oms_service.proto"),
                &format!("{proto_dir_str}/zk/gateway/v1/gateway_service.proto"),
                &format!("{proto_dir_str}/zk/gateway/v1/gateway_simulator_admin.proto"),
            ],
            &[proto_dir_str],
        )?;

    println!("cargo:rerun-if-changed={proto_dir_str}/zk/oms");
    println!("cargo:rerun-if-changed={proto_dir_str}/zk/gateway");
    Ok(())
}
