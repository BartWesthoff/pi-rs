//! Ports of the selector component tests: `test/thinking-selector.test.ts`, `test/trust-selector.test.ts`,
//! `test/oauth-selector.test.ts`, `test/model-selector.test.ts`, `test/scoped-models-selector.test.ts`,
//! `test/settings-selector.test.ts`, `test/session-selector-search.test.ts`,
//! `test/session-selector-rename.test.ts`, `test/session-selector-path-delete.test.ts` and
//! `test/tree-selector.test.ts`.

// `setup()` returns a guard that serializes tests touching the global theme/keybindings; it is
// held for the whole test on purpose (each test runs on its own single-threaded runtime).
#![allow(clippy::await_holding_lock)]

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::FutureExt;
use parking_lot::Mutex;
use pi_agent::types::ThinkingLevel;
use pi_ai::auth::types::{AuthCheck, AuthType};
use pi_ai::providers::faux::FauxModelDefinition;
use pi_coding_agent::core::keybindings::KeybindingsManager;
use pi_coding_agent::core::session_manager::{SessionEntry, SessionInfo, SessionTreeNode};
use pi_coding_agent::core::trust_manager::{ProjectTrustStoreEntry, ProjectTrustUpdate};
use pi_coding_agent::modes::interactive::components::model_selector::ModelSelectorComponent;
use pi_coding_agent::modes::interactive::components::oauth_selector::{
    AuthSelectorProvider, AuthSelectorProviderAuthType, OAuthSelectorComponent, OAuthSelectorComponentMode,
};
use pi_coding_agent::modes::interactive::components::scoped_models_selector::{
    ModelsCallbacks, ModelsConfig, ScopedModelsSelectorComponent,
};
use pi_coding_agent::modes::interactive::components::session_selector::{
    SessionSelectorComponent, SessionSelectorComponentOptions, SessionsLoader,
};
use pi_coding_agent::modes::interactive::components::session_selector_search::{
    NameFilter, SortMode, filter_and_sort_sessions,
};
use pi_coding_agent::modes::interactive::components::settings_selector::{
    SettingsCallbacks, SettingsConfig, SettingsConfigDoubleEscapeAction, SettingsConfigSteeringMode,
    SettingsSelectorComponent,
};
use pi_coding_agent::modes::interactive::components::thinking_selector::ThinkingSelectorComponent;
use pi_coding_agent::modes::interactive::components::tree_selector::{FilterMode, TreeSelectorComponent};
use pi_coding_agent::modes::interactive::components::trust_selector::{
    TrustSelection, TrustSelectorComponent, TrustSelectorOptions,
};
use pi_coding_agent::modes::interactive::theme::theme::{TerminalTheme, init_theme};
use pi_tui::keybindings::{KeySpec, set_keybindings};
use pi_tui::tui::Component;
use pi_tui::utils::visible_width;
use serde_json::json;
use support::*;

/// Keybindings and the theme are process-wide; tests in this binary run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn setup() -> parking_lot::MutexGuard<'static, ()> {
    let guard = SERIAL.lock();
    init_theme(Some("dark"), Some(false));
    set_keybindings(KeybindingsManager::new(None, None).tui_manager());
    guard
}

fn set_bindings(bindings: &[(&str, &str)]) {
    let config = bindings.iter().map(|(k, v)| (k.to_string(), Some(KeySpec::from(*v)))).collect();
    set_keybindings(KeybindingsManager::new(Some(config), None).tui_manager());
}

fn strip(text: &str) -> String {
    pi_coding_agent::utils::ansi::strip_ansi(text).unwrap_or_else(|_| text.to_string())
}

fn render_plain(component: &dyn Component, width: usize) -> String {
    strip(&component.render(width).join("\n"))
}

const DOWN: &str = "\x1b[B";
const UP: &str = "\x1b[A";

// =============================================================================
// thinking-selector.test.ts
// =============================================================================

#[test]
fn thinking_selector_keeps_the_current_level_marked_while_browsing() {
    let _g = setup();
    let selector = ThinkingSelectorComponent::new(
        ThinkingLevel::Medium,
        &[ThinkingLevel::Medium, ThinkingLevel::High],
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
    );
    let level_row = |level: &str| -> Option<String> {
        selector.get_select_list().render(80).into_iter().map(|l| strip(&l)).find(|l| l.contains(level))
    };
    assert_eq!(selector.get_select_list().get_selected_item().map(|i| i.label), Some("✓ medium".to_string()));
    assert!(level_row("medium").unwrap().starts_with("→ ✓ medium"));
    selector.handle_input(DOWN);
    assert!(level_row("medium").unwrap().starts_with("  ✓ medium"));
    assert!(level_row("high").unwrap().starts_with("→   high"));
}

#[test]
fn thinking_selector_uses_the_configured_save_binding() {
    let _g = setup();
    set_bindings(&[("app.thinking.save", "ctrl+r")]);
    let saved: Arc<Mutex<Vec<ThinkingLevel>>> = Arc::default();
    let s = saved.clone();
    let selector = ThinkingSelectorComponent::new(
        ThinkingLevel::Medium,
        &[ThinkingLevel::Medium, ThinkingLevel::High],
        Arc::new(|_| {}),
        Arc::new(|| {}),
        Some(Arc::new(move |level| s.lock().push(level))),
        None,
    );
    assert!(render_plain(&selector, 80).contains("Ctrl+R to set as default"));
    selector.handle_input("\x13");
    assert!(saved.lock().is_empty());
    selector.handle_input("\x12");
    assert_eq!(*saved.lock(), vec![ThinkingLevel::Medium]);
}

// =============================================================================
// trust-selector.test.ts
// =============================================================================

fn trust_selector(
    cwd: &str,
    saved: Option<(&str, bool)>,
    trusted: bool,
    on_select: Arc<dyn Fn(TrustSelection) + Send + Sync>,
) -> TrustSelectorComponent {
    TrustSelectorComponent::new(&TrustSelectorOptions {
        cwd: cwd.to_string(),
        saved_decision: saved.map(|(path, decision)| ProjectTrustStoreEntry { path: path.to_string(), decision }),
        project_trusted: trusted,
        on_select,
        on_cancel: Arc::new(|| {}),
    })
}

#[test]
fn trust_selector_keeps_the_saved_trusted_decision_marked_while_browsing() {
    let _g = setup();
    let selector = trust_selector("/project", Some(("/project", true)), true, Arc::new(|_| {}));
    let output = render_plain(&selector, 120);
    assert!(output.contains("Saved decision: trusted (/project)"));
    assert!(output.contains("Current session: trusted"));
    assert!(output.contains("→ ✓ Trust"));

    selector.handle_input(DOWN);
    let output = render_plain(&selector, 120);
    assert!(output.contains("✓ Trust"));
    assert!(output.contains("→   Trust parent folder (/)"));
    assert!(!output.contains("✓ Do not trust"));
}

#[test]
fn trust_selector_selects_a_trust_decision() {
    let _g = setup();
    let selected: Arc<Mutex<Vec<TrustSelection>>> = Arc::default();
    let s = selected.clone();
    let selector = trust_selector("/project", None, false, Arc::new(move |sel| s.lock().push(sel)));
    selector.handle_input("\n");
    let selected = selected.lock();
    assert_eq!(selected.len(), 1);
    assert!(selected[0].trusted);
    assert_eq!(selected[0].updates, vec![ProjectTrustUpdate { path: "/project".into(), decision: Some(true) }]);
}

#[test]
fn trust_selector_labels_saved_ancestor_decisions_as_inherited() {
    let _g = setup();
    let selector = trust_selector("/parent/project/nested", Some(("/parent", true)), true, Arc::new(|_| {}));
    assert!(render_plain(&selector, 120).contains("Saved decision: trusted (inherited from /parent)"));
}

#[test]
fn trust_selector_adds_a_trust_parent_option() {
    let _g = setup();
    let selected: Arc<Mutex<Vec<TrustSelection>>> = Arc::default();
    let s = selected.clone();
    let selector =
        trust_selector("/parent/project", Some(("/parent", true)), true, Arc::new(move |sel| s.lock().push(sel)));
    let output = render_plain(&selector, 120);
    assert!(output.contains("Saved decision: trusted (inherited from /parent)"));
    assert!(output.contains("✓ Trust parent folder (/parent)"));

    selector.handle_input("\n");
    let selected = selected.lock();
    assert!(selected[0].trusted);
    assert_eq!(
        selected[0].updates,
        vec![
            ProjectTrustUpdate { path: "/parent".into(), decision: Some(true) },
            ProjectTrustUpdate { path: "/parent/project".into(), decision: None },
        ]
    );
}

// =============================================================================
// oauth-selector.test.ts
// =============================================================================

