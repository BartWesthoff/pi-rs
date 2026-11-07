//! Port of `packages/coding-agent/src/modes/interactive/components/model-selector.ts`.

use crate::core::model_runtime::ModelRuntime;
use crate::core::tools::js_util::locale_compare;
use crate::modes::interactive::components::dynamic_border::DynamicBorder;
use crate::modes::interactive::components::keybinding_hints::{key_display_text, key_hint};
use crate::modes::interactive::model_search::{ModelSearchItem, get_model_selector_search_text};
use crate::modes::interactive::theme::theme::{ThemeColor, theme};
use parking_lot::Mutex;
use pi_ai::models::chat_models_are_equal;
use pi_ai::types::Model;
use pi_tui::components::input::Input;
use pi_tui::components::spacer::Spacer;
use pi_tui::components::text::Text;
use pi_tui::fuzzy::fuzzy_filter;
use pi_tui::keybindings::get_keybindings;
use pi_tui::tui::{Component, ComponentRef, Container, MouseEvent, MouseResult, Tui};
use std::collections::HashSet;
use std::sync::Weak;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelItem {
    pub provider: String,
    pub id: String,
    /// Shared so the all/active/filtered lists rebuilt on every refresh and keystroke copy a pointer,
    /// not the whole model.
    pub model: Arc<Model>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopedModelItem {
    pub model: Model,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DefaultModelReference {
    pub provider: String,
    pub id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModelScope {
    #[serde(rename = "all")]
    All,
    #[serde(rename = "scoped")]
    Scoped,
}

impl ModelScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Scoped => "scoped",
        }
    }
}

struct State {
    all_models: Vec<ModelItem>,
    scoped_model_items: Vec<ModelItem>,
    active_models: Vec<ModelItem>,
    filtered_models: Vec<ModelItem>,
    selected_index: usize,
    error_message: Option<String>,
    refresh_status_message: String,
    refresh_status_success: bool,
    scoped_models: Vec<ScopedModelItem>,
    scope: ModelScope,
    focused: bool,
}

struct Inner {
    container: Container,
    search_input: Arc<Input>,
    list_container: Arc<Container>,
    current_model: Option<Model>,
    model_runtime: Arc<ModelRuntime>,
    on_select_callback: Arc<dyn Fn(Model) + Send + Sync>,
    on_select_as_default_callback: Option<Arc<dyn Fn(Model) + Send + Sync>>,
    on_cancel_callback: Arc<dyn Fn() + Send + Sync>,
    tui: Arc<Tui>,
    default_model: Option<DefaultModelReference>,
    scope_text: Option<Arc<Text>>,
    scope_hint_text: Option<Arc<Text>>,
    refresh_abort_controller: CancellationToken,
    closed: AtomicBool,
    state: Mutex<State>,
}

fn is_current(current: &Option<Model>, model: &Model) -> bool {
    chat_models_are_equal(current.as_ref(), Some(model))
}

impl Inner {
    fn is_default_model(&self, model: &Model) -> bool {
        self.default_model.as_ref().is_some_and(|d| d.provider == model.provider && d.id == model.id)
    }

    fn is_default_search(query: &str) -> bool {
        let normalized = query.trim().to_lowercase();
        !normalized.is_empty() && "default".starts_with(&normalized)
    }

    fn sort_models(&self, models: Vec<ModelItem>) -> Vec<ModelItem> {
        let mut sorted = models;
        // Sort: current model first, default model second, then by provider.
        sorted.sort_by(|a, b| {
            let a_cur = is_current(&self.current_model, &a.model);
            let b_cur = is_current(&self.current_model, &b.model);
            if a_cur != b_cur {
                return if a_cur { std::cmp::Ordering::Less } else { std::cmp::Ordering::Greater };
            }
            let a_def = self.is_default_model(&a.model);
            let b_def = self.is_default_model(&b.model);
            if a_def != b_def {
                return if a_def { std::cmp::Ordering::Less } else { std::cmp::Ordering::Greater };
            }
            locale_compare(&a.provider, &b.provider)
        });
        sorted
    }

