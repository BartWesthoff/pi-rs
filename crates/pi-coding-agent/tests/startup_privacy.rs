//! Startup privacy: with default settings a fresh start contacts nothing, and a catalog request is
//! made only when one is asked for explicitly.
//!
//! `/refresh-models` and `pi update --models` both end in `ModelRuntime::refresh` with
//! `allow_network: Some(true)`, which the runtime tests drive directly against pi-ai's local
//! `mock_http` server (the catalog base URL points at it). The interactive tests route every
//! outgoing HTTP client through the same server with the proxy environment, so a startup contact
//! would appear as a recorded `CONNECT pi.dev:443` line.

#[path = "../../pi-ai/tests/mock_http/mod.rs"]
mod mock_http;
mod support;

use std::sync::Arc;
use std::time::Duration;

use mock_http::{MockServer, Request, Response};
use pi_ai::auth::types::{ApiKeyCredential, Credential};
use pi_ai::models::ModelsRefreshOptions;
use pi_coding_agent::core::agent_session_runtime::{AgentSessionRuntime, CreateAgentSessionRuntimeFactory};
use pi_coding_agent::core::agent_session_services::AgentSessionServices;
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::extensions::types::ProviderConfig;
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use pi_coding_agent::modes::interactive::interactive_mode::{InteractiveMode, InteractiveModeOptions};
use pi_tui::terminal::Terminal;
use pi_tui::testing::VirtualTerminal;
use serde_json::{Value, json};
use support::*;

/// Every test in this binary mutates the process environment (proxy, telemetry), so they run one at
/// a time.
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const PROXY_VARS: [&str; 12] = [
    "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy", "NO_PROXY", "no_proxy",
    "PI_TELEMETRY", "PI_OFFLINE", "PI_SKIP_VERSION_CHECK", "PI_CODING_AGENT_DIR",
];

/// Drop proxy/telemetry environment an earlier test may have set.
fn clear_env() {
    // SAFETY: callers hold LOCK.
    unsafe {
        for var in PROXY_VARS {
            std::env::remove_var(var);
        }
    }
}

fn catalog_response() -> Response {
    Response::json(200, json!({ "fresh-model": { "id": "fresh-model", "name": "Fresh" } }))
}

fn credentials() -> Arc<AuthStorage> {
    let mut entries = indexmap::IndexMap::new();
    entries.insert("openai".to_string(), Credential::ApiKey(ApiKeyCredential { key: Some("test-key".into()), env: None }));
    AuthStorage::in_memory(Some(&entries))
}

