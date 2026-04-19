use std::path::PathBuf;
use std::sync::Once;

use pyo3::prelude::*;
use pyo3::types::PyList;
use zk_pyo3_bridge::manifest;
use zk_pyo3_bridge::py_runtime::PyRuntime;
use zk_pyo3_bridge::refdata_adapter::{PyRefdataLoader, RefdataLoader};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Test-only: prepend the fixtures dir to sys.path so flat fixture modules
/// (e.g. `fake_refdata`) are importable by the embedded interpreter.
///
/// Production code never mutates sys.path — see dependency-contract.md.
/// This helper exists solely so integration tests can ship local fixture
/// packages without having to install them as wheels.
fn init_runtime() -> PyRuntime {
    static PATH_SET: Once = Once::new();
    let rt = PyRuntime::initialize().expect("failed to initialize Python runtime");
    PATH_SET.call_once(|| {
        Python::with_gil(|py| {
            let sys = py.import_bound("sys").unwrap();
            let sys_path = sys.getattr("path").unwrap();
            let path: &Bound<'_, PyList> = sys_path.downcast::<PyList>().unwrap();
            path.insert(0, fixtures_dir().to_string_lossy().to_string())
                .unwrap();
        });
    });
    rt
}

// ─── Manifest tests ─────────────────────────────────────────────────────────

#[test]
fn test_manifest_parse_test_venue() {
    let mf = manifest::load_manifest(&fixtures_dir(), "test_venue").unwrap();
    assert_eq!(mf.venue, "test_venue");
    assert_eq!(mf.version, 1);
    assert!(mf.capabilities.contains_key("gw"));
    assert!(mf.capabilities.contains_key("rtmd"));
    assert!(mf.capabilities.contains_key("refdata"));
}

#[test]
fn test_manifest_resolve_capability() {
    let mf = manifest::load_manifest(&fixtures_dir(), "test_venue").unwrap();
    let cap = manifest::resolve_capability(&mf, "refdata").unwrap();
    assert_eq!(cap.language, "python");
    assert_eq!(cap.entrypoint, "python:fake_refdata:FakeRefdataLoader");
}

#[test]
fn test_manifest_missing_capability() {
    let mf = manifest::load_manifest(&fixtures_dir(), "test_venue").unwrap();
    assert!(manifest::resolve_capability(&mf, "nonexistent").is_err());
}

#[test]
fn test_entrypoint_parse() {
    let ep = manifest::parse_python_entrypoint("python:fake_refdata:FakeRefdataLoader").unwrap();
    assert_eq!(ep.module_path, "fake_refdata");
    assert_eq!(ep.class_name, "FakeRefdataLoader");
}

// ─── Python runtime tests ───────────────────────────────────────────────────

#[test]
fn test_py_runtime_loads_fixture_class() {
    let rt = init_runtime();
    let ep = manifest::parse_python_entrypoint("python:fake_refdata:FakeRefdataLoader").unwrap();
    let handle = rt.load_class(&ep, serde_json::json!({}));
    assert!(handle.is_ok(), "failed to load class: {:?}", handle.err());
}

#[test]
fn test_py_runtime_bad_module_returns_error() {
    let rt = init_runtime();
    let ep = manifest::parse_python_entrypoint("python:nonexistent_module:SomeClass").unwrap();
    let result = rt.load_class(&ep, serde_json::json!({}));
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("import") || err.contains("module"),
        "error should mention import failure: {err}"
    );
}

// ─── Refdata adapter tests ──────────────────────────────────────────────────

#[tokio::test]
async fn test_refdata_loader_end_to_end() {
    let rt = init_runtime();
    let ep = manifest::parse_python_entrypoint("python:fake_refdata:FakeRefdataLoader").unwrap();
    let handle = rt.load_class(&ep, serde_json::json!({})).unwrap();
    let loader = PyRefdataLoader::new(handle);

    let instruments = loader.load_instruments().await.unwrap();
    assert_eq!(instruments.len(), 2);
    assert_eq!(instruments[0].instrument_id, "BTC-USDT-PERP");
    assert_eq!(instruments[0].instrument_id_exchange, "BTC-USDT-SWAP");
    assert_eq!(instruments[1].instrument_id, "ETH-USDT-SPOT");
}

