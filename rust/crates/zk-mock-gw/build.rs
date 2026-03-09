fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_root = "../../../protos";
    tonic_build::configure()
        .build_server(true)
        .build_client(false)
        .compile_protos(
            &[&format!("{proto_root}/rpc-exch-gateway.proto")],
            &[proto_root],
        )?;
    Ok(())
}
