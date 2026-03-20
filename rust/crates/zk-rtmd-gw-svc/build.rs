fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../protos")
        .canonicalize()
        .expect("protos/ directory not found — run from zkbot/rust/");
    let proto_dir_str = proto_dir.to_str().unwrap();

    tonic_build::configure()
        .extern_path(".zk.rtmd.v1", "::zk_proto_rs::zk::rtmd::v1")
        .build_server(true)
        .build_client(false)
        .compile_protos(
            &[&format!("{proto_dir_str}/zk/rtmd/v1/rtmd_query_service.proto")],
            &[proto_dir_str],
        )?;

    println!("cargo:rerun-if-changed={proto_dir_str}/zk/rtmd");
    Ok(())
}