    fn load_models_from_snapshot(&self) {
        let models: Vec<ModelItem> = self
            .model_runtime
            .get_available_snapshot()
            .into_iter()
            .map(|model| ModelItem { provider: model.provider.clone(), id: model.id.clone(), model: Arc::new(model) })
            .collect();
        let all_models = self.sort_models(models);
        let mut s = self.state.lock();
        s.all_models = all_models;
        let refreshed: Vec<ScopedModelItem> = s
            .scoped_models
            .iter()
            .map(|scoped| match self.model_runtime.get_model(&scoped.model.provider, &scoped.model.id) {
                Some(model) => ScopedModelItem { model, thinking_level: scoped.thinking_level.clone() },
                None => scoped.clone(),
            })
            .collect();
        s.scoped_models = refreshed;
        s.scoped_model_items = s
            .scoped_models
            .iter()
            .map(|scoped| ModelItem {
                provider: scoped.model.provider.clone(),
                id: scoped.model.id.clone(),
                model: Arc::new(scoped.model.clone()),
            })
            .collect();
        s.active_models = if s.scope == ModelScope::Scoped { s.scoped_model_items.clone() } else { s.all_models.clone() };
        s.filtered_models = s.active_models.clone();
        let current_index = s.filtered_models.iter().position(|item| is_current(&self.current_model, &item.model));
        s.selected_index = match current_index {
            Some(i) => i,
            None => s.selected_index.min(s.filtered_models.len().saturating_sub(1)),
        };
    }

    fn get_scope_text(&self, scope: ModelScope) -> String {
        let t = theme();
        let all_text = if scope == ModelScope::All { t.fg(&ThemeColor::Accent, "all") } else { t.fg(&ThemeColor::Muted, "all") };
        let scoped_text = if scope == ModelScope::Scoped {
            t.fg(&ThemeColor::Accent, "scoped")
        } else {
            t.fg(&ThemeColor::Muted, "scoped")
        };
        format!(
            "{}{all_text}{}{scoped_text}",
            t.fg(&ThemeColor::Muted, "Scope: "),
            t.fg(&ThemeColor::Muted, " | ")
        )
    }

    fn get_scope_hint_text() -> String {
        key_hint("tui.input.tab".into(), "scope") + &theme().fg(&ThemeColor::Muted, " (all/scoped)")
    }

    fn set_scope(&self, scope: ModelScope) {
        {
            let mut s = self.state.lock();
            if s.scope == scope {
                return;
            }
            s.scope = scope;
            s.active_models = if scope == ModelScope::Scoped { s.scoped_model_items.clone() } else { s.all_models.clone() };
            s.selected_index =
                s.active_models.iter().position(|item| is_current(&self.current_model, &item.model)).unwrap_or(0);
        }
        self.filter_models(&self.search_input.get_value());
        if let Some(scope_text) = &self.scope_text {
            scope_text.set_text(self.get_scope_text(scope));
        }
    }

    fn filter_models(&self, query: &str) {
        {
            let mut s = self.state.lock();
            if !query.is_empty() {
                let filtered = fuzzy_filter(&s.active_models, query, |item| {
                    let default_text = if self.is_default_model(&item.model) { " default" } else { "" };
                    format!(
                        "{}{default_text}",
                        get_model_selector_search_text(&ModelSearchItem {
                            id: item.id.clone(),
                            provider: item.provider.clone(),
                            name: Some(item.model.name.clone()),
                        })
                    )
                });
                if Self::is_default_search(query) {
                    let default_items: Vec<ModelItem> =
                        s.active_models.iter().filter(|item| self.is_default_model(&item.model)).cloned().collect();
                    let default_keys: HashSet<String> =
                        default_items.iter().map(|item| format!("{}\0{}", item.provider, item.id)).collect();
                    let mut combined = default_items;
                    combined.extend(
                        filtered.into_iter().filter(|item| !default_keys.contains(&format!("{}\0{}", item.provider, item.id))),
                    );
                    s.filtered_models = combined;
                } else {
                    s.filtered_models = filtered;
                }
            } else {
                s.filtered_models = s.active_models.clone();
            }
            // When filtering by a query, move the selector to the top row so the best
            // match is highlighted. When the query is cleared, keep the current position
            // clamped to the (restored) list length.
            s.selected_index =
                if !query.is_empty() { 0 } else { s.selected_index.min(s.filtered_models.len().saturating_sub(1)) };
        }
        self.update_list();
    }

