/// Compile the new versioned zk.*.v1 proto packages.
/// Legacy protos (common, oms, exch_gw, …) remain as committed generated files.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../protos");
    let proto_dir = proto_dir
        .canonicalize()
        .expect("protos/ dir not found — run from zkbot/rust/");
    let proto_dir = proto_dir.to_str().unwrap().to_owned();

    let files = [
        "zk/common/v1/common.proto",
        "zk/config/v1/config.proto",
        "zk/discovery/v1/discovery.proto",
        "zk/oms/v1/oms.proto",
        "zk/oms/v1/oms_service.proto",
        "zk/exch_gw/v1/exch_gw.proto",
        "zk/gateway/v1/gateway_service.proto",
        "zk/gateway/v1/gateway_simulator_admin.proto",
        "zk/strategy/v1/strategy.proto",
        "zk/rtmd/v1/rtmd.proto",
        "zk/rtmd/v1/rtmd_query_service.proto",
        "zk/engine/v1/engine_service.proto",
        "zk/monitor/v1/monitor.proto",
        "zk/pilot/v1/pilot_service.proto",
        "zk/pilot/v1/bootstrap.proto",
    ];

    let full_paths: Vec<String> = files.iter().map(|f| format!("{proto_dir}/{f}")).collect();

    prost_build::compile_protos(&full_paths, &[&proto_dir])?;

    // Re-run if any zk/ proto changes.
    println!("cargo:rerun-if-changed={proto_dir}/zk");
    Ok(())
}