fn provider(id: &str, name: &str, status: Option<(AuthType, &str)>) -> AuthSelectorProvider {
    AuthSelectorProvider {
        id: id.into(),
        name: name.into(),
        auth_type: AuthSelectorProviderAuthType::ApiKey,
        method: None,
        status: status.map(|(auth_type, source)| AuthCheck { source: Some(source.to_string()), auth_type }),
        subscription: None,
    }
}

fn oauth_output(p: AuthSelectorProvider) -> String {
    let selector =
        OAuthSelectorComponent::new(OAuthSelectorComponentMode::Login, &[p], Arc::new(|_, _| {}), Arc::new(|| {}), None);
    render_plain(&selector, 120)
}

#[test]
fn oauth_selector_renders_an_option_without_auth_status_as_not_configured() {
    let _g = setup();
    let output = oauth_output(provider("google", "Google", None));
    assert!(output.contains("not configured"));
    assert!(!output.contains("✓ configured"));
}

#[test]
fn oauth_selector_shows_oauth_auth_distinctly_in_the_api_key_selector() {
    let _g = setup();
    let output = oauth_output(provider("anthropic", "Anthropic", Some((AuthType::OAuth, "OAuth"))));
    assert!(output.contains("subscription configured"));
}

#[test]
fn oauth_selector_shows_environment_api_key_auth_as_configured() {
    let _g = setup();
    let output = oauth_output(provider("openai", "OpenAI", Some((AuthType::ApiKey, "OPENAI_API_KEY"))));
    assert!(output.contains("✓ env: OPENAI_API_KEY"));
    assert!(!output.contains("not configured"));
}

#[test]
fn oauth_selector_shows_models_json_api_key_and_command_auth_as_configured() {
    let _g = setup();
    let output = oauth_output(provider("local-proxy", "local-proxy", Some((AuthType::ApiKey, "key in models.json"))));
    assert!(output.contains("✓ key in models.json"));
    let output = oauth_output(provider("op-proxy", "op-proxy", Some((AuthType::ApiKey, "command in models.json"))));
    assert!(output.contains("✓ command in models.json"));
}

// =============================================================================
// model-selector.test.ts
// =============================================================================

fn faux_model(id: &str, name: &str) -> FauxModelDefinition {
    FauxModelDefinition { id: id.into(), name: Some(name.into()), reasoning: Some(true), ..Default::default() }
}

fn fake_tui() -> Arc<pi_tui::tui::Tui> {
    pi_tui::tui::Tui::main_screen(Arc::new(pi_tui::testing::VirtualTerminal::new(80, 24)))
}

#[tokio::test]
async fn model_selector_keeps_the_current_model_marked_while_browsing() {
    let _g = setup();
    let harness = create_harness(HarnessOptions {
        models: vec![faux_model("current-model", "Current Model"), faux_model("browsed-model", "Browsed Model")],
        ..Default::default()
    })
    .await
    .unwrap();
    let current = harness.get_model_by_id("current-model").unwrap();
    let selector = ModelSelectorComponent::new(
        fake_tui(),
        Some(current.clone()),
        harness.model_runtime.clone(),
        &[],
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );
    let row = |id: &str| -> Option<String> {
        render_plain(&selector, 120)
            .split('\n')
            .find(|l| l.contains(&format!("{id} [")))
            .map(|l| l.trim_end().to_string())
    };
    assert_eq!(row("current-model"), Some(format!("→ ✓ current-model [{}]", current.provider)));
    selector.handle_input(DOWN);
    assert_eq!(row("current-model"), Some(format!("  ✓ current-model [{}]", current.provider)));
    // The test environment may expose other configured providers (sorted before or after the faux provider),
    // so the browsed model is reached through the search instead of a fixed number of arrow presses.
    for c in "browsed".chars() {
        selector.handle_input(&c.to_string());
    }
    assert_eq!(row("browsed-model"), Some(format!("→   browsed-model [{}]", current.provider)));
    selector.dispose();
    harness.cleanup();
}

/// The model id on the highlighted (→) row.
fn selected_model_id(rendered: &str) -> Option<String> {
    let line = rendered.split('\n').find(|l| l.starts_with("→ "))?;
    let rest = line.trim_start_matches('→').trim_start();
    let id = rest.split(" [").next()?.trim_start_matches('✓').trim();
    (!id.is_empty()).then(|| id.to_string())
}