    fn update_list(&self) {
        self.list_container.clear();
        let t = theme();
        let s = self.state.lock();
        let len = s.filtered_models.len() as i64;
        let max_visible: i64 = 10;
        let selected = s.selected_index as i64;
        let start_index = (selected - max_visible / 2).min(len - max_visible).max(0);
        let end_index = (start_index + max_visible).min(len);

        // Show visible slice of filtered models
        for i in start_index..end_index {
            let item = &s.filtered_models[i as usize];
            let is_selected = i == selected;
            let is_cur = is_current(&self.current_model, &item.model);
            let default_badge =
                if self.is_default_model(&item.model) { t.fg(&ThemeColor::Muted, " · default") } else { String::new() };
            let cursor = if is_selected { t.fg(&ThemeColor::Accent, "→ ") } else { "  ".to_string() };
            let current_marker = if is_cur { t.fg(&ThemeColor::Accent, "✓ ") } else { "  ".to_string() };
            let model_text = if is_selected { t.fg(&ThemeColor::Accent, &item.id) } else { item.id.clone() };
            let provider_badge = t.fg(&ThemeColor::Muted, &format!("[{}]", item.provider));
            let line = format!("{cursor}{current_marker}{model_text} {provider_badge}{default_badge}");
            self.list_container.add_child(Arc::new(Text::new(line, 0, 0, None)));
        }

        // Add scroll indicator if needed
        if start_index > 0 || end_index < len {
            let scroll_info = t.fg(&ThemeColor::Muted, &format!("  ({}/{})", selected + 1, len));
            self.list_container.add_child(Arc::new(Text::new(scroll_info, 0, 0, None)));
        }

        // Show error message or "no results" if empty
        if let Some(error) = s.error_message.as_deref().filter(|e| !e.is_empty()) {
            for line in error.split('\n') {
                self.list_container.add_child(Arc::new(Text::new(t.fg(&ThemeColor::Error, line), 0, 0, None)));
            }
        } else if len == 0 {
            self.list_container
                .add_child(Arc::new(Text::new(t.fg(&ThemeColor::Muted, "  No matching models"), 0, 0, None)));
        } else if let Some(selected) = s.filtered_models.get(s.selected_index) {
            self.list_container.add_child(Arc::new(Spacer::new(1)));
            self.list_container.add_child(Arc::new(Text::new(
                t.fg(&ThemeColor::Muted, &format!("  Model Name: {}", selected.model.name)),
                0,
                0,
                None,
            )));
        }
        if !s.refresh_status_message.is_empty() {
            self.list_container.add_child(Arc::new(Spacer::new(1)));
            let color = if s.refresh_status_success { ThemeColor::Success } else { ThemeColor::Muted };
            self.list_container.add_child(Arc::new(Text::new(
                t.fg(&color, &format!("  {}", s.refresh_status_message)),
                0,
                0,
                None,
            )));
        }
    }

    async fn refresh_models(self: Arc<Self>) {
        let timeout = Duration::from_millis(15_000);
        let timed_out = Arc::new(AtomicBool::new(false));
        let timer = {
            let timed_out = timed_out.clone();
            let controller = self.refresh_abort_controller.clone();
            crate::utils::uncaught::spawn(async move {
                tokio::time::sleep(timeout).await;
                timed_out.store(true, Ordering::SeqCst);
                controller.cancel();
            })
        };
        // pi-rs divergence from TS Pi: opening `/model` reloads the local catalog only, so nothing is
        // fetched from pi.dev. `/refresh-models` (and /scoped-models, /login, `pi update --models`)
        // refreshes the catalogs over the network on demand.
        let result = self
            .model_runtime
            .refresh(Some(pi_ai::models::ModelsRefreshOptions {
                allow_network: Some(false),
                signal: Some(self.refresh_abort_controller.clone()),
                ..Default::default()
            }))
            .await;
        timer.abort();
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        let timed_out = timed_out.load(Ordering::SeqCst);
        match result {
            Ok(result) => {
                {
                    let mut s = self.state.lock();
                    s.refresh_status_message = String::new();
                    if result.aborted && timed_out {
                        s.error_message = Some("Model refresh timed out; showing cached models.".to_string());
                    } else if result.errors.len() == 1 {
                        let key = result.errors.keys().next().cloned().unwrap_or_default();
                        s.error_message = Some(format!("Could not refresh {key}; showing cached models."));
                    } else if result.errors.len() > 1 {
                        let keys: Vec<&str> = result.errors.keys().map(String::as_str).collect();
                        s.error_message = Some(format!(
                            "Could not refresh {} model catalogs ({}); showing cached models.",
                            result.errors.len(),
                            keys.join(", ")
                        ));
                    } else {
                        s.error_message = self.model_runtime.get_error().filter(|e| !e.is_empty());
                        if s.error_message.is_none() {
                            s.refresh_status_message = "Model catalogs refreshed.".to_string();
                            s.refresh_status_success = true;
                        }
                    }
                }
                self.load_models_from_snapshot();
                self.filter_models(&self.search_input.get_value());
                self.tui.request_render(false);
            }
            Err(error) => {
                {
                    let mut s = self.state.lock();
                    s.refresh_status_message = String::new();
                    s.error_message = Some(if timed_out {
                        "Model refresh timed out; showing cached models.".to_string()
                    } else {
                        format!("Could not refresh model catalogs: {error}")
                    });
                }
                self.update_list();
                self.tui.request_render(false);
            }
        }
    }

