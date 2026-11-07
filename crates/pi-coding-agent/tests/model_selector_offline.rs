//! Opening `/model` with `--offline` / `PI_OFFLINE` re-reads models.json but sends no catalog
//! request. The interactive entry point turns `--offline` into `PI_OFFLINE=1` before the model
//! runtime is created; the runtime reads it once (TS `process.env.PI_OFFLINE === undefined`).
//!
//! This binary holds a single test because it sets `PI_OFFLINE` for the process.

#[path = "../../pi-ai/tests/mock_http/mod.rs"]
mod mock_http;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mock_http::{MockServer, Request, Response};
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::keybindings::KeybindingsManager;
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use pi_coding_agent::core::models_store::InMemoryCodingAgentModelsStore;
use pi_coding_agent::modes::interactive::components::model_selector::ModelSelectorComponent;
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_tui::keybindings::set_keybindings;
use pi_tui::tui::Component;
use serde_json::{Value, json};

fn render_plain(component: &dyn Component) -> String {
    let text = component.render(120).join("\n");
    pi_coding_agent::utils::ansi::strip_ansi(&text).unwrap_or(text)
}

/// Wait until the selector's background refresh has settled (success or a reported failure).
async fn wait_for_refresh(selector: &ModelSelectorComponent) -> String {
    let started = Instant::now();
    loop {
        let rendered = render_plain(selector);
        if rendered.contains("Model catalogs refreshed.") || rendered.contains("showing cached models") {
            return rendered;
        }
        assert!(started.elapsed() < Duration::from_secs(5), "the catalog refresh did not settle:\n{rendered}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn models_json(path: &Path, providers: &[&str]) {
    let providers: serde_json::Map<String, Value> = providers
        .iter()
        .map(|id| {
            let entry = json!({"baseUrl": "http://127.0.0.1:9/v1", "api": "openai-completions", "apiKey": "local-key",
                               "models": [{"id": format!("{id}-model")}]});
            (id.to_string(), entry)
        })
        .collect();
    std::fs::write(path, json!({ "providers": providers }).to_string()).unwrap();
}

/// A runtime whose remote catalogs (built-in providers) are served by `catalog`, with a stored
/// key for one built-in provider so its catalog is refreshable.
async fn runtime(dir: &Path, catalog: &MockServer) -> Arc<ModelRuntime> {
    let auth_path = dir.join("auth.json");
    std::fs::write(&auth_path, json!({"anthropic": {"type": "api_key", "key": "sk-test-placeholder"}}).to_string()).unwrap();
    ModelRuntime::create(Some(&CreateModelRuntimeOptions {
        credentials: Some(AuthStorage::create(Some(&auth_path.to_string_lossy()))),
        models_path: Some(dir.join("models.json").to_string_lossy().into_owned()),
        models_store: Some(Arc::new(InMemoryCodingAgentModelsStore::new())),
        allow_model_network: Some(false),
        catalog_base_url: Some(catalog.url.clone()),
        ..Default::default()
    }))
    .await
    .unwrap()
}

async fn open_model_selector(runtime: &Arc<ModelRuntime>) -> String {
    let tui = pi_tui::tui::Tui::main_screen(Arc::new(pi_tui::testing::VirtualTerminal::new(80, 24)));
    // Filter to the models.json providers: built-in catalogs fill the visible rows otherwise.
    let selector =
        ModelSelectorComponent::new(tui, None, runtime.clone(), &[], Arc::new(|_| {}), Arc::new(|| {}), Some("local"), None, None);
    let rendered = wait_for_refresh(&selector).await;
    selector.dispose();
    rendered
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_selector_refresh_reloads_models_json_without_network_when_offline() {
    init_theme(Some("dark"), Some(false));
    set_keybindings(KeybindingsManager::new(None, None).tui_manager());
    let catalog = MockServer::start(|_request: &Request, _| Response::json(200, json!({}))).await;

    // Control: with network allowed, opening `/model` refreshes the remote catalog.
    let online = tempfile::tempdir().unwrap();
    models_json(&online.path().join("models.json"), &["local-a"]);
    let online_runtime = runtime(online.path(), &catalog).await;
    assert!(catalog.requests().is_empty(), "creation with allow_model_network=false stays offline");
    open_model_selector(&online_runtime).await;
    let online_requests = catalog.requests().len();
    assert!(online_requests > 0, "the control run never reached the catalog server; the offline check below would prove nothing");

    // SAFETY: this binary runs a single test, and no other thread reads the environment here.
    unsafe { std::env::set_var("PI_OFFLINE", "1") };
    let offline = tempfile::tempdir().unwrap();
    models_json(&offline.path().join("models.json"), &["local-a"]);
    let offline_runtime = runtime(offline.path(), &catalog).await;
    models_json(&offline.path().join("models.json"), &["local-b"]);
    let rendered = open_model_selector(&offline_runtime).await;
    // SAFETY: as above.
    unsafe { std::env::remove_var("PI_OFFLINE") };

    assert!(rendered.contains("local-b-model [local-b]"), "models.json was not re-read:\n{rendered}");
    assert!(!rendered.contains("local-a-model [local-a]"), "{rendered}");
    assert!(!rendered.contains("Could not refresh"), "offline refresh reported a network failure:\n{rendered}");
    let paths: Vec<String> = catalog.requests()[online_requests..].iter().map(|r| r.path.clone()).collect();
    assert!(paths.is_empty(), "offline /model refresh sent catalog requests: {paths:?}");
}