/// Wait for the background catalog refresh; TS `vi.waitFor` gives it 1 s.
async fn wait_for_catalog_refresh(selector: &ModelSelectorComponent) -> Duration {
    let started = std::time::Instant::now();
    loop {
        if render_plain(selector, 120).contains("Model catalogs refreshed.") {
            return started.elapsed();
        }
        assert!(started.elapsed() < Duration::from_secs(1), "the catalog refresh did not settle:\n{}", render_plain(selector, 120));
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

// suite/regressions/6999-models-json-hot-reload.test.ts
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_selector_reloads_models_json_when_opened() {
    use pi_coding_agent::core::auth_storage::AuthStorage;
    use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
    use pi_coding_agent::core::models_store::InMemoryCodingAgentModelsStore;

    fn models_json(provider: &str, model: &str) -> String {
        json!({"providers": {provider: {
            "baseUrl": "https://example.test/v1", "api": "openai-completions", "apiKey": "test-key",
            "models": [{"id": model}]
        }}})
        .to_string()
    }

    let _g = setup();
    let dir = tempfile::tempdir().unwrap();
    let models_path = dir.path().join("models.json");
    std::fs::write(&models_path, models_json("old-provider", "old-model")).unwrap();
    let runtime = ModelRuntime::create(Some(&CreateModelRuntimeOptions {
        credentials: Some(AuthStorage::in_memory(None)),
        models_path: Some(models_path.to_string_lossy().into_owned()),
        models_store: Some(Arc::new(InMemoryCodingAgentModelsStore::new())),
        allow_model_network: Some(false),
        ..Default::default()
    }))
    .await
    .unwrap();
    let old = runtime.get_model("old-provider", "old-model").expect("old model loads at startup");

    std::fs::write(&models_path, models_json("new-provider", "new-model")).unwrap();
    // Opening `/model` with the removed model still current must not keep it selectable.
    let selector = ModelSelectorComponent::new(fake_tui(), Some(old), runtime.clone(), &[], Arc::new(|_| {}), Arc::new(|| {}), None, None, None);
    wait_for_catalog_refresh(&selector).await;

    let rendered = render_plain(&selector, 120);
    assert!(rendered.contains("new-model [new-provider]"), "{rendered}");
    assert!(!rendered.contains("old-model [old-provider]"), "{rendered}");
    assert_eq!(selected_model_id(&rendered).as_deref(), Some("new-model"), "{rendered}");
    assert!(runtime.get_model("old-provider", "old-model").is_none());

    // Adding the provider back makes it appear on the next open.
    std::fs::write(
        &models_path,
        json!({"providers": {
            "new-provider": {"baseUrl": "https://example.test/v1", "api": "openai-completions", "apiKey": "test-key", "models": [{"id": "new-model"}]},
            "old-provider": {"baseUrl": "https://example.test/v1", "api": "openai-completions", "apiKey": "test-key", "models": [{"id": "old-model"}]}
        }})
        .to_string(),
    )
    .unwrap();
    selector.dispose();
    let selector = ModelSelectorComponent::new(fake_tui(), None, runtime.clone(), &[], Arc::new(|_| {}), Arc::new(|| {}), None, None, None);
    wait_for_catalog_refresh(&selector).await;
    let rendered = render_plain(&selector, 120);
    assert!(rendered.contains("new-model [new-provider]"), "{rendered}");
    assert!(rendered.contains("old-model [old-provider]"), "{rendered}");
    selector.dispose();
}

// suite/regressions/7209-model-selector-filter-resets-selection.test.ts
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_selector_filter_moves_selection_to_the_first_row_in_the_all_tab() {
    let _g = setup();
    let harness = create_harness(HarnessOptions {
        models: vec![
            faux_model("alpha-1", "Alpha One"),
            faux_model("alpha-2", "Alpha Two"),
            faux_model("alpha-3", "Alpha Three"),
            faux_model("beta-1", "Beta One"),
        ],
        ..Default::default()
    })
    .await
    .unwrap();
    let current = harness.get_model_by_id("alpha-1").unwrap();
    let selector = ModelSelectorComponent::new(
        fake_tui(),
        Some(current),
        harness.model_runtime.clone(),
        &[],
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );
    let elapsed = wait_for_catalog_refresh(&selector).await;
    eprintln!("model catalog refresh settled after {elapsed:?}");

    // Current model (alpha-1) is sorted first, so selection starts on row 0.
    assert_eq!(selected_model_id(&render_plain(&selector, 120)).as_deref(), Some("alpha-1"));
    // Narrow to this provider's models, then move down two rows.
    selector.handle_input(DOWN);
    selector.handle_input(DOWN);
    let moved = selected_model_id(&render_plain(&selector, 120));
    assert_ne!(moved.as_deref(), Some("alpha-1"));

    // Typing a query moves the selection back to the top row, not clamped at index 2.
    for c in "alpha".chars() {
        selector.handle_input(&c.to_string());
    }
    let rendered = render_plain(&selector, 120);
    assert_eq!(selected_model_id(&rendered).as_deref(), Some("alpha-1"), "{rendered}");
    assert!(!rendered.contains("beta-1"), "{rendered}");
    selector.dispose();
    harness.cleanup();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_selector_filter_moves_selection_to_the_first_row_in_the_scoped_tab() {
    use pi_coding_agent::modes::interactive::components::model_selector::ScopedModelItem;
    let _g = setup();
    let harness = create_harness(HarnessOptions {
        models: vec![faux_model("alpha-1", "Alpha One"), faux_model("alpha-2", "Alpha Two"), faux_model("alpha-3", "Alpha Three")],
        ..Default::default()
    })
    .await
    .unwrap();
    let model = |id: &str| harness.get_model_by_id(id).unwrap();
    // The scoped list is not in current-model-first order; the current model sits at index 2.
    let scoped: Vec<ScopedModelItem> = ["alpha-2", "alpha-3", "alpha-1"]
        .iter()
        .map(|id| ScopedModelItem { model: model(id), thinking_level: None })
        .collect();
    let selector = ModelSelectorComponent::new(
        fake_tui(),
        Some(model("alpha-1")),
        harness.model_runtime.clone(),
        &scoped,
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );
    wait_for_catalog_refresh(&selector).await;
    assert_eq!(selected_model_id(&render_plain(&selector, 120)).as_deref(), Some("alpha-1"));
    for c in "alpha".chars() {
        selector.handle_input(&c.to_string());
    }
    assert_eq!(selected_model_id(&render_plain(&selector, 120)).as_deref(), Some("alpha-2"));
    selector.dispose();
    harness.cleanup();
}

#[tokio::test]
async fn model_selector_uses_the_configured_save_binding() {
    let _g = setup();
    set_bindings(&[("app.models.save", "ctrl+r")]);
    let harness = create_harness(HarnessOptions::default()).await.unwrap();
    let current = harness.get_model();
    let saved: Arc<Mutex<Vec<String>>> = Arc::default();
    let s = saved.clone();
    let selector = ModelSelectorComponent::new(
        fake_tui(),
        Some(current.clone()),
        harness.model_runtime.clone(),
        &[],
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        Some(Arc::new(move |m: pi_ai::types::Model| s.lock().push(m.id))),
        None,
    );
    assert!(render_plain(&selector, 120).contains("Ctrl+R to set as default"));
    selector.handle_input("\x13");
    assert!(saved.lock().is_empty());
    selector.handle_input("\x12");
    assert_eq!(*saved.lock(), vec![current.id.clone()]);
    harness.cleanup();
}

// =============================================================================
// scoped-models-selector.test.ts
// =============================================================================

struct ModelState {
    id: &'static str,
    enabled: bool,
}

async fn scoped_selector(
    models: Arc<Mutex<Vec<ModelState>>>,
) -> (ScopedModelsSelectorComponent, Harness) {
    let defs: Vec<FauxModelDefinition> =
        models.lock().iter().map(|m| faux_model(m.id, &m.id.to_uppercase())).collect();
    let harness = create_harness(HarnessOptions { models: defs, ..Default::default() }).await.unwrap();
    let provider = harness.models[0].provider.clone();
    let enabled: Vec<String> =
        models.lock().iter().filter(|m| m.enabled).map(|m| format!("{provider}/{}", m.id)).collect();
    let on_change_models = models.clone();
    let selector = ScopedModelsSelectorComponent::new(
        &ModelsConfig { all_models: harness.models.clone(), enabled_model_ids: Some(enabled), refresh_status: None },
        &ModelsCallbacks {
            on_change: Arc::new(move |ids: Option<Vec<String>>| {
                for model in on_change_models.lock().iter_mut() {
                    model.enabled = match &ids {
                        None => true,
                        Some(ids) => ids.contains(&format!("{provider}/{}", model.id)),
                    };
                }
                async { Ok(()) }.boxed()
            }),
            on_persist: Arc::new(|_| async { Ok(()) }.boxed()),
            on_cancel: Arc::new(|| {}),
        },
    );
    (selector, harness)
}

fn marker_states(selector: &ScopedModelsSelectorComponent, models: &Arc<Mutex<Vec<ModelState>>>) -> Vec<bool> {
    let output = render_plain(selector, 120);
    let lines: Vec<&str> = output.split('\n').collect();
    models
        .lock()
        .iter()
        .map(|model| {
            let line = lines.iter().find(|l| l.contains(&format!("{} [", model.id))).expect("row");
            line.chars().skip(2).collect::<String>().starts_with("✓ ")
        })
        .collect()
}

fn enabled_states(models: &Arc<Mutex<Vec<ModelState>>>) -> Vec<bool> {
    models.lock().iter().map(|m| m.enabled).collect()
}

fn three_models(enabled: [bool; 3]) -> Arc<Mutex<Vec<ModelState>>> {
    Arc::new(Mutex::new(vec![
        ModelState { id: "model-a", enabled: enabled[0] },
        ModelState { id: "model-b", enabled: enabled[1] },
        ModelState { id: "model-c", enabled: enabled[2] },
    ]))
}

#[tokio::test]
async fn scoped_models_marks_every_model_after_enabling_all() {
    let _g = setup();
    let models = three_models([true, false, false]);
    let (selector, harness) = scoped_selector(models.clone()).await;
    selector.handle_input("\x01");
    assert_eq!(enabled_states(&models), vec![true, true, true]);
    assert_eq!(marker_states(&selector, &models), vec![true, true, true]);
    assert!(render_plain(&selector, 120).contains("all enabled"));
    harness.cleanup();
}

#[tokio::test]
async fn scoped_models_disables_only_the_selected_model_after_enabling_all() {
    let _g = setup();
    let models = three_models([true, false, false]);
    let (selector, harness) = scoped_selector(models.clone()).await;
    selector.handle_input("\x01");
    selector.handle_input("\r");
    assert_eq!(enabled_states(&models), vec![false, true, true]);
    assert_eq!(marker_states(&selector, &models), vec![false, true, true]);
    harness.cleanup();
}

#[tokio::test]
async fn scoped_models_enables_only_the_selected_model_after_clearing_all() {
    let _g = setup();
    let models = three_models([true, true, true]);
    let (selector, harness) = scoped_selector(models.clone()).await;
    selector.handle_input("\x18");
    assert_eq!(enabled_states(&models), vec![false, false, false]);
    assert_eq!(marker_states(&selector, &models), vec![false, false, false]);
    selector.handle_input("\r");
    assert_eq!(enabled_states(&models), vec![true, false, false]);
    assert_eq!(marker_states(&selector, &models), vec![true, false, false]);
    harness.cleanup();
}

#[tokio::test]
async fn scoped_models_restores_all_enabled_after_re_enabling_the_last_disabled_model() {
    let _g = setup();
    let models = three_models([true, false, false]);
    let (selector, harness) = scoped_selector(models.clone()).await;
    selector.handle_input("\x01"); // enable all -> null
    selector.handle_input("\r"); // disable model-a; enabled models re-sort first: [b, c, a]
    selector.handle_input(DOWN);
    selector.handle_input(DOWN); // move selection back to model-a
    selector.handle_input("\r"); // re-enable model-a
    assert_eq!(enabled_states(&models), vec![true, true, true]);
    assert_eq!(marker_states(&selector, &models), vec![true, true, true]);
    assert!(render_plain(&selector, 120).contains("all enabled"));
    harness.cleanup();
}

// =============================================================================
// settings-selector.test.ts
// =============================================================================

fn base_config() -> SettingsConfig {
    use pi_coding_agent::compat::WheelScrollLines;
    use pi_coding_agent::core::settings_schema::*;
    SettingsConfig {
        auto_compact: true,
        default_model: "not set".into(),
        current_model: None,
        available_default_models: Vec::new(),
        show_images: false,
        image_width_cells: 80,
        auto_resize_images: true,
        block_images: false,
        enable_skill_commands: true,
        steering_mode: SettingsConfigSteeringMode::OneAtATime,
        follow_up_mode: SettingsConfigSteeringMode::OneAtATime,
        transport: pi_coding_agent::core::settings_manager::TransportSetting::Sse,
        session_share_destination: SessionShareDestinationValue::Auto,
        http_idle_timeout_ms: 300_000,
        cache_warming_mode: CacheWarmingModeValue::Off,
        thinking_level: ThinkingLevel::High.into(),
        available_thinking_levels: Vec::new(),
        model_thinking_levels: Default::default(),
        current_theme: "dark".into(),
        terminal_theme: TerminalTheme::Dark,
        available_themes: Vec::new(),
        hide_thinking_block: false,
        mermaid_rendering_mode: MermaidRenderingModeValue::Streaming,
        show_cache_miss_notices: false,
        collapse_changelog: false,
        enable_install_telemetry: true,
        double_escape_action: SettingsConfigDoubleEscapeAction::Tree,
        tree_filter_mode: FilterMode::Default,
        show_hardware_cursor: false,
        editor_padding_x: 0,
        output_pad: 0,
        autocomplete_max_visible: 5,
        quiet_startup: QuietStartupValue::Bool(false),
        default_project_trust: DefaultProjectTrustValue::Ask,
        clear_on_shrink: false,
        show_terminal_progress: false,
        tui_mode: TuiModeValue::Regular,
        fullscreen_exit_output: FullscreenExitOutputValue::Transcript,
        fullscreen_scrollbar: pi_tui::components::scroll_view::ScrollViewScrollbar::Auto,
        fullscreen_copy_on_select: true,
        fullscreen_wheel_scroll_lines: WheelScrollLines::Lines(7),
        warnings: WarningSettings { anthropic_extra_usage: None },
    }
}

type Recorded = Arc<Mutex<Vec<String>>>;

fn noop_callbacks(record: Recorded) -> SettingsCallbacks {
    let r = move |prefix: &'static str| {
        let record = record.clone();
        move |value: String| record.lock().push(format!("{prefix}:{value}"))
    };
    let exit = r("exit");
    let scrollbar = r("scrollbar");
    let copy = r("copy");
    let wheel = r("wheel");
    let share = r("share");
    let preview = r("preview");
    SettingsCallbacks {
        on_auto_compact_change: Arc::new(|_| {}),
        on_show_images_change: Arc::new(|_| {}),
        on_image_width_cells_change: Arc::new(|_| {}),
        on_auto_resize_images_change: Arc::new(|_| {}),
        on_block_images_change: Arc::new(|_| {}),
        on_enable_skill_commands_change: Arc::new(|_| {}),
        on_steering_mode_change: Arc::new(|_| {}),
        on_follow_up_mode_change: Arc::new(|_| {}),
        on_transport_change: Arc::new(|_| {}),
        on_session_share_destination_change: Arc::new(move |d| share(d.as_str().to_string())),
        on_http_idle_timeout_ms_change: Arc::new(|_| {}),
        on_cache_warming_mode_change: Arc::new(|_| {}),
        on_model_thinking_level_change: Arc::new(|_, _, _| {}),
        on_model_thinking_level_remove: Arc::new(|_, _| {}),
        on_theme_change: Arc::new(|_| {}),
        on_theme_preview: Some(Arc::new(preview)),
        on_hide_thinking_block_change: Arc::new(|_| {}),
        on_mermaid_rendering_mode_change: Arc::new(|_| {}),
        on_show_cache_miss_notices_change: Arc::new(|_| {}),
        on_collapse_changelog_change: Arc::new(|_| {}),
        on_enable_install_telemetry_change: Arc::new(|_| {}),
        on_double_escape_action_change: Arc::new(|_| {}),
        on_tree_filter_mode_change: Arc::new(|_| {}),
        on_show_hardware_cursor_change: Arc::new(|_| {}),
        on_editor_padding_x_change: Arc::new(|_| {}),
        on_output_pad_change: Arc::new(|_| {}),
        on_autocomplete_max_visible_change: Arc::new(|_| {}),
        on_quiet_startup_change: Arc::new(|_| {}),
        on_default_project_trust_change: Arc::new(|_| {}),
        on_clear_on_shrink_change: Arc::new(|_| {}),
        on_show_terminal_progress_change: Arc::new(|_| {}),
        on_tui_mode_change: Arc::new(|_| {}),
        on_fullscreen_exit_output_change: Arc::new(move |o| exit(o.as_str().to_string())),
        on_fullscreen_scrollbar_change: Arc::new(move |s| {
            scrollbar(
                match s {
                    pi_tui::components::scroll_view::ScrollViewScrollbar::Auto => "auto",
                    pi_tui::components::scroll_view::ScrollViewScrollbar::Always => "always",
                    pi_tui::components::scroll_view::ScrollViewScrollbar::Hidden => "hidden",
                }
                .to_string(),
            )
        }),
        on_fullscreen_copy_on_select_change: Arc::new(move |v| copy(v.to_string())),
        on_fullscreen_wheel_scroll_lines_change: Arc::new(move |v| {
            wheel(match v {
                pi_coding_agent::compat::WheelScrollLines::Lines(n) => n.to_string(),
                pi_coding_agent::compat::WheelScrollLines::Auto(a) => a,
            })
        }),
        on_warnings_change: Arc::new(|_| {}),
        on_cancel: Arc::new(|| {}),
    }
}

fn recorded(record: &Recorded, prefix: &str) -> Vec<String> {
    record
        .lock()
        .iter()
        .filter_map(|r| r.strip_prefix(&format!("{prefix}:")).map(str::to_string))
        .collect()
}

#[test]
fn settings_selector_cycles_through_fullscreen_settings() {
    let _g = setup();
    let record: Recorded = Arc::default();
    let config = base_config();
    let callbacks = noop_callbacks(record.clone());
    let cycle = |label: &str, count: usize| {
        let list = SettingsSelectorComponent::new(&config, &callbacks).get_settings_list();
        for c in label.chars() {
            list.handle_input(&c.to_string());
        }
        for _ in 0..count {
            list.handle_input("\r");
        }
    };
    cycle("Fullscreen exit output", 2);
    assert_eq!(recorded(&record, "exit"), vec!["resume-hint", "transcript"]);
    cycle("Fullscreen scrollbar", 3);
    assert_eq!(recorded(&record, "scrollbar"), vec!["always", "hidden", "auto"]);
    cycle("Fullscreen copy on select", 2);
    assert_eq!(recorded(&record, "copy"), vec!["false", "true"]);
    // #9758: custom values from settings.json stay in the cycle.
    cycle("Fullscreen wheel scrolling", 3);
    assert_eq!(recorded(&record, "wheel"), vec!["10", "auto", "1"]);
}

#[test]
fn settings_selector_cycles_through_share_destinations() {
    let _g = setup();
    let record: Recorded = Arc::default();
    let list = SettingsSelectorComponent::new(&base_config(), &noop_callbacks(record.clone())).get_settings_list();
    list.select_item("share-destination");
    for _ in 0..3 {
        list.handle_input("\r");
    }
    assert_eq!(recorded(&record, "share"), vec!["gist", "radius", "auto"]);
}

#[test]
fn settings_selector_keeps_the_configured_fixed_theme_marked_while_browsing() {
    let _g = setup();
    let mut config = base_config();
    config.current_theme = "dark".into();
    config.available_themes = vec!["system".into(), "dark".into(), "light".into()];
    let list = SettingsSelectorComponent::new(&config, &noop_callbacks(Arc::default())).get_settings_list();
    list.select_item("theme");
    list.handle_input("\r");
    let output = render_plain(list.as_ref(), 120);
    let re = regex::Regex::new(r" {4}system +Theme created from your terminal's colors\n {4}automatic +Use separate themes")
        .unwrap();
    assert!(re.is_match(&output), "{output}");
    assert!(output.contains("→ ✓ dark"));

    list.handle_input(DOWN);
    let output = render_plain(list.as_ref(), 120);
    assert!(output.contains("  ✓ dark"));
    assert!(output.contains("→   light"));
}

#[test]
fn settings_selector_keeps_a_configured_automatic_theme_marked_while_browsing() {
    let _g = setup();
    let mut config = base_config();
    config.current_theme = "light/dark".into();
    config.available_themes = vec!["dark".into(), "light".into(), "other".into()];
    let list = SettingsSelectorComponent::new(&config, &noop_callbacks(Arc::default())).get_settings_list();
    list.select_item("theme");
    list.handle_input("\r");
    list.handle_input("\r");
    let output = render_plain(list.as_ref(), 120);
    assert!(output.contains("→ ✓ light"), "{output}");

    list.handle_input(DOWN);
    let output = render_plain(list.as_ref(), 120);
    assert!(output.contains("  ✓ light"));
    assert!(output.contains("→   other"));
}

#[tokio::test]
async fn settings_selector_keeps_the_configured_per_model_thinking_level_marked_while_browsing() {
    let _g = setup();
    let harness = create_harness(HarnessOptions {
        models: vec![FauxModelDefinition { id: "thinking-model".into(), reasoning: Some(true), ..Default::default() }],
        ..Default::default()
    })
    .await
    .unwrap();
    let model = harness.get_model_by_id("thinking-model").unwrap();
    let model_key = format!("{}/{}", model.provider, model.id);
    let mut config = base_config();
    config.default_model = model_key.clone();
    config.available_default_models = vec![model.clone()];
    config.thinking_level = ThinkingLevel::High.into();
    config.model_thinking_levels.insert(model_key, ThinkingLevel::Medium.into());
    let list = SettingsSelectorComponent::new(&config, &noop_callbacks(Arc::default())).get_settings_list();

    list.select_item("model-thinking");
    list.handle_input("\r");
    list.handle_input("\r");

    let output = render_plain(list.as_ref(), 120);
    assert!(output.contains("→ ✓ medium"), "{output}");
    assert!(output.contains("    (clear override)"));

    list.handle_input(DOWN);
    let output = render_plain(list.as_ref(), 120);
    assert!(output.contains("  ✓ medium"));
    assert!(output.contains("→   high"));
    harness.cleanup();
}

// =============================================================================
// session-selector-search.test.ts
// =============================================================================

fn date(s: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&chrono::Utc)
}

fn session(id: &str, modified: &str, text: &str, name: Option<&str>) -> SessionInfo {
    SessionInfo {
        path: format!("/tmp/{id}.jsonl"),
        id: id.into(),
        cwd: String::new(),
        name: name.map(str::to_string),
        parent_session_path: None,
        created: date("1970-01-01T00:00:00.000Z"),
        modified: date(modified),
        message_count: 1,
        first_message: "hello".into(),
        all_messages_text: text.into(),
    }
}

fn ids(sessions: &[SessionInfo]) -> Vec<String> {
    sessions.iter().map(|s| s.id.clone()).collect()
}

#[test]
fn session_search_filters_by_quoted_phrase_with_whitespace_normalization() {
    let sessions = vec![
        session("a", "2026-01-01T00:00:00.000Z", "node\n\n   cve was discussed", None),
        session("b", "2026-01-02T00:00:00.000Z", "node something else", None),
    ];
    assert_eq!(ids(&filter_and_sort_sessions(&sessions, "\"node cve\"", &SortMode::Recent, None)), vec!["a"]);
}

#[test]
fn session_search_filters_by_regex_case_insensitively() {
    let sessions = vec![
        session("a", "2026-01-02T00:00:00.000Z", "Brave is great", None),
        session("b", "2026-01-03T00:00:00.000Z", "bravery is not the same", None),
    ];
    assert_eq!(ids(&filter_and_sort_sessions(&sessions, "re:\\bbrave\\b", &SortMode::Recent, None)), vec!["a"]);
}

#[test]
fn session_search_recent_sort_preserves_input_order() {
    let sessions = vec![
        session("newer", "2026-01-03T00:00:00.000Z", "brave", None),
        session("older", "2026-01-01T00:00:00.000Z", "brave", None),
        session("nomatch", "2026-01-04T00:00:00.000Z", "something else", None),
    ];
    assert_eq!(
        ids(&filter_and_sort_sessions(&sessions, "\"brave\"", &SortMode::Recent, None)),
        vec!["newer", "older"]
    );
}

#[test]
fn session_search_relevance_sort_orders_by_score_and_ties_by_modified() {
    let sessions = vec![
        session("late", "2026-01-03T00:00:00.000Z", "xxxx brave", None),
        session("early", "2026-01-01T00:00:00.000Z", "brave xxxx", None),
    ];
    assert_eq!(
        ids(&filter_and_sort_sessions(&sessions, "\"brave\"", &SortMode::Relevance, None)),
        vec!["early", "late"]
    );
    let tie = vec![
        session("newer", "2026-01-03T00:00:00.000Z", "brave", None),
        session("older", "2026-01-01T00:00:00.000Z", "brave", None),
    ];
    assert_eq!(ids(&filter_and_sort_sessions(&tie, "\"brave\"", &SortMode::Relevance, None)), vec!["newer", "older"]);
}

#[test]
fn session_search_returns_empty_list_for_invalid_regex() {
    let sessions = vec![session("a", "2026-01-01T00:00:00.000Z", "brave", None)];
    assert!(filter_and_sort_sessions(&sessions, "re:(", &SortMode::Recent, None).is_empty());
}

fn named_sessions() -> Vec<SessionInfo> {
    vec![
        session("named1", "2026-01-03T00:00:00.000Z", "blueberry", Some("My Project")),
        session("named2", "2026-01-02T00:00:00.000Z", "blueberry", Some("Another Named")),
        session("other1", "2026-01-04T00:00:00.000Z", "blueberry", None),
        session("other2", "2026-01-01T00:00:00.000Z", "blueberry", None),
    ]
}

#[test]
fn session_search_name_filter() {
    let sessions = named_sessions();
    assert_eq!(
        ids(&filter_and_sort_sessions(&sessions, "", &SortMode::Recent, Some(&NameFilter::All))),
        vec!["named1", "named2", "other1", "other2"]
    );
    assert_eq!(
        ids(&filter_and_sort_sessions(&sessions, "", &SortMode::Recent, Some(&NameFilter::Named))),
        vec!["named1", "named2"]
    );
    assert_eq!(
        ids(&filter_and_sort_sessions(&sessions, "blueberry", &SortMode::Recent, Some(&NameFilter::Named))),
        vec!["named1", "named2"]
    );
    let whitespace = vec![
        session("whitespace", "2026-01-01T00:00:00.000Z", "test", Some("   ")),
        session("empty", "2026-01-02T00:00:00.000Z", "test", Some("")),
        session("named", "2026-01-03T00:00:00.000Z", "test", Some("Real Name")),
    ];
    assert_eq!(
        ids(&filter_and_sort_sessions(&whitespace, "", &SortMode::Recent, Some(&NameFilter::Named))),
        vec!["named"]
    );
}

// =============================================================================
// session-selector-rename.test.ts / session-selector-path-delete.test.ts
// =============================================================================

async fn flush() {
    tokio::time::sleep(Duration::from_millis(30)).await;
}

fn fixed_loader(sessions: Vec<SessionInfo>) -> SessionsLoader {
    Arc::new(move |_, _| {
        let sessions = sessions.clone();
        async move { Ok(sessions) }.boxed()
    })
}

fn selector_with(
    current: SessionsLoader,
    all: SessionsLoader,
    options: SessionSelectorComponentOptions,
    current_path: Option<&str>,
) -> SessionSelectorComponent {
    SessionSelectorComponent::new(
        &current,
        &all,
        Arc::new(|_| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        Some(&options),
        current_path,
    )
}

fn options(show_rename_hint: Option<bool>) -> SessionSelectorComponentOptions {
    SessionSelectorComponentOptions {
        rename_session: None,
        show_rename_hint,
        keybindings: Some(Arc::new(KeybindingsManager::new(None, None))),
    }
}

#[tokio::test]
async fn session_selector_shows_rename_hint_when_configured() {
    let _g = setup();
    let selector = selector_with(
        fixed_loader(vec![session("a", "1970-01-01T00:00:00.000Z", "hello", None)]),
        fixed_loader(vec![]),
        options(Some(true)),
        None,
    );
    flush().await;
    let output = selector.render(120).join("\n");
    assert!(output.contains("ctrl+r"));
    assert!(output.contains("rename"));
}

#[tokio::test]
async fn session_selector_hides_rename_hint_in_resume_picker() {
    let _g = setup();
    let selector = selector_with(
        fixed_loader(vec![session("a", "1970-01-01T00:00:00.000Z", "hello", None)]),
        fixed_loader(vec![]),
        options(Some(false)),
        None,
    );
    flush().await;
    let output = selector.render(120).join("\n");
    assert!(!output.contains("ctrl+r"));
    assert!(!output.contains("rename"));
}

#[tokio::test]
async fn session_selector_enters_rename_mode_and_submits() {
    let _g = setup();
    let sessions = vec![session("a", "1970-01-01T00:00:00.000Z", "hello", Some("Old"))];
    let renamed: Arc<Mutex<Vec<(String, Option<String>)>>> = Arc::default();
    let r = renamed.clone();
    let mut opts = options(Some(true));
    opts.rename_session = Some(Arc::new(move |path: String, name: Option<String>| {
        r.lock().push((path, name));
        async { Ok(()) }.boxed()
    }));
    let selector = selector_with(fixed_loader(sessions.clone()), fixed_loader(vec![]), opts, None);
    flush().await;

    selector.get_session_list().handle_input("\x1b[114;5u");
    flush().await;
    let output = selector.render(120).join("\n");
    assert!(output.contains("Rename Session"));
    assert!(!output.contains("Resume Session"));

    selector.handle_input("X");
    selector.handle_input("\r");
    flush().await;
    assert_eq!(*renamed.lock(), vec![(sessions[0].path.clone(), Some("XOld".to_string()))]);
}

const CTRL_D: &str = "\x04";
const CTRL_BACKSPACE: &str = "\x1b[127;5u";

fn two_sessions() -> Vec<SessionInfo> {
    vec![session("a", "1970-01-01T00:00:00.000Z", "hello", None), session("b", "1970-01-01T00:00:00.000Z", "hello", None)]
}

fn record_confirmations(selector: &SessionSelectorComponent) -> Arc<Mutex<Vec<Option<String>>>> {
    let changes: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
    let c = changes.clone();
    selector.get_session_list().set_on_delete_confirmation_change(Some(Arc::new(move |p| c.lock().push(p))));
    changes
}

#[tokio::test]
async fn session_selector_ctrl_backspace_with_query_does_not_delete() {
    let _g = setup();
    let selector = selector_with(fixed_loader(two_sessions()), fixed_loader(vec![]), options(None), None);
    flush().await;
    let changes = record_confirmations(&selector);
    let list = selector.get_session_list();
    list.handle_input("a");
    list.handle_input(CTRL_BACKSPACE);
    assert!(changes.lock().is_empty());
}

#[tokio::test]
async fn session_selector_ctrl_d_confirms_even_with_query() {
    let _g = setup();
    let sessions = two_sessions();
    let selector = selector_with(fixed_loader(sessions.clone()), fixed_loader(vec![]), options(None), None);
    flush().await;
    let changes = record_confirmations(&selector);
    let list = selector.get_session_list();
    list.handle_input("a");
    list.handle_input(CTRL_D);
    assert_eq!(*changes.lock(), vec![Some(sessions[0].path.clone())]);
}

#[tokio::test]
async fn session_selector_ctrl_backspace_with_empty_query_confirms_and_deletes() {
    let _g = setup();
    let sessions = two_sessions();
    let selector = selector_with(fixed_loader(sessions.clone()), fixed_loader(vec![]), options(None), None);
    flush().await;
    let changes = record_confirmations(&selector);
    let list = selector.get_session_list();
    let deleted: Arc<Mutex<Option<String>>> = Arc::default();
    let d = deleted.clone();
    list.set_on_delete_session(Some(Arc::new(move |path: String| {
        *d.lock() = Some(path);
        async { Ok(()) }.boxed()
    })));

    list.handle_input(CTRL_BACKSPACE);
    assert_eq!(*changes.lock(), vec![Some(sessions[0].path.clone())]);
    list.handle_input("\r");
    assert_eq!(*changes.lock(), vec![Some(sessions[0].path.clone()), None]);
    assert_eq!(*deleted.lock(), Some(sessions[0].path.clone()));
}

/// A loader whose result is resolved later through the returned sender.
fn deferred_loader(
    calls: Arc<AtomicUsize>,
    partial: Option<Vec<SessionInfo>>,
) -> (SessionsLoader, Arc<Mutex<Option<tokio::sync::oneshot::Sender<Vec<SessionInfo>>>>>) {
    let (tx, rx) = tokio::sync::oneshot::channel::<Vec<SessionInfo>>();
    let rx = Arc::new(Mutex::new(Some(rx)));
    let loader: SessionsLoader = Arc::new(move |on_progress, _| {
        calls.fetch_add(1, Ordering::SeqCst);
        if let (Some(progress), Some(partial)) = (on_progress, partial.clone()) {
            progress(1, 2, Some(partial));
        }
        let rx = rx.lock().take();
        async move {
            match rx {
                Some(rx) => rx.await.map_err(|e| anyhow::anyhow!(e)),
                None => Ok(Vec::new()),
            }
        }
        .boxed()
    });
    (loader, Arc::new(Mutex::new(Some(tx))))
}

#[tokio::test]
async fn session_selector_does_not_switch_back_to_all_when_all_load_resolves_late() {
    let _g = setup();
    let calls = Arc::new(AtomicUsize::new(0));
    let (all_loader, resolver) = deferred_loader(calls.clone(), None);
    let selector = selector_with(
        fixed_loader(vec![session("current", "1970-01-01T00:00:00.000Z", "hello", None)]),
        all_loader,
        options(None),
        None,
    );
    flush().await;
    let list = selector.get_session_list();
    list.handle_input("\t"); // current -> all (starts async load)
    list.handle_input("\t"); // all -> current
    let _ = resolver.lock().take().unwrap().send(vec![session("all", "1970-01-01T00:00:00.000Z", "hello", None)]);
    flush().await;

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let output = selector.render(120).join("\n");
    assert!(output.contains("Resume Session (Current Folder)"));
    assert!(!output.contains("Resume Session (All)"));
}

#[tokio::test]
async fn session_selector_does_not_start_redundant_all_loads() {
    let _g = setup();
    let calls = Arc::new(AtomicUsize::new(0));
    let all_sessions = vec![session("all", "1970-01-01T00:00:00.000Z", "hello", None)];
    let (all_loader, resolver) = deferred_loader(calls.clone(), Some(all_sessions.clone()));
    let selector = selector_with(
        fixed_loader(vec![session("current", "1970-01-01T00:00:00.000Z", "hello", None)]),
        all_loader,
        options(None),
        None,
    );
    flush().await;
    let list = selector.get_session_list();
    list.handle_input("\t"); // current -> all (starts async load)
    list.handle_input("\t"); // all -> current
    list.handle_input("\t"); // current -> all again while load pending

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(selector.get_session_list().get_selected_session_path(), Some(all_sessions[0].path.clone()));
    assert!(selector.render(120).join("\n").contains("Loading"));

    let _ = resolver.lock().take().unwrap().send(all_sessions);
    flush().await;
}

struct SymlinkedPaths {
    base_dir: tempfile::TempDir,
    parent_alias_a: String,
    parent_alias_b: String,
    child_alias_b: String,
}

#[cfg(unix)]
fn create_symlinked_session_paths() -> SymlinkedPaths {
    let base_dir = tempfile::tempdir().unwrap();
    let base = base_dir.path();
    let real_dir = base.join("real");
    let alias_a = base.join("alias-a");
    let alias_b = base.join("alias-b");
    std::fs::create_dir_all(&real_dir).unwrap();
    std::fs::create_dir_all(&alias_a).unwrap();
    std::fs::create_dir_all(&alias_b).unwrap();
    let shared = real_dir.join("sessions");
    std::fs::create_dir_all(&shared).unwrap();
    std::os::unix::fs::symlink(&shared, alias_a.join("sessions")).unwrap();
    std::os::unix::fs::symlink(&shared, alias_b.join("sessions")).unwrap();
    std::fs::write(shared.join("parent.jsonl"), "parent\n").unwrap();
    std::fs::write(shared.join("child.jsonl"), "child\n").unwrap();
    let p = |dir: &std::path::Path, name: &str| dir.join("sessions").join(name).to_string_lossy().to_string();
    SymlinkedPaths {
        parent_alias_a: p(&alias_a, "parent.jsonl"),
        parent_alias_b: p(&alias_b, "parent.jsonl"),
        child_alias_b: p(&alias_b, "child.jsonl"),
        base_dir,
    }
}

#[cfg(unix)]
#[tokio::test]
async fn session_selector_threads_sessions_across_symlink_aliases() {
    let _g = setup();
    let paths = create_symlinked_session_paths();
    let mut parent = session("parent", "2026-01-01T00:00:00.000Z", "hello", Some("Parent"));
    parent.path = paths.parent_alias_b.clone();
    let mut child = session("child", "2025-12-31T00:00:00.000Z", "hello", Some("Child"));
    child.path = paths.child_alias_b.clone();
    child.parent_session_path = Some(paths.parent_alias_a.clone());
    let selector = selector_with(fixed_loader(vec![parent, child]), fixed_loader(vec![]), options(None), None);
    flush().await;
    let output = render_plain(&selector, 120);
    assert!(output.contains("Parent"));
    assert!(output.contains("└─ Child"));
    drop(paths.base_dir);
}

#[tokio::test]
async fn session_selector_sorts_threads_by_latest_subtree_activity() {
    let _g = setup();
    let parent_one = session("parent-one", "2026-01-02T00:00:00.000Z", "hello", Some("Parent one"));
    let parent_two = session("parent-two", "2026-01-01T00:00:00.000Z", "hello", Some("Parent two"));
    let mut child_two = session("child-two", "2026-01-03T00:00:00.000Z", "hello", Some("Child two"));
    child_two.parent_session_path = Some(parent_two.path.clone());
    let selector =
        selector_with(fixed_loader(vec![parent_one, parent_two, child_two]), fixed_loader(vec![]), options(None), None);
    flush().await;
    let output = render_plain(&selector, 120);
    let parent_two_index = output.find("Parent two").unwrap();
    let child_two_index = output.find("└─ Child two").unwrap();
    let parent_one_index = output.find("Parent one").unwrap();
    assert!(child_two_index > parent_two_index);
    assert!(parent_one_index > child_two_index);
}

#[cfg(unix)]
#[tokio::test]
async fn session_selector_treats_the_current_session_as_active_across_symlinks() {
    let _g = setup();
    let paths = create_symlinked_session_paths();
    let mut parent = session("parent", "1970-01-01T00:00:00.000Z", "hello", Some("Parent"));
    parent.path = paths.parent_alias_b.clone();
    let selector =
        selector_with(fixed_loader(vec![parent]), fixed_loader(vec![]), options(None), Some(&paths.parent_alias_a));
    flush().await;
    let changes = record_confirmations(&selector);
    let error: Arc<Mutex<Option<String>>> = Arc::default();
    let e = error.clone();
    let list = selector.get_session_list();
    list.set_on_error(Some(Arc::new(move |m| *e.lock() = Some(m))));
    list.handle_input(CTRL_D);
    assert!(changes.lock().is_empty());
    assert_eq!(error.lock().as_deref(), Some("Cannot delete the currently active session"));
}

// =============================================================================
// tree-selector.test.ts
// =============================================================================

fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

fn usage() -> serde_json::Value {
    json!({
        "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
    })
}

fn entry(value: serde_json::Value) -> SessionEntry {
    serde_json::from_value(value).expect("valid session entry")
}

fn user_message(id: &str, parent: Option<&str>, content: &str) -> SessionEntry {
    entry(json!({
        "type": "message", "id": id, "parentId": parent, "timestamp": now_iso(),
        "message": { "role": "user", "content": content, "timestamp": 0 }
    }))
}

fn assistant_message(id: &str, parent: Option<&str>, text: &str) -> SessionEntry {
    entry(json!({
        "type": "message", "id": id, "parentId": parent, "timestamp": now_iso(),
        "message": {
            "role": "assistant", "content": [{ "type": "text", "text": text }],
            "api": "anthropic-messages", "provider": "anthropic", "model": "claude-sonnet-4",
            "usage": usage(), "stopReason": "stop", "timestamp": 0
        }
    }))
}

fn tool_call_only_assistant(id: &str, parent: Option<&str>) -> SessionEntry {
    entry(json!({
        "type": "message", "id": id, "parentId": parent, "timestamp": now_iso(),
        "message": {
            "role": "assistant",
            "content": [{ "type": "toolCall", "id": format!("tc-{id}"), "name": "read", "arguments": { "path": "test.ts" } }],
            "api": "anthropic-messages", "provider": "anthropic", "model": "claude-sonnet-4",
            "usage": usage(), "stopReason": "toolUse", "timestamp": 0
        }
    }))
}

fn model_change(id: &str, parent: Option<&str>) -> SessionEntry {
    entry(json!({
        "type": "model_change", "id": id, "parentId": parent, "timestamp": now_iso(),
        "provider": "anthropic", "modelId": "claude-sonnet-4"
    }))
}

/// Build a tree from entries using parentId relationships.
fn build_tree(entries: Vec<SessionEntry>) -> Vec<SessionTreeNode> {
    fn attach(id: &str, entries: &[SessionEntry]) -> SessionTreeNode {
        let entry = entries.iter().find(|e| e.id() == id).unwrap().clone();
        let children =
            entries.iter().filter(|e| e.parent_id() == Some(id)).map(|e| attach(e.id(), entries)).collect();
        SessionTreeNode { entry, children, label: None, label_timestamp: None }
    }
    entries.iter().filter(|e| e.parent_id().is_none()).map(|e| attach(e.id(), &entries)).collect()
}

fn tree_selector(tree: &[SessionTreeNode], leaf: &str, filter: Option<FilterMode>) -> TreeSelectorComponent {
    TreeSelectorComponent::new(tree, Some(leaf), 24, Arc::new(|_| {}), Arc::new(|| {}), None, None, filter.as_ref())
}

fn selected(selector: &TreeSelectorComponent) -> Option<String> {
    selector.get_tree_list().get_selected_node().map(|n| n.entry.id().to_string())
}

fn branch_entries() -> Vec<SessionEntry> {
    vec![
        user_message("user-1", None, "hello"),
        assistant_message("asst-1", Some("user-1"), "hi"),
        user_message("user-2", Some("asst-1"), "active branch"),
        assistant_message("asst-2", Some("user-2"), "response"),
        user_message("user-3", Some("asst-1"), "sibling branch"),
    ]
}

#[test]
fn tree_focuses_nearest_visible_ancestor_of_a_model_change_leaf() {
    let _g = setup();
    let tree = build_tree(vec![
        user_message("user-1", None, "hello"),
        assistant_message("asst-1", Some("user-1"), "hi"),
        user_message("user-2", Some("asst-1"), "active branch"),
        model_change("model-1", Some("user-2")),
        user_message("user-3", Some("asst-1"), "sibling branch"),
    ]);
    let selector = tree_selector(&tree, "model-1", None);
    assert_eq!(selected(&selector).as_deref(), Some("user-2"));
}

#[test]
fn tree_hides_context_edits_by_default_and_labels_them_in_all_mode() {
    let _g = setup();
    let tree = build_tree(vec![
        user_message("user-1", None, "hello"),
        assistant_message("asst-1", Some("user-1"), "hi"),
        entry(json!({
            "type": "context_edit", "id": "edit-1", "parentId": "asst-1", "timestamp": now_iso(),
            "targetId": "asst-1", "replacement": null
        })),
    ]);
    let default_selector = tree_selector(&tree, "edit-1", None);
    assert_eq!(selected(&default_selector).as_deref(), Some("asst-1"));

    let all_selector = tree_selector(&tree, "edit-1", Some(FilterMode::All));
    let rendered = strip(&all_selector.get_tree_list().render(200).join("\n"));
    assert!(rendered.contains("[context omit: asst-1]"), "{rendered}");
}

#[test]
fn tree_focuses_nearest_visible_ancestor_of_a_thinking_level_leaf() {
    let _g = setup();
    let tree = build_tree(vec![
        user_message("user-1", None, "hello"),
        assistant_message("asst-1", Some("user-1"), "hi"),
        user_message("user-2", Some("asst-1"), "active branch"),
        entry(json!({
            "type": "thinking_level_change", "id": "thinking-1", "parentId": "user-2",
            "timestamp": now_iso(), "thinkingLevel": "high"
        })),
        user_message("user-3", Some("asst-1"), "sibling branch"),
    ]);
    let selector = tree_selector(&tree, "thinking-1", None);
    assert_eq!(selected(&selector).as_deref(), Some("user-2"));
}

#[test]
fn tree_switches_to_nearest_user_message_with_user_only_filter_and_back() {
    let _g = setup();
    let tree = build_tree(branch_entries());
    let selector = tree_selector(&tree, "asst-2", None);
    assert_eq!(selected(&selector).as_deref(), Some("asst-2"));
    selector.handle_input("\x15"); // Ctrl+U
    assert_eq!(selected(&selector).as_deref(), Some("user-2"));
    selector.handle_input("\x04"); // Ctrl+D
    assert_eq!(selected(&selector).as_deref(), Some("user-2"));
}

#[test]
fn tree_help_wraps_semantic_rows_without_truncating() {
    let _g = setup();
    let tree = build_tree(vec![user_message("user-1", None, "hello"), assistant_message("asst-1", Some("user-1"), "hi")]);
    let selector = tree_selector(&tree, "asst-1", None);
    let lines: Vec<String> = selector.render(30).iter().map(|l| strip(l)).collect();
    let plain = lines.join("\n");
    for needle in ["branch", "copy", "filters", "cycle", "label time"] {
        assert!(plain.contains(needle), "missing {needle}: {plain}");
    }
    assert!(!plain.contains("..."));
    assert!(lines.iter().all(|l| visible_width(l) <= 30));
}

#[test]
fn tree_copies_the_full_selected_message_with_ctrl_x() {
    let _g = setup();
    let message = format!("{}\nsecond line", "long message ".repeat(30));
    let tree =
        build_tree(vec![user_message("user-1", None, "hello"), assistant_message("asst-1", Some("user-1"), &message)]);
    let selector = tree_selector(&tree, "asst-1", None);
    let copied: Arc<Mutex<Option<String>>> = Arc::default();
    let c = copied.clone();
    selector.set_on_copy(Some(Arc::new(move |text| *c.lock() = text)));
    selector.handle_input("\x18");
    assert_eq!(copied.lock().as_deref(), Some(message.as_str()));
}

#[test]
fn tree_toggles_label_timestamps_for_labeled_nodes() {
    let _g = setup();
    use chrono::TimeZone;
    let mut tree =
        build_tree(vec![user_message("user-1", None, "hello"), assistant_message("asst-1", Some("user-1"), "hi")]);
    let label_date = chrono::Local.with_ymd_and_hms(2026, 3, 28, 14, 32, 0).unwrap();
    tree[0].label = Some("checkpoint".into());
    tree[0].label_timestamp =
        Some(label_date.with_timezone(&chrono::Utc).format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string());
    let selector = tree_selector(&tree, "asst-1", None);
    let list = selector.get_tree_list();
    let render = list.render(200).join("\n");
    assert!(render.contains("[checkpoint]"));
    assert!(!render.contains("3/28 14:32"));
    assert!(!render.contains("[+label time]"));
    selector.handle_input("T");
    let render = list.render(200).join("\n");
    assert!(render.contains("3/28 14:32"));
    assert!(render.contains("[+label time]"));
}

#[test]
fn tree_preserves_selection_through_empty_filters() {
    let _g = setup();
    let tree = build_tree(vec![
        user_message("user-1", None, "hello"),
        assistant_message("asst-1", Some("user-1"), "hi"),
        user_message("user-2", Some("asst-1"), "bye"),
        assistant_message("asst-2", Some("user-2"), "goodbye"),
    ]);
    let selector = tree_selector(&tree, "asst-2", None);
    assert_eq!(selected(&selector).as_deref(), Some("asst-2"));
    selector.handle_input("\x0c"); // Ctrl+L
    assert_eq!(selected(&selector), None);
    selector.handle_input("\x04"); // Ctrl+D
    assert_eq!(selected(&selector).as_deref(), Some("asst-2"));

    let tree = build_tree(vec![user_message("user-1", None, "hello"), assistant_message("asst-1", Some("user-1"), "hi")]);
    let selector = tree_selector(&tree, "asst-1", None);
    assert_eq!(selected(&selector).as_deref(), Some("asst-1"));
    selector.handle_input("\x0c");
    assert_eq!(selected(&selector), None);
    selector.handle_input("\x0c");
    assert_eq!(selected(&selector).as_deref(), Some("asst-1"));
    selector.handle_input("\x0c");
    assert_eq!(selected(&selector), None);
    selector.handle_input("\x04");
    assert_eq!(selected(&selector).as_deref(), Some("asst-1"));
}

const CTRL_LEFT: &str = "\x1b[1;5D";
const CTRL_RIGHT: &str = "\x1b[1;5C";
const ALT_LEFT: &str = "\x1b[1;3D";
const ALT_RIGHT: &str = "\x1b[1;3C";

fn branching_tree() -> Vec<SessionTreeNode> {
    build_tree(vec![
        user_message("user-1", None, "first message"),
        assistant_message("asst-1", Some("user-1"), "response 1"),
        user_message("user-2", Some("asst-1"), "second message"),
        assistant_message("asst-2", Some("user-2"), "response 2"),
        user_message("user-3a", Some("asst-2"), "branch A start"),
        assistant_message("asst-3a", Some("user-3a"), "branch A response"),
        user_message("user-4a", Some("asst-3a"), "branch A deep"),
        assistant_message("asst-4a", Some("user-4a"), "branch A leaf"),
        user_message("user-3b", Some("asst-2"), "branch B start"),
        assistant_message("asst-3b", Some("user-3b"), "branch B response"),
        user_message("user-4b", Some("asst-3b"), "branch B deep"),
    ])
}

fn steps(selector: &TreeSelectorComponent, steps: &[(&str, &str)]) {
    for (input, expected) in steps {
        selector.handle_input(input);
        assert_eq!(selected(selector).as_deref(), Some(*expected), "after {input:?}");
    }
}

#[test]
fn tree_ctrl_right_unfolds_then_jumps_segments() {
    let _g = setup();
    let tree = branching_tree();
    let selector = tree_selector(&tree, "asst-4a", None);
    steps(
        &selector,
        &[
            (CTRL_LEFT, "user-3a"),
            (CTRL_LEFT, "user-3a"),
            (DOWN, "user-3b"),
            (UP, "user-3a"),
            (CTRL_RIGHT, "user-3a"),
            (DOWN, "asst-3a"),
            (CTRL_LEFT, "user-3a"),
            (CTRL_RIGHT, "asst-4a"),
        ],
    );
}

#[test]
fn tree_alt_arrows_alias_fold_and_unfold() {
    let _g = setup();
    let tree = branching_tree();
    let selector = tree_selector(&tree, "asst-4a", None);
    steps(&selector, &[(ALT_LEFT, "user-3a"), (ALT_LEFT, "user-3a"), (ALT_RIGHT, "user-3a"), (ALT_RIGHT, "asst-4a")]);
}

#[test]
fn tree_folding_root_hides_subtree_and_keeps_nested_folds() {
    let _g = setup();
    let tree = branching_tree();
    let selector = tree_selector(&tree, "asst-4a", None);
    steps(
        &selector,
        &[
            (CTRL_LEFT, "user-3a"),
            (CTRL_LEFT, "user-3a"),
            (CTRL_LEFT, "user-1"),
            (CTRL_LEFT, "user-1"),
            (DOWN, "user-1"),
            (CTRL_RIGHT, "user-1"),
            (CTRL_RIGHT, "user-3a"),
            (DOWN, "user-3b"),
        ],
    );
}

#[test]
fn tree_fold_and_navigate_on_non_active_branch() {
    let _g = setup();
    let tree = branching_tree();
    let selector = tree_selector(&tree, "asst-4a", None);
    let mut found = false;
    for _ in 0..20 {
        selector.handle_input(DOWN);
        if selected(&selector).as_deref() == Some("user-3b") {
            found = true;
            break;
        }
    }
    assert!(found);
    steps(
        &selector,
        &[(CTRL_RIGHT, "user-4b"), (CTRL_LEFT, "user-3b"), (CTRL_LEFT, "user-3b"), (CTRL_LEFT, "user-1")],
    );
}

#[test]
fn tree_fold_and_navigate_with_multiple_roots() {
    let _g = setup();
    let tree = build_tree(vec![
        user_message("user-1", None, "first root"),
        assistant_message("asst-1", Some("user-1"), "response 1"),
        user_message("user-2", None, "second root"),
        assistant_message("asst-2", Some("user-2"), "response 2"),
    ]);
    let selector = tree_selector(&tree, "asst-1", None);
    assert_eq!(selected(&selector).as_deref(), Some("asst-1"));
    steps(
        &selector,
        &[
            (CTRL_LEFT, "user-1"),
            (CTRL_LEFT, "user-1"),
            (DOWN, "user-2"),
            (CTRL_RIGHT, "asst-2"),
            (CTRL_LEFT, "user-2"),
            (CTRL_LEFT, "user-2"),
            (CTRL_LEFT, "user-2"),
        ],
    );
}

#[test]
fn tree_folding_root_hides_descendants_behind_filtered_nodes() {
    let _g = setup();
    let tree = build_tree(vec![
        user_message("user-1", None, "hello"),
        tool_call_only_assistant("tool-asst-1", Some("user-1")),
        user_message("user-2", Some("tool-asst-1"), "follow up"),
        assistant_message("asst-2", Some("user-2"), "response"),
    ]);
    let selector = tree_selector(&tree, "asst-2", None);
    steps(&selector, &[(CTRL_LEFT, "user-1"), (CTRL_LEFT, "user-1"), (DOWN, "user-1")]);
}

#[test]
fn tree_search_and_filter_changes_reset_fold_state() {
    let _g = setup();
    for reset in [&["b", "\x1b"][..], &["\x15", "\x04"][..]] {
        let tree = branching_tree();
        let selector = tree_selector(&tree, "asst-4a", None);
        selector.handle_input(CTRL_LEFT);
        selector.handle_input(CTRL_LEFT);
        if reset[0] == "b" {
            selector.handle_input(DOWN);
            assert_eq!(selected(&selector).as_deref(), Some("user-3b"));
        }
        for input in reset {
            selector.handle_input(input);
        }
        let mut current = String::new();
        for _ in 0..20 {
            selector.handle_input(DOWN);
            current = selected(&selector).unwrap_or_default();
            if current == "user-3a" {
                break;
            }
        }
        assert_eq!(current, "user-3a");
        selector.handle_input(DOWN);
        assert_eq!(selected(&selector).as_deref(), Some("asst-3a"));
    }
}