    fn dispose(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.refresh_abort_controller.cancel();
    }

    fn handle_select(&self, model: Model) {
        self.dispose();
        (self.on_select_callback)(model);
    }

    fn selected_model(&self) -> Option<Model> {
        let s = self.state.lock();
        s.filtered_models.get(s.selected_index).map(|item| Model::clone(&item.model))
    }
}

/// Component that renders a model selector with search
/// TS: `extends Container implements Focusable`
pub struct ModelSelectorComponent {
    inner: Arc<Inner>,
}

impl ModelSelectorComponent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tui: Arc<Tui>,
        current_model: Option<Model>,
        model_runtime: Arc<ModelRuntime>,
        scoped_models: &[ScopedModelItem],
        on_select: Arc<dyn Fn(Model) + Send + Sync>,
        on_cancel: Arc<dyn Fn() + Send + Sync>,
        initial_search_input: Option<&str>,
        on_select_as_default: Option<Arc<dyn Fn(Model) + Send + Sync>>,
        default_model: Option<&DefaultModelReference>,
    ) -> Self {
        let scope = if scoped_models.is_empty() { ModelScope::All } else { ModelScope::Scoped };
        let inner = Arc::new_cyclic(|weak: &Weak<Inner>| {
            let t = theme();
            let container = Container::new();
            // Add top border
            container.add_child(Arc::new(DynamicBorder::new(None)));
            container.add_child(Arc::new(Spacer::new(1)));

            // Add hint about model filtering
            let (scope_text, scope_hint_text) = if !scoped_models.is_empty() {
                // Scope text is set right after construction (needs the theme helpers on Inner).
                let scope_text = Arc::new(Text::new("", 0, 0, None));
                container.add_child(scope_text.clone());
                let scope_hint_text = Arc::new(Text::new(Inner::get_scope_hint_text(), 0, 0, None));
                container.add_child(scope_hint_text.clone());
                (Some(scope_text), Some(scope_hint_text))
            } else {
                let hint_text = "Only showing models from configured providers. Use /login to add providers.";
                container.add_child(Arc::new(Text::new(t.fg(&ThemeColor::Warning, hint_text), 0, 0, None)));
                (None, None)
            };
            container.add_child(Arc::new(Spacer::new(1)));

            // Create search input
            let search_input = Arc::new(Input::default());
            if let Some(initial) = initial_search_input.filter(|s| !s.is_empty()) {
                search_input.set_value(initial);
            }
            let w = weak.clone();
            search_input.set_on_submit(Some(Arc::new(move |_v: String| {
                // Enter on search input selects the first filtered item
                if let Some(inner) = w.upgrade()
                    && let Some(model) = inner.selected_model()
                {
                    inner.handle_select(model);
                }
            })));
            container.add_child(search_input.clone());
            container.add_child(Arc::new(Spacer::new(1)));

            // Create list container
            let list_container = Arc::new(Container::new());
            container.add_child(list_container.clone());
            container.add_child(Arc::new(Spacer::new(1)));

            // Hint
            if on_select_as_default.is_some() {
                container.add_child(Arc::new(Text::new(
                    t.fg(
                        &ThemeColor::Dim,
                        &format!(
                            "  {} to select · {} to set as default · {} to cancel",
                            key_display_text("tui.select.confirm".into()),
                            key_display_text("app.models.save".into()),
                            key_display_text("tui.select.cancel".into())
                        ),
                    ),
                    0,
                    0,
                    None,
                )));
            }

            // Add bottom border
            container.add_child(Arc::new(DynamicBorder::new(None)));

            Inner {
                container,
                search_input,
                list_container,
                current_model,
                model_runtime,
                on_select_callback: on_select,
                on_select_as_default_callback: on_select_as_default,
                on_cancel_callback: on_cancel,
                tui,
                default_model: default_model.cloned(),
                scope_text,
                scope_hint_text,
                refresh_abort_controller: CancellationToken::new(),
                closed: AtomicBool::new(false),
                state: Mutex::new(State {
                    all_models: Vec::new(),
                    scoped_model_items: Vec::new(),
                    active_models: Vec::new(),
                    filtered_models: Vec::new(),
                    selected_index: 0,
                    error_message: None,
                    refresh_status_message: "Refreshing model catalogs…".to_string(),
                    refresh_status_success: false,
                    scoped_models: scoped_models.to_vec(),
                    scope,
                    focused: false,
                }),
            }
        });
        if let Some(scope_text) = &inner.scope_text {
            scope_text.set_text(inner.get_scope_text(scope));
        }

        // Render the current snapshot immediately, then refresh in the background.
        inner.load_models_from_snapshot();
        match initial_search_input.filter(|s| !s.is_empty()) {
            Some(query) => inner.filter_models(query),
            None => inner.update_list(),
        }
        inner.tui.request_render(false);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            crate::utils::uncaught::spawn_on(&handle, pi_tui::ui_lock::atomic(inner.clone().refresh_models()));
        }
        Self { inner }
    }

    pub fn dispose(&self) {
        self.inner.dispose();
    }

    pub fn get_search_input(&self) -> Arc<Input> {
        self.inner.search_input.clone()
    }
}