#[tokio::test]
async fn test_refdata_loader_market_sessions() {
    let rt = init_runtime();
    let ep = manifest::parse_python_entrypoint("python:fake_refdata:FakeRefdataLoader").unwrap();
    let handle = rt.load_class(&ep, serde_json::json!({})).unwrap();
    let loader = PyRefdataLoader::new(handle);

    let sessions = loader.load_market_sessions().await.unwrap();
    assert!(sessions.is_empty());
}

// ─── Gateway adapter tests ──────────────────────────────────────────────────

#[tokio::test]
async fn test_gw_adapter_place_order_smoke() {
    use zk_gw_types::*;
    use zk_pyo3_bridge::gw_adapter::PyVenueAdapter;

    let rt = init_runtime();
    let ep = manifest::parse_python_entrypoint("python:fake_gw:FakeGatewayAdaptor").unwrap();
    let handle = rt.load_class(&ep, serde_json::json!({})).unwrap();
    let adapter = PyVenueAdapter::new(handle);

    adapter.connect().await.unwrap();

    let ack = adapter
        .place_order(VenuePlaceOrder {
            correlation_id: 42,
            exch_account_id: "test".into(),
            instrument: "BTC-USDT-PERP".into(),
            buysell_type: 1,
            openclose_type: 0,
            order_type: 1,
            price: 50000.0,
            qty: 0.01,
            leverage: 10.0,
            timestamp: 1234567890,
        })
        .await
        .unwrap();

    assert!(ack.success);
    assert_eq!(ack.exch_order_ref.as_deref(), Some("FAKE-42"));
}

#[tokio::test]
async fn test_gw_adapter_query_balance() {
    use zk_gw_types::*;
    use zk_pyo3_bridge::gw_adapter::PyVenueAdapter;

    let rt = init_runtime();
    let ep = manifest::parse_python_entrypoint("python:fake_gw:FakeGatewayAdaptor").unwrap();
    let handle = rt.load_class(&ep, serde_json::json!({})).unwrap();
    let adapter = PyVenueAdapter::new(handle);

    let balances = adapter
        .query_balance(VenueBalanceQuery {
            explicit_symbols: vec![],
        })
        .await
        .unwrap();

    assert_eq!(balances.len(), 1);
    assert_eq!(balances[0].asset, "USDT");
    assert_eq!(balances[0].total_qty, 10000.0);
}

#[tokio::test]
async fn test_gw_adapter_python_exception_maps_to_error() {
    use zk_gw_types::*;
    use zk_pyo3_bridge::gw_adapter::PyVenueAdapter;

    let rt = init_runtime();
    let ep = manifest::parse_python_entrypoint("python:fake_gw:ErrorGatewayAdaptor").unwrap();
    let handle = rt.load_class(&ep, serde_json::json!({})).unwrap();
    let adapter = PyVenueAdapter::new(handle);

    let result = adapter
        .place_order(VenuePlaceOrder {
            correlation_id: 1,
            exch_account_id: "test".into(),
            instrument: "BTC".into(),
            buysell_type: 1,
            openclose_type: 0,
            order_type: 1,
            price: 100.0,
            qty: 1.0,
            leverage: 1.0,
            timestamp: 0,
        })
        .await;

    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("simulated venue error"),
        "should contain Python error message: {err}"
    );
}

// ─── GW idle-timeout resilience ─────────────────────────────────────────────

