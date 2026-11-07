//! pi-rs divergence from TS Pi: opening `/model` re-reads models.json but never sends a catalog
//! request, online or offline — `/refresh-models` is what contacts pi.dev.

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

/// Wait until the selector's local reload has settled (success or a reported failure).
async fn wait_for_refresh(selector: &ModelSelectorComponent) -> String {
    let started = Instant::now();
    loop {
        let rendered = render_plain(selector);
        if rendered.contains("Model catalogs refreshed.") || rendered.contains("showing cached models") {
            return rendered;
        }
        assert!(started.elapsed() < Duration::from_secs(5), "the catalog reload did not settle:\n{rendered}");
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

/// A runtime whose remote catalogs (built-in providers) would be served by `catalog`, with a
/// stored key for one built-in provider so its catalog would be refreshable over the network.
async fn runtime(dir: &Path, catalog: &MockServer) -> Arc<ModelRuntime> {
    let auth_path = dir.join("auth.json");
    std::fs::write(&auth_path, json!({"anthropic": {"type": "api_key", "key": "sk-test-placeholder"}}).to_string()).unwrap();
    ModelRuntime::create(Some(&CreateModelRuntimeOptions {
        credentials: Some(AuthStorage::create(Some(&auth_path.to_string_lossy()))),
        models_path: Some(dir.join("models.json").to_string_lossy().into_owned()),
        models_store: Some(Arc::new(InMemoryCodingAgentModelsStore::new())),
        // Network enabled, but no refresh at creation: only the selector's own choice to stay
        // local keeps the catalog server quiet afterwards.
        allow_model_network: Some(true),
        refresh_on_create: Some(false),
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
async fn model_selector_reloads_models_json_without_contacting_the_catalog() {
    init_theme(Some("dark"), Some(false));
    set_keybindings(KeybindingsManager::new(None, None).tui_manager());
    let catalog = MockServer::start(|_request: &Request, _| Response::json(200, json!({}))).await;

    let dir = tempfile::tempdir().unwrap();
    models_json(&dir.path().join("models.json"), &["local-a"]);
    let runtime = runtime(dir.path(), &catalog).await;
    assert!(catalog.requests().is_empty(), "creating the runtime must not contact the catalog server");
    models_json(&dir.path().join("models.json"), &["local-b"]);

    let rendered = open_model_selector(&runtime).await;

    assert!(rendered.contains("local-b-model [local-b]"), "models.json was not re-read:\n{rendered}");
    assert!(!rendered.contains("local-a-model [local-a]"), "{rendered}");
    assert!(!rendered.contains("Could not refresh"), "the local reload reported a failure:\n{rendered}");
    let paths: Vec<String> = catalog.requests().iter().map(|r| r.path.clone()).collect();
    assert!(paths.is_empty(), "opening /model contacted the catalog server: {paths:?}");
}