fn catalog_paths(requests: &[Request]) -> Vec<String> {
    requests.iter().filter(|r| r.path_only().starts_with("/api/models/providers/")).map(|r| r.path.clone()).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_runtime_creation_makes_no_catalog_request() {
    let _lock = LOCK.lock().await;
    clear_env();
    let server = MockServer::start(|_, _| catalog_response()).await;
    let _runtime = ModelRuntime::create(Some(&CreateModelRuntimeOptions {
        credentials: Some(credentials()),
        models_path: Some(String::new()),
        catalog_base_url: Some(server.url.clone()),
        ..Default::default()
    }))
    .await
    .unwrap();
    // The create-time refresh is offline-only, so a start with default settings sends nothing.
    assert!(server.requests().is_empty(), "a default start contacted the catalog server: {:?}", server.requests().len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_refresh_contacts_the_catalog_server() {
    let _lock = LOCK.lock().await;
    clear_env();
    let server = MockServer::start(|_, _| catalog_response()).await;
    let runtime = ModelRuntime::create(Some(&CreateModelRuntimeOptions {
        credentials: Some(credentials()),
        models_path: Some(String::new()),
        catalog_base_url: Some(server.url.clone()),
        refresh_on_create: Some(false),
        ..Default::default()
    }))
    .await
    .unwrap();
    assert!(server.requests().is_empty(), "create must not fetch the catalog");

    runtime.refresh(Some(ModelsRefreshOptions { allow_network: Some(true), ..Default::default() })).await.unwrap();
    assert!(!catalog_paths(&server.requests()).is_empty(), "an explicit refresh must fetch the catalog");
}

// --- interactive startup ------------------------------------------------------------------------

struct Fixture {
    harness: Harness,
    mode: InteractiveMode,
    agent_dir: std::path::PathBuf,
}

impl Fixture {
    /// Route every HTTP client through `server` and start interactive mode. `telemetry` is the
    /// `PI_TELEMETRY` value (`None` leaves the variable unset).
    async fn new(server: &MockServer, telemetry: Option<&str>) -> Fixture {
        let agent_dir = std::env::temp_dir().join(format!("pi-startup-privacy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&agent_dir).unwrap();
        // SAFETY: tests in this binary are serialized by LOCK.
        unsafe {
            std::env::set_var("PI_CODING_AGENT_DIR", &agent_dir);
            // Deliberately not offline: a refresh would be allowed to run, so "no request" means
            // nothing asked for one rather than the network being switched off.
            std::env::remove_var("PI_OFFLINE");
            std::env::set_var("PI_SKIP_VERSION_CHECK", "1");
            for var in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"] {
                std::env::set_var(var, &server.url);
            }
            std::env::remove_var("NO_PROXY");
            std::env::remove_var("no_proxy");
            match telemetry {
                Some(value) => std::env::set_var("PI_TELEMETRY", value),
                None => std::env::remove_var("PI_TELEMETRY"),
            }
        }
        let settings: Value = json!({ "quietStartup": true });
        let harness = create_harness(HarnessOptions { settings: Some(settings), ..Default::default() }).await.unwrap();
        let session = harness.session.clone();
        let services = AgentSessionServices {
            cwd: harness.session_manager.get_cwd(),
            agent_dir: agent_dir.to_string_lossy().into_owned(),
            model_runtime: harness.model_runtime.clone(),
            settings_manager: harness.settings_manager.clone(),
            resource_loader: session.resource_loader(),
            diagnostics: Vec::new(),
        };
        let factory: CreateAgentSessionRuntimeFactory =
            Arc::new(|_| Box::pin(async { Err(anyhow::anyhow!("session replacement is not supported in this test")) }));
        let runtime = Arc::new(AgentSessionRuntime::new(session, &services, &factory, None, None));
        let terminal = VirtualTerminal::new(100, 30);
        let mode = InteractiveMode::new(
            runtime,
            Some(&InteractiveModeOptions {
                terminal: Some(Arc::new(terminal.clone()) as Arc<dyn Terminal>),
                ..Default::default()
            }),
        );
        mode.init().await.unwrap();
        Fixture { harness, mode, agent_dir }
    }

    fn cleanup(self) {
        self.mode.stop(None);
        self.harness.cleanup();
        let _ = std::fs::remove_dir_all(&self.agent_dir);
        // SAFETY: serialized by LOCK.
        unsafe {
            for var in PROXY_VARS {
                std::env::remove_var(var);
            }
        }
    }
}

fn seen(requests: &[Request]) -> Vec<String> {
    requests.iter().map(|r| format!("{} {}", r.method, r.path)).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn default_settings_make_no_request_at_startup() {
    let _lock = LOCK.lock().await;
    let server = MockServer::start(|_, _| Response::json(200, json!({}))).await;
    let fx = Fixture::new(&server, None).await;
    // Give startup, the changelog check and the background availability passes time to run.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    fx.mode.tui().request_render(false);
    let requests = server.requests();
    assert!(requests.is_empty(), "a default start contacted the network: {:?}", seen(&requests));
    fx.cleanup();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opted_in_telemetry_reports_the_install() {
    let _lock = LOCK.lock().await;
    let server = MockServer::start(|_, _| Response::json(200, json!({}))).await;
    let fx = Fixture::new(&server, Some("1")).await;
    for _ in 0..200 {
        if !server.requests().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let requests = server.requests();
    assert!(
        requests.iter().any(|r| r.path.contains("pi.dev")),
        "opted-in telemetry must report the install: {:?}",
        seen(&requests)
    );
    fx.cleanup();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refresh_models_command_fetches_the_catalog() {
    let _lock = LOCK.lock().await;
    let server = MockServer::start(|_, _| Response::json(200, json!({}))).await;
    let fx = Fixture::new(&server, None).await;
    // A refresh only contacts providers it has a credential for; give it one, and confirm that
    // still leaves startup silent.
    fx.harness
        .model_runtime
        .register_provider("openai", &ProviderConfig { api_key: Some("test-key".into()), ..Default::default() })
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(server.requests().is_empty(), "startup fetched the catalog: {:?}", seen(&server.requests()));

    fx.mode.submit("/refresh-models");
    for _ in 0..200 {
        if server.requests().iter().any(|r| r.path.contains("pi.dev")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let requests = server.requests();
    assert!(
        requests.iter().any(|r| r.path.contains("pi.dev")),
        "/refresh-models must fetch the catalog: {:?}",
        seen(&requests)
    );
    fx.cleanup();
}
