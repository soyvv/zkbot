use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tracing::info;

use zk_infra_rs::bootstrap::{bootstrap_runtime_config, BootstrapMode};
use zk_infra_rs::discovery_registration;
use zk_infra_rs::service_registry::ServiceRegistration;
use zk_rtmd_gw_svc::{
    config::{MdgwBootstrapConfig, MdgwService},
    grpc_handler::RtmdQueryHandler,
    nats_publisher::RtmdNatsPublisher,
    nats_sub_source::NatsKvSubSource,
    proto::zk_rtmd_v1::rtmd_query_service_server::RtmdQueryServiceServer,
};
use zk_rtmd_rs::{sub_manager::SubscriptionManager, venue::build_adapter};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "zk_rtmd_gw_svc=info,warn".into()),
        )
        .init();

    // ── 1. Load bootstrap config (always from env) ───────────────────────────
    let boot_cfg = MdgwBootstrapConfig::from_env()
        .expect("failed to load MdgwBootstrapConfig from env");

    // ── 2. NATS connection (uses only bootstrap nats_url) ────────────────────
    info!(url = %boot_cfg.nats_url, "connecting to NATS");
    let nats_client = async_nats::connect(&boot_cfg.nats_url).await?;
    let js = async_nats::jetstream::new(nats_client.clone());

    // ── 3. Config assembly + service registration ────────────────────────────
    let (cfg, mut registration) = if boot_cfg.bootstrap_token.is_empty() {
        // Direct mode: assemble config from env, then register in KV.
        let outcome = bootstrap_runtime_config::<MdgwService>(&boot_cfg, BootstrapMode::Direct)
            .expect("config assembly failed");
        let cfg = outcome.runtime_config;

        info!(
            mdgw_id = %cfg.mdgw_id,
            venue = %cfg.venue,
            grpc_port = cfg.grpc_port,
            source = "direct",
            "zk-rtmd-gw-svc config assembled"
        );

        // Build KV registration blob (needs venue from runtime config).
        let kv_key = format!("{}.{}", cfg.gateway_kv_prefix, cfg.mdgw_id);
        let kv_value = mdgw_registration_value(&cfg);

        let registration = ServiceRegistration::register_direct(
            &js,
            kv_key.clone(),
            kv_value,
            Duration::from_secs(15),
        )
        .await
        .expect("failed to register in NATS KV");
        info!(kv_key = %kv_key, "registered in NATS KV (direct)");

        (cfg, registration)
    } else {
        // Pilot mode: request Pilot grant first, then assemble config and register.
        info!("requesting Pilot bootstrap grant");
        let grpc_address = format!("{}:{}", boot_cfg.grpc_host, boot_cfg.grpc_port);
        let mut runtime_info = std::collections::HashMap::new();
        runtime_info.insert("grpc_address".into(), grpc_address);

        let grant = ServiceRegistration::pilot_request(
            &nats_client,
            &boot_cfg.bootstrap_token,
            &boot_cfg.mdgw_id,
            &boot_cfg.instance_type,
            &boot_cfg.env,
            runtime_info,
        )
        .await
        .expect("Pilot bootstrap request failed — is Pilot running?");

        let outcome = bootstrap_runtime_config::<MdgwService>(
            &boot_cfg,
            BootstrapMode::Pilot {
                payload: grant.payload.clone(),
                validate_hash: false, // enable once normalization contract is confirmed
            },
        )
        .expect("Pilot config assembly failed");
        let cfg = outcome.runtime_config;

        info!(
            mdgw_id = %cfg.mdgw_id,
            venue = %cfg.venue,
            grpc_port = cfg.grpc_port,
            source = "pilot",
            "zk-rtmd-gw-svc config assembled"
        );

        let kv_value = mdgw_registration_value(&cfg);
        let registration = ServiceRegistration::register_kv_with_grant(
            &nats_client,
            &js,
            &grant,
            kv_value,
            Duration::from_secs(15),
        )
        .await
        .expect("Pilot KV registration failed");

        info!(
            kv_key = registration.grant().kv_key,
            session_id = registration.grant().owner_session_id,
            "registered via Pilot grant"
        );

        (cfg, registration)
    };

    // ── 3b. Resolve venue config secrets (Python venues) ────────────────────
    // Read secret_ref from venue_config → fetch whole Vault document →
    // project allowed keys into venue_config. Venue-specific mapping lives
    // here, NOT in zk-infra-rs.
    #[cfg(feature = "python-venue")]
    let mut cfg = cfg;
    #[cfg(feature = "python-venue")]
    if let Some(ref venue_root) = cfg.venue_root {
        let root = std::path::PathBuf::from(venue_root);
        if let Ok(manifest) = zk_pyo3_bridge::manifest::load_manifest(&root, &cfg.venue) {
            if let Ok(cap) =
                zk_pyo3_bridge::manifest::resolve_capability(&manifest, zk_pyo3_bridge::manifest::CAP_RTMD)
            {
                let secret_paths = zk_pyo3_bridge::manifest::secret_ref_paths(cap);
                if !secret_paths.is_empty() {
                    let resolver: Box<dyn zk_infra_rs::vault::SecretResolver> =
                        if let Some(identity) = zk_infra_rs::vault::VaultIdentity::from_env() {
                            Box::new(
                                zk_infra_rs::vault::VaultSecretResolver::login(&identity)
                                    .await
                                    .expect("Vault login failed"),
                            )
                        } else {
                            Box::new(zk_infra_rs::vault::DevSecretResolver::new())
                        };

                    for pointer in &secret_paths {
                        if let Some(vault_path) =
                            zk_infra_rs::vault::extract_secret_ref(&cfg.venue_config, pointer)
                        {
                            let doc = resolver
                                .read_document(&vault_path)
                                .await
                                .expect("failed to read secret document from Vault");

                            // Host-side projection: map Vault keys → adaptor config keys.
                            let projections: &[(&str, &str)] = match cfg.venue.as_str() {
                                "oanda" => &[("apikey", "token")],
                                _ => &[],
                            };
                            let obj = cfg
                                .venue_config
                                .as_object_mut()
                                .expect("venue_config must be a JSON object");
                            for &(vault_key, config_key) in projections {
                                if let Some(val) = doc.get(vault_key) {
                                    obj.insert(
                                        config_key.into(),
                                        serde_json::Value::String(val.clone()),
                                    );
                                }
                            }
                        }
                    }
                    info!("venue config secrets resolved");
                }
            }
        }
    }

    // ── Venue adapter ────────────────────────────────────────────────────────
    let adapter = {
        #[cfg(feature = "python-venue")]
        {
            if let Some(ref venue_root) = cfg.venue_root {
                let root = std::path::PathBuf::from(venue_root);
                // Fail-fast: if venue_root is set, manifest must load successfully.
                let manifest =
                    zk_pyo3_bridge::manifest::load_manifest(&root, &cfg.venue)?;
                // Capability miss is OK — venue may only declare gw/refdata, not rtmd.
                let rtmd_cap = zk_pyo3_bridge::manifest::resolve_capability(
                    &manifest,
                    zk_pyo3_bridge::manifest::CAP_RTMD,
                )
                .ok()
                .filter(|cap| cap.language == "python");

                if let Some(cap) = rtmd_cap {
                    zk_pyo3_bridge::manifest::validate_config(
                        &root, &cfg.venue, cap, &cfg.venue_config,
                    )?;
                    let ep =
                        zk_pyo3_bridge::manifest::parse_python_entrypoint(&cap.entrypoint)?;
                    let rt = zk_pyo3_bridge::py_runtime::PyRuntime::initialize(&root)?;
                    let handle =
                        rt.load_class(&ep, cfg.venue_config.clone(), Some(&cfg.venue))?;
                    Arc::new(zk_pyo3_bridge::rtmd_adapter::PyRtmdVenueAdapter::new(handle))
                        as Arc<dyn zk_rtmd_rs::venue_adapter::RtmdVenueAdapter>
                } else {
                    build_adapter(&cfg.venue, &cfg.venue_config)?
                }
            } else {
                build_adapter(&cfg.venue, &cfg.venue_config)?
            }
        }
        #[cfg(not(feature = "python-venue"))]
        {
            build_adapter(&cfg.venue, &cfg.venue_config)?
        }
    };
    adapter.connect().await?;
    info!(venue = %cfg.venue, "venue adapter connected");

    // ── Subscription KV bucket ───────────────────────────────────────────────
    let sub_bucket = &cfg.rtmd_sub_bucket;
    let sub_ttl = Duration::from_secs(cfg.sub_lease_ttl_s * 3);
    let sub_store = match js.get_key_value(sub_bucket).await {
        Ok(s) => s,
        Err(_) => js
            .create_key_value(async_nats::jetstream::kv::Config {
                bucket: sub_bucket.to_string(),
                max_age: sub_ttl,
                ..Default::default()
            })
            .await?,
    };

    // ── Subscription manager ─────────────────────────────────────────────────
    let sub_source = Arc::new(NatsKvSubSource::new(sub_store, cfg.venue.clone()));
    let sub_mgr = Arc::new(SubscriptionManager::new(
        Arc::clone(&adapter),
        Arc::clone(&sub_source) as _,
    ));
    sub_mgr.reconcile_on_start().await?;
    info!("subscription reconciliation complete");

    // Spawn subscription watch loop.
    let sub_mgr_watch = Arc::clone(&sub_mgr);
    tokio::spawn(async move {
        sub_mgr_watch.run_watch_loop().await;
    });

    // ── Event loop: adapter events → NATS publisher ──────────────────────────
    let publisher = Arc::new(RtmdNatsPublisher::new(
        nats_client.clone(),
        cfg.venue.clone(),
        Arc::clone(&adapter),
    ));
    let adapter_events = Arc::clone(&adapter);
    let publisher_events = Arc::clone(&publisher);
    tokio::spawn(async move {
        loop {
            match adapter_events.next_event().await {
                Ok(event) => {
                    publisher_events.publish(event).await;
                }
                Err(zk_rtmd_rs::types::RtmdError::ChannelClosed) => {
                    tracing::info!("adapter event channel closed, event loop exiting");
                    break;
                }
                Err(e) => {
                    tracing::error!(error = %e, "event loop error");
                    break;
                }
            }
        }
    });

    // ── Bind gRPC listener before serving ────────────────────────────────────
    let grpc_addr: SocketAddr = format!("0.0.0.0:{}", cfg.grpc_port).parse()?;
    let listener = tokio::net::TcpListener::bind(grpc_addr).await?;
    let local_addr = listener.local_addr()?;
    info!(%local_addr, "gRPC listener bound");

    // ── gRPC server (serve on already-bound listener) ────────────────────────
    let handler = RtmdQueryHandler { adapter: Arc::clone(&adapter) };
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    info!(mdgw_id = %cfg.mdgw_id, "zk-rtmd-gw-svc LIVE");

    let grpc_server = tonic::transport::Server::builder()
        .add_service(RtmdQueryServiceServer::new(handler))
        .serve_with_incoming(incoming);

    let fenced = tokio::select! {
        r = grpc_server => {
            r?;
            false
        }
        _ = tokio::signal::ctrl_c() => {
            info!("shutdown signal received");
            false
        }
        _ = registration.wait_fenced() => {
            tracing::warn!("KV fencing detected — shutting down");
            true
        }
    };

    if !fenced {
        registration.deregister().await.ok();
    }

    info!("zk-rtmd-gw-svc stopped");
    Ok(())
}

fn mdgw_capabilities() -> Vec<String> {
    vec![
        "tick".into(),
        "kline".into(),
        "funding".into(),
        "orderbook".into(),
        "query_current".into(),
        "query_history".into(),
    ]
}

fn mdgw_metadata() -> std::collections::HashMap<String, String> {
    let mut m = std::collections::HashMap::new();
    m.insert("publisher_mode".into(), "standalone".into());
    m.insert("subscription_scope".into(), "global".into());
    m.insert(
        "query_types".into(),
        "current_tick,current_orderbook,current_funding,kline_history".into(),
    );
    m
}

fn mdgw_registration_value(
    cfg: &zk_rtmd_gw_svc::config::MdgwRuntimeConfig,
) -> bytes::Bytes {
    let grpc_address = format!("{}:{}", cfg.grpc_host, cfg.grpc_port);
    let reg_proto = discovery_registration::mdgw_registration(
        &cfg.mdgw_id,
        &grpc_address,
        &cfg.venue,
        mdgw_capabilities(),
        mdgw_metadata(),
    );
    discovery_registration::encode_registration(&reg_proto)
}