#[tokio::test]
async fn test_gw_next_event_survives_idle_timeout() {
    use std::sync::Arc;
    use std::time::Duration;
    use pyo3::prelude::*;
    use pyo3::types::{PyDict, PyDictMethods, PyList};
    use zk_gw_types::*;
    use zk_pyo3_bridge::gw_adapter::PyVenueAdapter;

    let rt = init_runtime();
    // Use IdleTimeoutGatewayAdaptor: its next_event() uses wait_for() so
    // timed-out coroutines complete and don't steal events from later retries.
    let ep =
        manifest::parse_python_entrypoint("python:fake_gw:IdleTimeoutGatewayAdaptor").unwrap();
    let handle = rt.load_class(&ep, serde_json::json!({})).unwrap();

    // Clone references before the adapter takes ownership.
    let obj_ref = Python::with_gil(|py| handle.inner.clone_ref(py));
    let event_loop = Arc::clone(&handle.event_loop);

    let mut adapter = PyVenueAdapter::new(handle);
    // Use a short timeout so we exercise the retry loop quickly.
    // The fixture's internal timeout is 1s; the bridge timeout is 2s.
    // The Python TimeoutError fires at 1s, bridge catches it and retries.
    adapter.set_event_timeout(Duration::from_secs(2));
    adapter.connect().await.unwrap();

    // Push an event after 5s — at least 2 idle-timeout cycles will have fired.
    let push_obj = Python::with_gil(|py| obj_ref.clone_ref(py));
    let push_loop = Arc::clone(&event_loop);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        tokio::task::spawn_blocking(move || {
            Python::with_gil(|py| {
                let event = PyDict::new_bound(py);
                event.set_item("event_type", "balance").unwrap();
                let d = PyDict::new_bound(py);
                d.set_item("asset", "USDT").unwrap();
                d.set_item("total_qty", 42.0f64).unwrap();
                d.set_item("avail_qty", 42.0f64).unwrap();
                d.set_item("frozen_qty", 0.0f64).unwrap();
                let payload = PyList::new_bound(py, &[d]);
                event.set_item("payload", payload).unwrap();

                let coro = push_obj
                    .call_method1(py, "push_event", (event,))
                    .expect("push_event call failed");
                push_loop
                    .run_coroutine(py, coro.into_bound(py), 5.0)
                    .expect("push_event coroutine failed");
            });
        })
        .await
        .unwrap();
    });

    // next_event must survive multiple 2s timeouts and eventually return.
    let event = tokio::time::timeout(Duration::from_secs(15), adapter.next_event())
        .await
        .expect("test timed out — next_event did not return")
        .expect("next_event returned an error");

    match event {
        VenueEvent::Balance(balances) => {
            assert_eq!(balances.len(), 1);
            assert_eq!(balances[0].asset, "USDT");
            assert_eq!(balances[0].total_qty, 42.0);
        }
        other => panic!("expected Balance event, got: {other:?}"),
    }
}

// ─── RTMD adapter tests ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_rtmd_adapter_subscribe_smoke() {
    use zk_pyo3_bridge::rtmd_adapter::PyRtmdVenueAdapter;
    use zk_rtmd_rs::types::*;
    use zk_rtmd_rs::venue_adapter::RtmdVenueAdapter;

    let rt = init_runtime();
    let ep = manifest::parse_python_entrypoint("python:fake_rtmd:FakeRtmdAdaptor").unwrap();
    let handle = rt.load_class(&ep, serde_json::json!({})).unwrap();
    let adapter = PyRtmdVenueAdapter::new(handle);

    adapter.connect().await.unwrap();

    let spec = RtmdSubscriptionSpec {
        stream_key: StreamKey {
            instrument_code: "BTC-USDT".into(),
            channel: ChannelType::Tick,
        },
        instrument_exch: "BTC-USDT".into(),
        venue: "test".into(),
    };

    adapter.subscribe(spec).await.unwrap();

    let active = adapter.snapshot_active().await.unwrap();
    assert_eq!(active.len(), 1);
}

#[test]
fn test_rtmd_adapter_instrument_exch_for() {
    use zk_pyo3_bridge::rtmd_adapter::PyRtmdVenueAdapter;
    use zk_rtmd_rs::venue_adapter::RtmdVenueAdapter;

    let rt = init_runtime();
    let ep = manifest::parse_python_entrypoint("python:fake_rtmd:FakeRtmdAdaptor").unwrap();
    let handle = rt.load_class(&ep, serde_json::json!({})).unwrap();
    let adapter = PyRtmdVenueAdapter::new(handle);

    let result = adapter.instrument_exch_for("BTC-USDT");
    assert_eq!(result, Some("FAKE-BTC-USDT".to_string()));
}
