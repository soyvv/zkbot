/// Tonic-generated service stubs for this binary.
///
/// Message types are NOT redefined here — they are shared from `zk-proto-rs`
/// via `extern_path` in `build.rs`. Only the service client/server traits and
/// wrappers are generated into `OUT_DIR`.
///
/// # Module layout
///
/// - `oms_svc` — `o_m_s_service_server::*` (server trait + wrapper used by main)
/// - `gw_svc`  — `gateway_service_client::*` (client used by `gw_client.rs`)
/// - `config_svc` — `config_introspection_service_server::*` (introspection)

pub mod oms_svc {
    include!(concat!(env!("OUT_DIR"), "/zk.oms.v1.rs"));
}

pub mod gw_svc {
    include!(concat!(env!("OUT_DIR"), "/zk.gateway.v1.rs"));
}

pub mod config_svc {
    include!(concat!(env!("OUT_DIR"), "/zk.config.v1.rs"));
}
