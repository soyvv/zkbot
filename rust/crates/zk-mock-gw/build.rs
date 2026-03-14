fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_root = "../../../protos";

    // Legacy ExchangeGatewayService (tqrpc_exch_gw package)
    tonic_build::configure()
        .build_server(true)
        .build_client(false)
        .compile_protos(
            &[&format!("{proto_root}/rpc-exch-gateway.proto")],
            &[proto_root],
        )?;

    // New GatewayService (zk.gateway.v1) — message types reused from zk-proto-rs.
    tonic_build::configure()
        .build_server(true)
        .build_client(false)
        .extern_path(".zk.common.v1", "::zk_proto_rs::zk::common::v1")
        .extern_path(".zk.exch_gw.v1", "::zk_proto_rs::zk::exch_gw::v1")
        .compile_protos(
            &[&format!("{proto_root}/zk/gateway/v1/gateway_service.proto")],
            &[proto_root],
        )?;

    Ok(())
}