impl Component for ModelSelectorComponent {
    fn render(&self, width: usize) -> Vec<String> {
        self.inner.container.render(width)
    }

    fn handle_input(&self, data: &str) {
        let inner = &self.inner;
        let kb = get_keybindings();
        if kb.matches(data, "tui.input.tab") {
            let (has_scoped, scope) = {
                let s = inner.state.lock();
                (!s.scoped_model_items.is_empty(), s.scope)
            };
            if has_scoped {
                let next_scope = if scope == ModelScope::All { ModelScope::Scoped } else { ModelScope::All };
                inner.set_scope(next_scope);
                if let Some(hint) = &inner.scope_hint_text {
                    hint.set_text(Inner::get_scope_hint_text());
                }
            }
            return;
        }
        if kb.matches(data, "tui.select.up") {
            // Up arrow - wrap to bottom when at top
            {
                let mut s = inner.state.lock();
                let len = s.filtered_models.len();
                if len == 0 {
                    return;
                }
                s.selected_index = if s.selected_index == 0 { len - 1 } else { s.selected_index - 1 };
            }
            inner.update_list();
        } else if kb.matches(data, "tui.select.down") {
            // Down arrow - wrap to top when at bottom
            {
                let mut s = inner.state.lock();
                let len = s.filtered_models.len();
                if len == 0 {
                    return;
                }
                s.selected_index = if s.selected_index == len - 1 { 0 } else { s.selected_index + 1 };
            }
            inner.update_list();
        } else if kb.matches(data, "tui.select.confirm") {
            if let Some(model) = inner.selected_model() {
                inner.handle_select(model);
            }
        } else if kb.matches(data, "tui.select.cancel") {
            inner.dispose();
            (inner.on_cancel_callback)();
        } else if kb.matches(data, "app.models.save") && inner.on_select_as_default_callback.is_some() {
            // Select and save as default
            if let Some(model) = inner.selected_model() {
                inner.dispose();
                if let Some(cb) = &inner.on_select_as_default_callback {
                    cb(model);
                }
            }
        } else {
            // Pass everything else to search input
            inner.search_input.handle_input(data);
            inner.filter_models(&inner.search_input.get_value());
        }
    }

    fn handles_input(&self) -> bool {
        true
    }

    fn invalidate(&self) {
        self.inner.container.invalidate();
    }

    fn handle_mouse(&self, event: &MouseEvent) -> Option<MouseResult> {
        self.inner.container.handle_mouse(event)
    }

    fn child_components(&self) -> Option<Vec<ComponentRef>> {
        self.inner.container.child_components()
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focused(&self, focused: bool) {
        self.inner.state.lock().focused = focused;
        self.inner.search_input.set_focused(focused);
    }

    fn focused(&self) -> bool {
        self.inner.state.lock().focused
    }
}
