//! Selectors shown in place of the editor (settings, models, thinking, tree, sessions, trust, login).

use super::*;
use crate::config::get_auth_path;
use crate::core::agent_session::AgentSessionSetScopedModelsScopedModels;
use crate::core::defaults::{DEFAULT_THINKING_LEVEL, THINKING_LEVEL_OPTIONS};
use crate::core::model_resolver::{DEFAULT_MODEL_PER_PROVIDER, find_exact_model_reference_match, resolve_model_scope_from_models};
use crate::core::radius::RADIUS_PROVIDER_ID;
use crate::core::trust_manager::{ProjectTrustStore, has_trust_requiring_project_resources};
use crate::modes::interactive::components::extension_selector::{ExtensionSelectorComponent, ExtensionSelectorOptions};
use crate::modes::interactive::components::login_dialog::LoginDialogComponent;
use crate::modes::interactive::components::model_selector::{DefaultModelReference, ModelSelectorComponent, ScopedModelItem};
use crate::modes::interactive::components::oauth_selector::{
    AuthSelectorProvider, AuthSelectorProviderAuthType, AuthSelectorProviderMethod, OAuthSelectorComponent,
    OAuthSelectorComponentMode, format_auth_selector_provider_status, format_auth_selector_provider_type,
};
use crate::modes::interactive::components::radius_login_selector::{RadiusOption, create_login_menu_selector};
use crate::modes::interactive::components::scoped_models_selector::{
    ModelsCallbacks, ModelsConfig, ScopedModelsSelectorComponent, ScopedModelsSelectorComponentSetRefreshStatusKind,
};
use crate::modes::interactive::components::session_selector::{SessionSelectorComponent, SessionSelectorComponentOptions};
use crate::modes::interactive::components::settings_selector::{SettingsCallbacks, SettingsConfig, SettingsSelectorComponent};
use crate::modes::interactive::components::thinking_selector::ThinkingSelectorComponent;
use crate::modes::interactive::components::tree_selector::TreeSelectorComponent;
use crate::modes::interactive::components::trust_selector::{TrustSelectorComponent, TrustSelectorOptions};
use crate::modes::interactive::components::user_message_selector::{UserMessageItem, UserMessageSelectorComponent};
use crate::modes::interactive::model_catalog_refresh::refresh_model_catalogs;
use crate::modes::interactive::program_status_reporter::BlockedStatusKind;
use futures::FutureExt;
use pi_agent::types::ThinkingLevel;
use pi_ai::auth::types::{AuthEvent, AuthInteraction, AuthPrompt, AuthPromptKind, AuthType};
use pi_ai::types::Model;
use tokio_util::sync::CancellationToken;

/// What `showSelector` shows: the component, its focus target and an optional dispose hook.
pub(crate) struct SelectorParts {
    pub(crate) component: ComponentRef,
    pub(crate) focus: ComponentRef,
    pub(crate) dispose: Option<Handler>,
}

const RADIUS_LOGIN_INTRO: &str = "Radius is a service crafted for Pi by the builders of Pi, Earendil Works";

fn convert<A: serde::Serialize, B: serde::de::DeserializeOwned>(value: &A) -> B {
    serde_json::from_value(serde_json::to_value(value).expect("serializable")).expect("compatible enum values")
}

fn auth_type_order(t: LoginProviderCompletionOptionAuthTypes) -> u8 {
    match t {
        LoginProviderCompletionOptionAuthTypes::Oauth => 0,
        LoginProviderCompletionOptionAuthTypes::ApiKey => 1,
    }
}

fn completion_auth_type(t: AuthSelectorProviderAuthType) -> LoginProviderCompletionOptionAuthTypes {
    match t {
        AuthSelectorProviderAuthType::Oauth => LoginProviderCompletionOptionAuthTypes::Oauth,
        AuthSelectorProviderAuthType::ApiKey => LoginProviderCompletionOptionAuthTypes::ApiKey,
    }
}

fn selector_auth_type(t: LoginProviderCompletionOptionAuthTypes) -> AuthSelectorProviderAuthType {
    match t {
        LoginProviderCompletionOptionAuthTypes::Oauth => AuthSelectorProviderAuthType::Oauth,
        LoginProviderCompletionOptionAuthTypes::ApiKey => AuthSelectorProviderAuthType::ApiKey,
    }
}

pub(crate) fn get_login_provider_completion_options(providers: &[AuthSelectorProvider]) -> Vec<LoginProviderCompletionOption> {
    let mut by_id: IndexMap<String, LoginProviderCompletionOption> = IndexMap::new();
    for provider in providers {
        let auth_type = completion_auth_type(provider.auth_type);
        if let Some(existing) = by_id.get_mut(&provider.id) {
            if !existing.auth_types.contains(&auth_type) {
                existing.auth_types.push(auth_type);
                existing.auth_types.sort_by_key(|t| auth_type_order(*t));
            }
            continue;
        }
        by_id.insert(
            provider.id.clone(),
            LoginProviderCompletionOption {
                id: provider.id.clone(),
                name: provider.name.clone(),
                auth_types: vec![auth_type],
                subscription: provider.subscription,
            },
        );
    }
    let mut options: Vec<LoginProviderCompletionOption> = by_id.into_values().collect();
    options.sort_by(|a, b| super::resources::locale_compare(&a.name, &b.name));
    options
}

pub(crate) fn get_login_provider_search_text(provider: &LoginProviderCompletionOption) -> String {
    let auth_types = provider
        .auth_types
        .iter()
        .map(|t| format!("{} {}", t.as_str(), format_auth_selector_provider_type(selector_auth_type(*t), provider.subscription)))
        .collect::<Vec<_>>()
        .join(" ");
    format!("{} {} {auth_types}", provider.id, provider.name)
}

pub(crate) fn format_login_provider_completion_description(provider: &LoginProviderCompletionOption) -> String {
    let auth_types = provider
        .auth_types
        .iter()
        .map(|t| format_auth_selector_provider_type(selector_auth_type(*t), provider.subscription))
        .collect::<Vec<_>>()
        .join("/");
    if provider.name == provider.id { auth_types } else { format!("{} · {auth_types}", provider.name) }
}

fn llama_cpp_post_login_guidance(action_label: &str, loaded_model_count: usize) -> String {
    if loaded_model_count == 0 {
        format!("{action_label}. No llama.cpp models are loaded. Use /llama to load a model, then /model to select it.")
    } else {
        format!("{action_label}. Use /model to select a loaded llama.cpp model, or /llama to manage models.")
    }
}

/// Bridges model-runtime login prompts and events to the login dialog.
struct DialogAuthInteraction {
    mode: Weak<Inner>,
    dialog: Arc<LoginDialogComponent>,
    provider_id: String,
}

#[async_trait::async_trait]
impl AuthInteraction for DialogAuthInteraction {
    async fn prompt(&self, prompt: AuthPrompt) -> anyhow::Result<String> {
        let mode = self.mode.upgrade().ok_or_else(|| anyhow::anyhow!("Login cancelled"))?;
        let response = async {
            match prompt.kind {
                AuthPromptKind::Select => mode.show_auth_select(&self.dialog, &prompt, &self.provider_id).await,
                AuthPromptKind::ManualCode => self.dialog.show_manual_input(&prompt.message).await,
                _ => self.dialog.show_prompt(&prompt.message, prompt.placeholder.as_deref()).await,
            }
        };
        let Some(signal) = prompt.signal.clone() else { return response.await };
        if signal.is_cancelled() {
            anyhow::bail!("Login cancelled");
        }
        tokio::select! {
            result = response => result,
            _ = signal.cancelled() => Err(anyhow::anyhow!("Login cancelled")),
        }
    }

    fn notify(&self, event: AuthEvent) {
        match event {
            AuthEvent::AuthUrl { url, instructions } => self.dialog.show_auth(&url, instructions.as_deref()),
            AuthEvent::DeviceCode { user_code, verification_uri, interval_seconds, expires_in_seconds } => {
                self.dialog.show_device_code(crate::compat::OAuthDeviceCodeInfo {
                    user_code,
                    verification_uri,
                    interval_seconds,
                    expires_in_seconds,
                });
                self.dialog.show_waiting("Waiting for authentication...");
            }
            AuthEvent::Info { message, links } => self.dialog.show_info(&message, Some(&links), None),
            AuthEvent::Progress { message } => self.dialog.show_progress(&message),
        }
    }
}

impl Inner {
    pub(crate) fn dispose_active_selector(&self) {
        let dispose = {
            let mut s = self.state.lock();
            s.active_selector_token = None;
            s.active_selector_dispose.take()
        };
        if let Some(dispose) = dispose {
            dispose();
        }
    }

    /// The `done` callback of a selector shown with [`Inner::show_selector`].
    fn selector_done(&self, token: u64, dispose: Arc<Mutex<Option<Handler>>>) -> Handler {
        let weak = self.weak.clone();
        Arc::new(move || {
            let dispose = dispose.lock().clone();
            if let Some(dispose) = dispose {
                dispose();
            }
            let Some(this) = weak.upgrade() else { return };
            {
                let mut s = this.state.lock();
                if s.active_selector_token != Some(token) {
                    return;
                }
                s.active_selector_token = None;
                s.active_selector_dispose = None;
            }
            let editor = this.editor_ref();
            this.editor_container.clear();
            this.editor_container.add_child(editor.clone());
            this.ui().set_focus(Some(editor));
        })
    }

    /// Shows a selector component in place of the editor. `create` receives a `done` callback.
    pub(crate) fn show_selector(&self, create: impl FnOnce(Handler) -> SelectorParts) {
        let token = self.next_id();
        let dispose_slot: Arc<Mutex<Option<Handler>>> = Arc::new(Mutex::new(None));
        let done = self.selector_done(token, dispose_slot.clone());
        let created = create(done);
        *dispose_slot.lock() = created.dispose.clone();
        self.dispose_active_selector();
        {
            let mut s = self.state.lock();
            s.active_selector_token = Some(token);
            s.active_selector_dispose = created.dispose.clone();
        }
        self.editor_container.clear();
        self.editor_container.add_child(created.component);
        let ui = self.ui();
        ui.set_focus(Some(created.focus));
        ui.request_render(false);
    }

    fn cancel_handler(&self, done: &Handler) -> Arc<dyn Fn() + Send + Sync> {
        let done = done.clone();
        let weak = self.weak.clone();
        Arc::new(move || {
            done();
            if let Some(this) = weak.upgrade() {
                this.request_render();
            }
        })
    }

    pub(crate) fn show_settings_selector(&self) {
        let this = self.this();
        self.show_selector(move |done| {
            let session = this.session();
            let settings = this.settings_manager();
            let default_model = match (settings.get_default_provider(), settings.get_default_model()) {
                (Some(provider), Some(model)) => format!("{provider}/{model}"),
                _ => "not set".to_string(),
            };
            // Read mode state up front: a guard temporary inside the struct literal would stay locked
            // until the end of the statement and deadlock on the next `this.state` access.
            let hide_thinking_block = this.state.lock().hide_thinking_block;
            let tui_mode = this.tui_mode();
            let config = SettingsConfig {
                auto_compact: session.auto_compaction_enabled(),
                default_model,
                current_model: session.model(),
                available_default_models: session.model_runtime().get_available_snapshot(),
                show_images: settings.get_show_images(),
                image_width_cells: settings.get_image_width_cells(),
                auto_resize_images: settings.get_image_auto_resize(),
                block_images: settings.get_block_images(),
                enable_skill_commands: settings.get_enable_skill_commands(),
                steering_mode: session.steering_mode(),
                follow_up_mode: session.follow_up_mode(),
                transport: settings.get_transport(),
                session_share_destination: settings.get_session_share_destination(),
                http_idle_timeout_ms: settings.get_http_idle_timeout_ms().unwrap_or(0),
                cache_warming_mode: settings.get_cache_warming_mode(),
                thinking_level: settings
                    .get_default_thinking_level_setting()
                    .unwrap_or_else(|| DEFAULT_THINKING_LEVEL.into()),
                available_thinking_levels: THINKING_LEVEL_OPTIONS.to_vec(),
                model_thinking_levels: settings.get_all_model_thinking_level_settings(),
                current_theme: this
                    .theme_controller
                    .get_theme_selection()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| crate::modes::interactive::theme::theme::SYSTEM_THEME_NAME.to_string()),
                terminal_theme: this.theme_controller.get_terminal_theme(),
                available_themes: crate::modes::interactive::theme::theme::get_available_themes(),
                hide_thinking_block,
                mermaid_rendering_mode: settings.get_mermaid_rendering_mode(),
                show_cache_miss_notices: settings.get_show_cache_miss_notices(),
                collapse_changelog: settings.get_collapse_changelog(),
                enable_install_telemetry: settings.get_enable_install_telemetry(),
                double_escape_action: settings.get_double_escape_action(),
                tree_filter_mode: settings.get_tree_filter_mode(),
                show_hardware_cursor: settings.get_show_hardware_cursor(),
                editor_padding_x: settings.get_editor_padding_x(),
                output_pad: settings.get_output_pad(),
                autocomplete_max_visible: settings.get_autocomplete_max_visible(),
                quiet_startup: settings.get_quiet_startup(),
                default_project_trust: settings.get_default_project_trust(),
                clear_on_shrink: settings.get_clear_on_shrink(),
                show_terminal_progress: settings.get_show_terminal_progress(),
                tui_mode,
                fullscreen_exit_output: settings.get_fullscreen_exit_output(),
                fullscreen_scrollbar: settings.get_fullscreen_scrollbar(),
                fullscreen_copy_on_select: settings.get_fullscreen_copy_on_select(),
                fullscreen_wheel_scroll_lines: settings.get_fullscreen_wheel_scroll_lines(),
                warnings: settings.get_warnings(),
            };
            let selector_slot: Arc<Mutex<Option<Arc<SettingsSelectorComponent>>>> = Arc::new(Mutex::new(None));
            let w = this.weak.clone();
            let with = move |f: &dyn Fn(Arc<Inner>)| {
                if let Some(this) = w.upgrade() {
                    f(this);
                }
            };
            macro_rules! cb {
                (|$this:ident, $arg:ident : $ty:ty| $body:expr) => {{
                    let with = with.clone();
                    Arc::new(move |$arg: $ty| with(&|$this: Arc<Inner>| { let $arg = $arg.clone(); $body; }))
                }};
            }
            let selector_for_tui = selector_slot.clone();
            let callbacks = SettingsCallbacks {
                on_auto_compact_change: cb!(|this, enabled: bool| {
                    this.session().set_auto_compaction_enabled(enabled);
                    this.footer.set_auto_compact_enabled(enabled);
                }),
                on_show_images_change: cb!(|this, enabled: bool| {
                    this.settings_manager().set_show_images(enabled);
                    for child in this.chat_container.children() {
                        if let Some(c) = pi_tui::tui::downcast_component::<ToolExecutionComponent>(&child) {
                            c.set_show_images(enabled);
                        }
                    }
                }),
                on_image_width_cells_change: cb!(|this, width: i64| {
                    this.settings_manager().set_image_width_cells(width);
                    for child in this.chat_container.children() {
                        if let Some(c) = pi_tui::tui::downcast_component::<ToolExecutionComponent>(&child) {
                            c.set_image_width_cells(width);
                        }
                    }
                }),
                on_auto_resize_images_change: cb!(|this, enabled: bool| this.settings_manager().set_image_auto_resize(enabled)),
                on_block_images_change: cb!(|this, blocked: bool| this.settings_manager().set_block_images(blocked)),
                on_enable_skill_commands_change: cb!(|this, enabled: bool| {
                    this.settings_manager().set_enable_skill_commands(enabled);
                    this.setup_autocomplete_provider();
                }),
                on_steering_mode_change: cb!(|this, mode: crate::modes::interactive::components::settings_selector::SettingsConfigSteeringMode| {
                    this.session().set_steering_mode(convert(&mode))
                }),
                on_follow_up_mode_change: cb!(|this, mode: crate::modes::interactive::components::settings_selector::SettingsConfigSteeringMode| {
                    this.session().set_follow_up_mode(convert(&mode))
                }),
                on_transport_change: cb!(|this, transport: pi_ai::types::Transport| {
                    this.settings_manager().set_transport(&convert(&transport));
                    this.session().agent().set_transport(transport);
                }),
                on_session_share_destination_change: cb!(|this, destination: crate::core::settings_schema::SessionShareDestination| {
                    this.settings_manager().set_session_share_destination(&destination)
                }),
                on_http_idle_timeout_ms_change: cb!(|this, timeout_ms: i64| {
                    let _ = this.settings_manager().set_http_idle_timeout_ms(timeout_ms);
                    let _ = crate::core::http_dispatcher::configure_http_dispatcher(Some(timeout_ms));
                    this.show_status(&format!(
                        "HTTP idle timeout: {}",
                        crate::core::http_dispatcher::format_http_idle_timeout_ms(timeout_ms)
                    ));
                }),
                on_cache_warming_mode_change: cb!(|this, mode: crate::core::settings_schema::CacheWarmingMode| {
                    this.session().set_cache_warming_mode(&mode);
                    this.show_status(&format!("Cache warming: {}", mode.as_str()));
                }),
                on_model_thinking_level_change: {
                    let with = with.clone();
                    Arc::new(move |provider: String, model_id: String, level: ThinkingLevel| {
                        let (provider, model_id) = (provider.clone(), model_id.clone());
                        with(&|this: Arc<Inner>| {
                            this.settings_manager().set_model_thinking_level(&provider, &model_id, level);
                            // If the override is for the current model, apply it to the session too
                            if let Some(current) = this.session().model()
                                && current.provider == provider
                                && current.id == model_id
                            {
                                this.session().set_thinking_level(level, None);
                                this.footer.invalidate();
                                this.update_editor_border_color();
                            }
                        })
                    })
                },
                on_model_thinking_level_remove: {
                    let with = with.clone();
                    Arc::new(move |provider: String, model_id: String| {
                        let (provider, model_id) = (provider.clone(), model_id.clone());
                        with(&|this: Arc<Inner>| {
                            let settings = this.settings_manager();
                            settings.remove_model_thinking_level(&provider, &model_id);
                            if let Some(current) = this.session().model()
                                && current.provider == provider
                                && current.id == model_id
                            {
                                let global = settings.get_default_thinking_level().unwrap_or(DEFAULT_THINKING_LEVEL);
                                this.session().set_thinking_level(global, None);
                                this.footer.invalidate();
                                this.update_editor_border_color();
                            }
                        })
                    })
                },
                on_theme_change: cb!(|this, theme_setting: String| {
                    this.settings_manager().set_theme(&theme_setting);
                    this.theme_controller.set_theme_setting(&theme_setting);
                }),
                on_theme_preview: Some(cb!(|this, name: String| this.theme_controller.preview(&name))),
                on_hide_thinking_block_change: cb!(|this, hidden: bool| {
                    this.state.lock().hide_thinking_block = hidden;
                    this.settings_manager().set_hide_thinking_block(hidden);
                    this.update_thinking_block_visibility();
                }),
                on_mermaid_rendering_mode_change: cb!(|this, mode: crate::core::settings_schema::MermaidRenderingMode| {
                    this.settings_manager().set_mermaid_rendering_mode(&mode);
                    this.chat_container.invalidate();
                    this.request_render();
                }),
                on_show_cache_miss_notices_change: cb!(|this, shown: bool| {
                    this.settings_manager().set_show_cache_miss_notices(shown);
                    this.rebuild_chat_from_messages();
                }),
                on_collapse_changelog_change: cb!(|this, collapsed: bool| this.settings_manager().set_collapse_changelog(collapsed)),
                on_enable_install_telemetry_change: cb!(|this, enabled: bool| this.settings_manager().set_enable_install_telemetry(enabled)),
                on_double_escape_action_change: cb!(|this, action: crate::modes::interactive::components::settings_selector::SettingsConfigDoubleEscapeAction| {
                    this.settings_manager().set_double_escape_action(convert(&action))
                }),
                on_tree_filter_mode_change: cb!(|this, mode: crate::modes::interactive::components::tree_selector::FilterMode| {
                    this.settings_manager().set_tree_filter_mode(mode)
                }),
                on_show_hardware_cursor_change: cb!(|this, enabled: bool| {
                    this.settings_manager().set_show_hardware_cursor(enabled);
                    this.ui().set_show_hardware_cursor(enabled);
                }),
                on_editor_padding_x_change: cb!(|this, padding: i64| {
                    this.settings_manager().set_editor_padding_x(padding);
                    let padding = padding.max(0) as usize;
                    this.default_editor.set_padding_x(padding);
                    if !this.state.lock().editor_is_default {
                        this.editor().set_padding_x(padding);
                    }
                }),
                on_output_pad_change: cb!(|this, padding: i64| {
                    this.settings_manager().set_output_pad(padding);
                    this.state.lock().output_pad = padding;
                    for container in [&this.chat_container, &this.pending_messages_container] {
                        for child in container.children() {
                            set_output_pad_on(&child, padding);
                        }
                    }
                    this.request_render();
                }),
                on_autocomplete_max_visible_change: cb!(|this, max_visible: i64| {
                    this.settings_manager().set_autocomplete_max_visible(max_visible);
                    let max_visible = max_visible.max(0) as usize;
                    this.default_editor.set_autocomplete_max_visible(max_visible);
                    if !this.state.lock().editor_is_default {
                        this.editor().set_autocomplete_max_visible(max_visible);
                    }
                }),
                on_quiet_startup_change: cb!(|this, quiet: crate::core::settings_schema::QuietStartup| {
                    this.settings_manager().set_quiet_startup(&quiet)
                }),
                on_default_project_trust_change: cb!(|this, value: crate::core::settings_schema::DefaultProjectTrust| {
                    this.settings_manager().set_default_project_trust(&value)
                }),
                on_clear_on_shrink_change: cb!(|this, enabled: bool| {
                    this.settings_manager().set_clear_on_shrink(enabled);
                    this.ui().set_clear_on_shrink(enabled);
                    if !enabled && this.state.lock().active_status_indicator.is_none() {
                        this.status_container.clear();
                    }
                }),
                on_show_terminal_progress_change: cb!(|this, enabled: bool| this.settings_manager().set_show_terminal_progress(enabled)),
                on_tui_mode_change: {
                    let with = with.clone();
                    let selector_slot = selector_for_tui.clone();
                    Arc::new(move |mode: TuiMode| {
                        let selector_slot = selector_slot.clone();
                        with(&move |this: Arc<Inner>| {
                            if !this.switch_tui_mode(mode, true, true) {
                                let selector = selector_slot.lock().clone();
                                if let Some(selector) = selector {
                                    selector
                                        .get_settings_list()
                                        .update_value("tui-mode", this.tui_mode().as_str());
                                }
                                this.show_status("Close active overlays before changing TUI mode");
                                return;
                            }
                            this.settings_manager().set_tui_mode(&mode);
                            if this.state.lock().active_status_indicator.is_none() {
                                this.status_container.clear();
                            }
                            this.show_status(&format!("TUI mode: {}", mode.as_str()));
                        })
                    })
                },
                on_fullscreen_exit_output_change: cb!(|this, output: crate::core::settings_schema::FullscreenExitOutput| {
                    this.settings_manager().set_fullscreen_exit_output(&output)
                }),
                on_fullscreen_scrollbar_change: cb!(|this, mode: pi_tui::components::scroll_view::ScrollViewScrollbar| {
                    this.settings_manager().set_fullscreen_scrollbar(mode);
                    this.apply_fullscreen_scrollbar_setting();
                }),
                on_fullscreen_copy_on_select_change: cb!(|this, enabled: bool| {
                    this.settings_manager().set_fullscreen_copy_on_select(enabled);
                    let ui = this.ui();
                    if ui.is_viewport_tui() {
                        ui.set_copy_on_select(enabled);
                    }
                }),
                on_fullscreen_wheel_scroll_lines_change: cb!(|this, lines: crate::compat::WheelScrollLines| {
                    this.settings_manager().set_fullscreen_wheel_scroll_lines(&lines);
                    let ui = this.ui();
                    if ui.is_viewport_tui() {
                        ui.set_wheel_scroll_lines(crate::modes::interactive::tui_renderer::to_tui_wheel_lines(&Some(lines)));
                    }
                }),
                on_warnings_change: cb!(|this, warnings: crate::core::settings_schema::WarningSettings| {
                    this.settings_manager().set_warnings(&warnings)
                }),
                on_cancel: this.cancel_handler(&done),
            };
            let selector = Arc::new(SettingsSelectorComponent::new(&config, &callbacks));
            *selector_slot.lock() = Some(selector.clone());
            let focus: ComponentRef = selector.get_settings_list();
            SelectorParts { component: selector, focus, dispose: None }
        });
    }

    pub(crate) fn handle_thinking_command(&self, search_term: Option<&str>) {
        let available = self.session().get_available_thinking_levels();
        let Some(search_term) = search_term.filter(|s| !s.is_empty()) else {
            self.show_thinking_selector();
            return;
        };
        let normalized = search_term.trim().to_lowercase();
        let Some(level) = available.iter().find(|l| l.as_str().to_lowercase() == normalized).copied() else {
            self.show_error(&format!(
                "Unknown thinking level \"{search_term}\". Available levels: {}.",
                available.iter().map(|l| l.as_str()).collect::<Vec<_>>().join(", ")
            ));
            return;
        };
        self.select_thinking_level(level, false);
    }

    fn select_thinking_level(&self, level: ThinkingLevel, persist: bool) {
        self.session()
            .set_thinking_level(level, Some(&crate::core::agent_session::ModelMutationOptions { persist: Some(persist) }));
        self.footer.invalidate();
        self.update_editor_border_color();
        self.show_status(&if persist {
            format!("Default thinking level: {}", level.as_str())
        } else {
            format!("Thinking level: {}", level.as_str())
        });
    }

    fn show_thinking_selector(&self) {
        let this = self.this();
        self.show_selector(move |done| {
            let session = this.session();
            let select = {
                let weak = this.weak.clone();
                let done = done.clone();
                move |level: ThinkingLevel, persist: bool| {
                    if let Some(this) = weak.upgrade() {
                        this.select_thinking_level(level, persist);
                    }
                    done();
                }
            };
            let select = Arc::new(select);
            let s1 = select.clone();
            let s2 = select;
            let selector = Arc::new(ThinkingSelectorComponent::new(
                session.thinking_level(),
                &session.get_available_thinking_levels(),
                Arc::new(move |level| s1(level, false)),
                this.cancel_handler(&done),
                Some(Arc::new(move |level| s2(level, true))),
                // TS passes the raw setting; an unrecognized value marks no level as the default.
                match this.settings_manager().get_default_thinking_level_setting() {
                    None => Some(DEFAULT_THINKING_LEVEL),
                    Some(level) => level.known_level(),
                },
            ));
            SelectorParts { component: selector.clone(), focus: selector, dispose: None }
        });
    }

    pub(crate) async fn handle_model_command(self: Arc<Self>, search_term: Option<String>) {
        let Some(search_term) = search_term.filter(|s| !s.is_empty()) else {
            self.show_model_selector(None);
            return;
        };
        match self.find_exact_model_match(&search_term).await {
            Some(model) => {
                match self
                    .session()
                    .set_model(model.clone(), Some(&crate::core::agent_session::ModelMutationOptions { persist: Some(false) }))
                    .await
                {
                    Ok(()) => {
                        self.footer.invalidate();
                        self.update_editor_border_color();
                        self.show_status(&format!("Model: {}", model.id));
                        let this = self.clone();
                        self.spawn(async move { this.maybe_warn_about_anthropic_subscription_auth(Some(model)).await });
                    }
                    Err(error) => self.show_error(&error.to_string()),
                }
            }
            None => self.show_model_selector(Some(&search_term)),
        }
    }

    /// pi-rs divergence from TS Pi: the catalog refresh is on demand only. This is what `/refresh-models` runs.
    pub(crate) async fn handle_refresh_models_command(self: Arc<Self>) {
        if std::env::var_os("PI_OFFLINE").is_some_and(|v| !v.is_empty()) {
            self.show_warning("Offline mode: model catalogs were not refreshed.");
            return;
        }
        self.show_status("Refreshing model catalogs…");
        let signal = CancellationToken::new();
        let timed_out = Arc::new(AtomicBool::new(false));
        let timer = {
            let signal = signal.clone();
            let timed_out = timed_out.clone();
            crate::utils::uncaught::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                timed_out.store(true, Ordering::SeqCst);
                signal.cancel();
            })
        };
        let result = refresh_model_catalogs(&self.session().model_runtime(), signal).await;
        timer.abort();
        match result {
            Ok(result) => {
                if result.aborted && timed_out.load(Ordering::SeqCst) {
                    self.show_warning("Model refresh timed out; showing cached models.");
                } else if !result.errors.is_empty() {
                    self.show_warning(&format!(
                        "Could not refresh {}; showing cached models.",
                        result.errors.keys().cloned().collect::<Vec<_>>().join(", ")
                    ));
                } else {
                    self.update_available_provider_count();
                    self.footer.invalidate();
                    self.show_status("Model catalogs refreshed.");
                }
            }
            Err(error) => self.show_warning(&if timed_out.load(Ordering::SeqCst) {
                "Model refresh timed out; showing cached models.".to_string()
            } else {
                format!("Could not refresh model catalogs: {error}")
            }),
        }
        self.request_render();
    }

    async fn find_exact_model_match(&self, search_term: &str) -> Option<Model> {
        let session = self.session();
        let scoped = session.scoped_models();
        let cached: Vec<Model> = if !scoped.is_empty() {
            scoped.iter().map(|s| s.model.clone()).collect()
        } else {
            session.model_runtime().get_available_snapshot()
        };
        // pi-rs divergence from TS Pi: never fetch catalogs just because `/model <ref>` missed the cache.
        find_exact_model_reference_match(search_term, &cached)
    }

    pub(crate) fn maybe_save_implicit_project_trust_after_reload(&self) -> bool {
        let cwd = self.session_manager().get_cwd();
        if self.state.lock().auto_trust_on_reload_cwd.as_deref() != Some(cwd.as_str()) {
            return false;
        }
        if !self.settings_manager().is_project_trusted() || !has_trust_requiring_project_resources(&cwd) {
            return false;
        }
        let store = ProjectTrustStore::new(&self.runtime_host.services().agent_dir);
        match store.get(&cwd) {
            Ok(Some(_)) => {
                self.state.lock().auto_trust_on_reload_cwd = None;
                false
            }
            Ok(None) => match store.set(&cwd, Some(true)) {
                Ok(()) => {
                    self.state.lock().auto_trust_on_reload_cwd = None;
                    true
                }
                Err(error) => {
                    self.show_warning(&format!("Could not save project trust after reload: {error}"));
                    false
                }
            },
            Err(error) => {
                self.show_warning(&format!("Could not save project trust after reload: {error}"));
                false
            }
        }
    }

    pub(crate) fn show_trust_selector(&self) {
        let cwd = self.session_manager().get_cwd();
        let store = Arc::new(ProjectTrustStore::new(&self.runtime_host.services().agent_dir));
        let saved = store.get_entry(&cwd).ok().flatten();
        let this = self.this();
        self.show_selector(move |done| {
            let weak = this.weak.clone();
            let done_select = done.clone();
            let selector = Arc::new(TrustSelectorComponent::new(&TrustSelectorOptions {
                cwd,
                saved_decision: saved,
                project_trusted: this.settings_manager().is_project_trusted(),
                on_select: Arc::new(move |selection| {
                    let _ = store.set_many(&selection.updates);
                    done_select();
                    if let Some(this) = weak.upgrade() {
                        this.show_status(&format!(
                            "Saved trust decision: {}. Restart {APP_NAME} for this to take effect.",
                            if selection.trusted { "trusted" } else { "untrusted" }
                        ));
                    }
                }),
                on_cancel: this.cancel_handler(&done),
            }));
            SelectorParts { component: selector.clone(), focus: selector, dispose: None }
        });
    }

    pub(crate) fn show_model_selector(&self, initial_search_input: Option<&str>) {
        let this = self.this();
        let initial = initial_search_input.map(str::to_string);
        self.show_selector(move |done| {
            let session = this.session();
            let select_model = {
                let weak = this.weak.clone();
                let done = done.clone();
                Arc::new(move |model: Model, persist: bool| {
                    let Some(this) = weak.upgrade() else { return };
                    let done = done.clone();
                    let target = this.clone();
                    // Like the TS async handler: run synchronously up to the first await, so a fast
                    // `setModel` closes the selector before the next key arrives (keys typed in
                    // between would otherwise go to the closing selector).
                    this.start_now(async move {
                        let result = target
                            .session()
                            .set_model(model.clone(), Some(&crate::core::agent_session::ModelMutationOptions { persist: Some(persist) }))
                            .await;
                        match result {
                            Ok(()) => {
                                target.update_available_provider_count();
                                target.footer.invalidate();
                                target.update_editor_border_color();
                                done();
                                target.show_status(&if persist {
                                    format!("Default model: {}/{}", model.provider, model.id)
                                } else {
                                    format!("Model: {}", model.id)
                                });
                                target.maybe_warn_about_anthropic_subscription_auth(Some(model)).await;
                            }
                            Err(error) => {
                                done();
                                target.show_error(&error.to_string());
                            }
                        }
                    });
                })
            };
            let settings = this.settings_manager();
            let default_model = match (settings.get_default_provider(), settings.get_default_model()) {
                (Some(provider), Some(id)) => Some(DefaultModelReference { provider, id }),
                _ => None,
            };
            let scoped: Vec<ScopedModelItem> = session
                .scoped_models()
                .into_iter()
                .map(|s| ScopedModelItem { model: s.model, thinking_level: s.thinking_level.map(|l| l.as_str().to_string()) })
                .collect();
            let s1 = select_model.clone();
            let s2 = select_model;
            let selector = Arc::new(ModelSelectorComponent::new(
                this.ui(),
                session.model(),
                session.model_runtime(),
                &scoped,
                Arc::new(move |model| s1(model, false)),
                this.cancel_handler(&done),
                initial.as_deref(),
                Some(Arc::new(move |model| s2(model, true))),
                default_model.as_ref(),
            ));
            let dispose_target = selector.clone();
            SelectorParts {
                component: selector.clone(),
                focus: selector,
                dispose: Some(Arc::new(move || dispose_target.dispose())),
            }
        });
    }

    pub(crate) fn show_models_selector(&self) {
        let session = self.session();
        let available = Arc::new(Mutex::new(session.model_runtime().get_available_snapshot()));
        let configured_patterns = self.settings_manager().get_enabled_models();
        let session_scoped = session.scoped_models();
        let configured_enabled_ids = {
            let patterns = configured_patterns.clone();
            move |models: &[Model]| -> Option<Vec<String>> {
                let patterns = patterns.as_ref().filter(|p| !p.is_empty())?;
                let resolved = resolve_model_scope_from_models(patterns, models);
                let mut ids: Vec<String> =
                    resolved.scoped_models.iter().map(|s| format!("{}/{}", s.model.provider, s.model.id)).collect();
                for diagnostic in resolved.diagnostics {
                    if diagnostic.code.as_str() == "no-match" && !ids.contains(&diagnostic.pattern) {
                        ids.push(diagnostic.pattern);
                    }
                }
                Some(ids)
            }
        };
        let current_enabled: Arc<Mutex<Option<Vec<String>>>> = Arc::new(Mutex::new(if !session_scoped.is_empty() {
            Some(session_scoped.iter().map(|s| format!("{}/{}", s.model.provider, s.model.id)).collect())
        } else {
            configured_enabled_ids(&available.lock())
        }));
        let selection_changed = Arc::new(AtomicBool::new(false));

        let update_session_models = {
            let weak = self.weak.clone();
            let available = available.clone();
            let current_enabled = current_enabled.clone();
            Arc::new(move |enabled: Option<Vec<String>>| {
                let Some(this) = weak.upgrade() else { return };
                *current_enabled.lock() = enabled.clone();
                let models = available.lock().clone();
                let ids: HashSet<String> = models.iter().map(|m| format!("{}/{}", m.provider, m.id)).collect();
                let has_enabled_available = enabled.as_ref().is_some_and(|e| e.iter().any(|id| ids.contains(id)));
                let all_enabled = enabled.as_ref().is_some_and(|e| ids.iter().all(|id| e.contains(id)));
                if let Some(enabled) = enabled.as_ref().filter(|_| has_enabled_available && !all_enabled) {
                    let scoped = resolve_model_scope_from_models(enabled, &models).scoped_models;
                    this.session().set_scoped_models(
                        &scoped
                            .into_iter()
                            .map(|s| AgentSessionSetScopedModelsScopedModels { model: s.model, thinking_level: s.thinking_level })
                            .collect::<Vec<_>>(),
                    );
                } else {
                    this.session().set_scoped_models(&[]);
                }
                this.update_available_provider_count();
                this.request_render();
            })
        };

        let this = self.this();
        self.show_selector(move |done| {
            let disposed = Arc::new(AtomicBool::new(false));
            let timed_out = Arc::new(AtomicBool::new(false));
            let signal = CancellationToken::new();
            let initial_models = available.lock().clone();
            let on_change = {
                let update = update_session_models.clone();
                let changed = selection_changed.clone();
                Arc::new(move |enabled: Option<Vec<String>>| {
                    changed.store(true, Ordering::SeqCst);
                    update(enabled);
                    futures::future::ready(Ok(())).boxed()
                })
            };
            let on_persist = {
                let weak = this.weak.clone();
                let available = available.clone();
                Arc::new(move |enabled: Option<Vec<String>>| {
                    if let Some(this) = weak.upgrade() {
                        let models = available.lock().clone();
                        let ids: HashSet<String> = models.iter().map(|m| format!("{}/{}", m.provider, m.id)).collect();
                        let all_enabled = enabled
                            .as_ref()
                            .is_some_and(|e| e.len() == models.len() && e.iter().all(|id| ids.contains(id)));
                        let patterns = if all_enabled { None } else { enabled };
                        this.settings_manager().set_enabled_models(patterns.as_deref());
                        this.show_status("Model selection saved to settings");
                    }
                    futures::future::ready(Ok(())).boxed()
                })
            };
            let selector = Arc::new(ScopedModelsSelectorComponent::new(
                &ModelsConfig {
                    all_models: initial_models,
                    enabled_model_ids: current_enabled.lock().clone(),
                    refresh_status: Some("Refreshing model catalogs…".to_string()),
                },
                &ModelsCallbacks { on_change, on_persist, on_cancel: this.cancel_handler(&done) },
            ));

            // Refresh catalogs in the background.
            {
                let weak = this.weak.clone();
                let selector = selector.clone();
                let disposed = disposed.clone();
                let timed_out = timed_out.clone();
                let signal = signal.clone();
                let available = available.clone();
                let current_enabled = current_enabled.clone();
                let selection_changed = selection_changed.clone();
                let update = update_session_models.clone();
                let configured_enabled_ids = configured_enabled_ids.clone();
                let session_scoped_empty = session_scoped.is_empty();
                this.spawn(async move {
                    let timer = {
                        let signal = signal.clone();
                        let timed_out = timed_out.clone();
                        crate::utils::uncaught::spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                            timed_out.store(true, Ordering::SeqCst);
                            signal.cancel();
                        })
                    };
                    let Some(this) = weak.upgrade() else { return };
                    let result = refresh_model_catalogs(&this.session().model_runtime(), signal).await;
                    timer.abort();
                    if disposed.load(Ordering::SeqCst) {
                        return;
                    }
                    use ScopedModelsSelectorComponentSetRefreshStatusKind as Kind;
                    match result {
                        Ok(result) => {
                            let models = this.session().model_runtime().get_available_snapshot();
                            *available.lock() = models.clone();
                            if !selection_changed.load(Ordering::SeqCst) && session_scoped_empty {
                                let ids = configured_enabled_ids(&models);
                                *current_enabled.lock() = ids.clone();
                                selector.update_models(&models, Some(ids));
                            } else {
                                selector.update_models(&models, None);
                            }
                            let current = current_enabled.lock().clone();
                            if current.is_some() {
                                update(current);
                            }
                            if result.aborted && timed_out.load(Ordering::SeqCst) {
                                selector.set_refresh_status("Model refresh timed out; showing cached models.", Kind::Warning);
                            } else if !result.errors.is_empty() {
                                selector.set_refresh_status(
                                    &format!(
                                        "Could not refresh {}; showing cached models.",
                                        result.errors.keys().cloned().collect::<Vec<_>>().join(", ")
                                    ),
                                    Kind::Warning,
                                );
                            } else {
                                selector.set_refresh_status("Model catalogs refreshed.", Kind::Success);
                            }
                        }
                        Err(error) => {
                            selector.set_refresh_status(
                                &if timed_out.load(Ordering::SeqCst) {
                                    "Model refresh timed out; showing cached models.".to_string()
                                } else {
                                    format!("Could not refresh model catalogs: {error}")
                                },
                                Kind::Warning,
                            );
                        }
                    }
                    this.request_render();
                });
            }
            let dispose: Handler = {
                let disposed = disposed.clone();
                let signal = signal.clone();
                Arc::new(move || {
                    disposed.store(true, Ordering::SeqCst);
                    signal.cancel();
                })
            };
            SelectorParts { component: selector.clone(), focus: selector, dispose: Some(dispose) }
        });
    }

    pub(crate) fn show_user_message_selector(&self) {
        let messages = self.session().get_user_messages_for_forking();
        if messages.is_empty() {
            self.show_status("No messages to fork from");
            return;
        }
        let initial = messages.last().map(|m| m.entry_id.clone());
        let this = self.this();
        self.show_selector(move |done| {
            let items: Vec<UserMessageItem> =
                messages.iter().map(|m| UserMessageItem { id: m.entry_id.clone(), text: m.text.clone(), timestamp: None }).collect();
            let weak = this.weak.clone();
            let done_select = done.clone();
            let selector = Arc::new(UserMessageSelectorComponent::new(
                &items,
                Arc::new(move |entry_id: String| {
                    done_select();
                    let Some(this) = weak.upgrade() else { return };
                    let target = this.clone();
                    this.spawn(async move {
                        match target.runtime_host.fork(&entry_id, None).await {
                            Ok(result) => {
                                if result.cancelled {
                                    target.request_render();
                                    return;
                                }
                                target.editor().set_text(result.selected_text.as_deref().unwrap_or(""));
                                target.show_status("Forked to new session");
                            }
                            Err(error) => target.show_error(&error.to_string()),
                        }
                    });
                }),
                this.cancel_handler(&done),
                initial.as_deref(),
            ));
            let focus: ComponentRef = selector.get_message_list();
            SelectorParts { component: selector, focus, dispose: None }
        });
    }

    pub(crate) fn show_tree_selector(&self, initial_selected_id: Option<String>) {
        let sm = self.session_manager();
        let tree = sm.get_tree();
        let real_leaf_id = sm.get_leaf_id();
        let initial_filter_mode = self.settings_manager().get_tree_filter_mode();
        if tree.is_empty() {
            self.show_status("No entries in session");
            return;
        }
        let this = self.this();
        self.show_selector(move |done| {
            let weak = this.weak.clone();
            let done_select = done.clone();
            let on_select = Arc::new(move |entry_id: String| {
                let Some(this) = weak.upgrade() else { return };
                let done = done_select.clone();
                let target = this.clone();
                this.spawn(async move { target.navigate_to_tree_entry(entry_id, done).await });
            });
            let weak_label = this.weak.clone();
            let selector = Arc::new(TreeSelectorComponent::new(
                &tree,
                real_leaf_id.as_deref(),
                this.ui().terminal().rows() as i64,
                on_select,
                this.cancel_handler(&done),
                Some(Arc::new(move |entry_id: String, label: Option<String>| {
                    if let Some(this) = weak_label.upgrade() {
                        let _ = this.session_manager().append_label_change(&entry_id, label.as_deref());
                        this.request_render();
                    }
                })),
                initial_selected_id.as_deref(),
                Some(&initial_filter_mode),
            ));
            let weak_copy = this.weak.clone();
            selector.set_on_copy(Some(Arc::new(move |text: Option<String>| {
                let Some(this) = weak_copy.upgrade() else { return };
                let Some(text) = text.filter(|t| !t.is_empty()) else {
                    this.show_error("Selected entry has no text to copy");
                    return;
                };
                let target = this.clone();
                this.spawn(async move {
                    match crate::utils::clipboard::copy_to_clipboard(&text).await {
                        Ok(()) => target.show_status("Copied selected message to clipboard"),
                        Err(error) => target.show_error(&error.to_string()),
                    }
                });
            })));
            SelectorParts { component: selector.clone(), focus: selector, dispose: None }
        });
    }

    async fn navigate_to_tree_entry(self: Arc<Self>, entry_id: String, done: Handler) {
        // Selecting the current leaf is a no-op (already there)
        if Some(entry_id.as_str()) == self.session_manager().get_leaf_id().as_deref() {
            done();
            self.show_status("Already at this point");
            return;
        }
        done();

        let mut wants_summary = false;
        let mut custom_instructions: Option<String> = None;
        if !self.settings_manager().get_branch_summary_skip_prompt() {
            loop {
                let choice = self
                    .show_extension_selector(
                        "Summarize branch?",
                        vec!["No summary".to_string(), "Summarize".to_string(), "Summarize with custom prompt".to_string()],
                        Default::default(),
                        None,
                    )
                    .await;
                let Some(choice) = choice else {
                    // User pressed escape - re-show tree selector with same selection
                    self.show_tree_selector(Some(entry_id));
                    return;
                };
                wants_summary = choice != "No summary";
                if choice == "Summarize with custom prompt" {
                    custom_instructions = self.show_extension_editor("Custom summarization instructions", None).await;
                    if custom_instructions.is_none() {
                        continue;
                    }
                }
                break;
            }
        }

        let session = self.session();
        // The user committed to navigating: stop the active response first.
        if session.is_streaming() {
            self.restore_queued_messages_to_editor(false, None);
            let _ = session.abort().await;
        }
        if session.is_compacting() {
            self.show_error("Wait for the current compaction or tree navigation to finish before navigating the session tree.");
            return;
        }

        let original_on_escape = self.default_editor.on_escape();
        let mut showing_indicator = false;
        if wants_summary {
            let weak = self.weak.clone();
            self.default_editor.set_on_escape(Some(Arc::new(move || {
                if let Some(this) = weak.upgrade() {
                    this.session().abort_branch_summary();
                }
            })));
            self.add_spacer();
            self.show_status_indicator(
                crate::modes::interactive::components::status_indicator::BranchSummaryStatusIndicator::new(self.ui())
                    .indicator(),
            );
            showing_indicator = true;
            self.request_render();
        }

        let result = session
            .navigate_tree(
                &entry_id,
                Some(&crate::core::agent_session::AgentSessionNavigateTreeOptions {
                    summarize: Some(wants_summary),
                    custom_instructions,
                    replace_instructions: None,
                    label: None,
                }),
            )
            .await;
        match result {
            Ok(result) => {
                if result.aborted == Some(true) {
                    self.show_status("Branch summarization cancelled");
                    self.finish_tree_navigation(showing_indicator, original_on_escape);
                    self.show_tree_selector(Some(entry_id));
                    return;
                }
                if result.cancelled {
                    self.show_status("Navigation cancelled");
                } else {
                    self.chat_container.clear();
                    self.render_initial_messages();
                    if let Some(text) = &result.editor_text
                        && self.editor().get_text().trim().is_empty()
                    {
                        self.editor().set_text(text);
                    }
                    self.show_status("Navigated to selected point");
                    let this = self.clone();
                    self.spawn(async move { this.flush_compaction_queue(false).await });
                }
            }
            Err(error) => self.show_error(&error.to_string()),
        }
        self.finish_tree_navigation(showing_indicator, original_on_escape);
    }

    fn finish_tree_navigation(&self, showing_indicator: bool, original_on_escape: Option<Handler>) {
        if showing_indicator {
            self.clear_status_indicator(Some(StatusIndicatorKind::BranchSummary));
        }
        self.default_editor.set_on_escape(original_on_escape);
    }

    pub(crate) fn show_session_selector(&self) {
        let this = self.this();
        self.show_selector(move |done| {
            let sm = this.session_manager();
            let cwd = sm.get_cwd();
            let session_dir = sm.get_session_dir();
            let uses_default = sm.uses_default_session_dir();
            let current_loader: crate::modes::interactive::components::session_selector::SessionsLoader = {
                let (cwd, dir) = (cwd.clone(), session_dir.clone());
                Arc::new(move |on_progress, signal| {
                    let (cwd, dir) = (cwd.clone(), dir.clone());
                    Box::pin(async move { SessionManager::list(&cwd, Some(&dir), on_progress, signal).await })
                })
            };
            let all_loader: crate::modes::interactive::components::session_selector::SessionsLoader = {
                let dir = session_dir.clone();
                Arc::new(move |on_progress, signal| {
                    let dir = dir.clone();
                    Box::pin(async move {
                        if uses_default {
                            SessionManager::list_all(None, on_progress, signal).await
                        } else {
                            SessionManager::list_all(Some(&dir), on_progress, signal).await
                        }
                    })
                })
            };
            let weak = this.weak.clone();
            let done_select = done.clone();
            let weak_exit = this.weak.clone();
            let weak_render = this.weak.clone();
            let selector = Arc::new(SessionSelectorComponent::new(
                &current_loader,
                &all_loader,
                Arc::new(move |session_path: String| {
                    done_select();
                    if let Some(this) = weak.upgrade() {
                        let target = this.clone();
                        this.spawn(async move {
                            target.handle_resume_session(&session_path, None).await;
                        });
                    }
                }),
                this.cancel_handler(&done),
                Arc::new(move || {
                    if let Some(this) = weak_exit.upgrade() {
                        let target = this.clone();
                        this.spawn(async move { target.shutdown(false).await });
                    }
                }),
                Arc::new(move || {
                    if let Some(this) = weak_render.upgrade() {
                        this.request_render();
                    }
                }),
                Some(&SessionSelectorComponentOptions {
                    rename_session: Some(Arc::new(|path: String, next: Option<String>| {
                        Box::pin(async move {
                            let next = next.unwrap_or_default().trim().to_string();
                            if next.is_empty() {
                                return Ok(());
                            }
                            let manager = SessionManager::open(&path, None, None)?;
                            manager.append_session_info(&next)?;
                            Ok(())
                        })
                    })),
                    show_rename_hint: Some(true),
                    keybindings: Some(this.keybindings.clone()),
                }),
                sm.get_session_file().as_deref(),
            ));
            SelectorParts { component: selector.clone(), focus: selector, dispose: None }
        });
    }

    // ------------------------------------------------------------------
    // Login / logout
    // ------------------------------------------------------------------

    pub(crate) fn get_login_provider_options(&self, auth_type: Option<AuthSelectorProviderAuthType>) -> Vec<AuthSelectorProvider> {
        let runtime = self.session().model_runtime();
        let mut options = Vec::new();
        for provider in runtime.get_providers() {
            let id = provider.id().to_string();
            let status = runtime.get_provider_auth_status(&id);
            let status = status.configured.then(|| pi_ai::auth::types::AuthCheck {
                source: status.label.clone().or_else(|| status.source.map(|s| s.as_str().to_string())),
                auth_type: if runtime.is_using_o_auth(&id) { AuthType::OAuth } else { AuthType::ApiKey },
            });
            let auth = provider.auth();
            let subscription = auth.oauth.as_ref().is_some_and(|o| o.is_subscription() == Some(true));
            if auth_type.is_none_or(|t| t == AuthSelectorProviderAuthType::Oauth)
                && let Some(oauth) = &auth.oauth
            {
                options.push(AuthSelectorProvider {
                    id: id.clone(),
                    name: provider.name().to_string(),
                    auth_type: AuthSelectorProviderAuthType::Oauth,
                    method: Some(AuthSelectorProviderMethod::Arc2(oauth.clone())),
                    status: status.clone(),
                    subscription: Some(subscription),
                });
            }
            if auth_type.is_none_or(|t| t == AuthSelectorProviderAuthType::ApiKey)
                && let Some(api_key) = &auth.api_key
            {
                options.push(AuthSelectorProvider {
                    id: id.clone(),
                    name: provider.name().to_string(),
                    auth_type: AuthSelectorProviderAuthType::ApiKey,
                    method: Some(AuthSelectorProviderMethod::Arc(api_key.clone())),
                    status,
                    subscription: Some(subscription),
                });
            }
        }
        options.sort_by(|a, b| super::resources::locale_compare(&a.name, &b.name));
        options
    }

    async fn get_logout_provider_options(&self) -> anyhow::Result<Vec<AuthSelectorProvider>> {
        let runtime = self.session().model_runtime();
        let signal = CancellationToken::new();
        let timer = {
            let signal = signal.clone();
            crate::utils::uncaught::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                signal.cancel();
            })
        };
        let credentials = runtime.list_credentials(Some(&crate::compat::AuthOperationOptions { signal: Some(signal) })).await;
        timer.abort();
        let mut options: Vec<AuthSelectorProvider> = credentials?
            .into_iter()
            .map(|info| {
                let provider = runtime.get_provider(&info.provider_id);
                AuthSelectorProvider {
                    id: info.provider_id.clone(),
                    name: provider.as_ref().map(|p| p.name().to_string()).unwrap_or_else(|| info.provider_id.clone()),
                    auth_type: info.credential_type.into(),
                    method: None,
                    status: Some(pi_ai::auth::types::AuthCheck {
                        source: Some("stored credential".to_string()),
                        auth_type: info.credential_type,
                    }),
                    subscription: Some(
                        provider.as_ref().and_then(|p| p.auth().oauth.as_ref().map(|o| o.is_subscription() == Some(true))).unwrap_or(false),
                    ),
                }
            })
            .collect();
        options.sort_by(|a, b| super::resources::locale_compare(&a.name, &b.name));
        Ok(options)
    }

    fn find_login_provider_options(&self, provider_ref: &str) -> Vec<AuthSelectorProvider> {
        let normalized = provider_ref.trim().to_lowercase();
        if normalized.is_empty() {
            return Vec::new();
        }
        self.get_login_provider_options(None)
            .into_iter()
            .filter(|p| p.id.to_lowercase() == normalized || p.name.to_lowercase() == normalized)
            .collect()
    }

    pub(crate) async fn handle_login_command(self: Arc<Self>, provider_ref: Option<String>) {
        let Some(provider_ref) = provider_ref.filter(|p| !p.is_empty()) else {
            self.show_login_auth_type_selector(None);
            return;
        };
        let options = self.find_login_provider_options(&provider_ref);
        if options.len() == 1 {
            self.start_provider_login(options[0].clone(), None).await;
            return;
        }
        if options.len() > 1 {
            let ids: HashSet<&str> = options.iter().map(|p| p.id.as_str()).collect();
            if ids.len() == 1 {
                self.show_login_auth_type_selector(Some(options));
                return;
            }
        }
        self.show_login_provider_selector(None, Some(provider_ref));
    }

    /// `on_back` reopens the selector the login was started from when the user cancels it.
    async fn start_provider_login(self: Arc<Self>, option: AuthSelectorProvider, on_back: Option<Handler>) {
        match option.auth_type {
            AuthSelectorProviderAuthType::Oauth => self.show_login_dialog(&option.id, &option.name, on_back).await,
            AuthSelectorProviderAuthType::ApiKey => {
                let supports_login = matches!(&option.method, Some(AuthSelectorProviderMethod::Arc(api)) if api.supports_login());
                if supports_login {
                    self.show_api_key_login_dialog(&option.id, &option.name, on_back).await;
                } else {
                    self.show_ambient_auth_dialog(&option, on_back);
                }
            }
        }
    }

    fn spawn_provider_login(&self, option: AuthSelectorProvider, on_back: Option<Handler>) {
        let this = self.this();
        self.spawn(async move { this.start_provider_login(option, on_back).await });
    }

    fn show_login_auth_type_selector(&self, provider_options: Option<Vec<AuthSelectorProvider>>) {
        // The top-level selector offers Radius directly, as its last option.
        let radius_option = if provider_options.is_some() {
            None
        } else {
            self.get_login_provider_options(Some(AuthSelectorProviderAuthType::Oauth))
                .into_iter()
                .find(|p| p.id == RADIUS_PROVIDER_ID)
        };
        let radius_text = radius_option.as_ref().map(|r| format!("Sign in with {}", r.name));
        let radius_label = radius_option
            .as_ref()
            .zip(radius_text.as_ref())
            .map(|(r, text)| format!("{text}{}", format_auth_selector_provider_status(r)));
        let oauth_login_label = provider_options
            .as_ref()
            .and_then(|options| options.iter().find(|p| p.auth_type == AuthSelectorProviderAuthType::Oauth))
            .and_then(|p| match &p.method {
                Some(AuthSelectorProviderMethod::Arc2(oauth)) => oauth.login_label().map(str::to_string),
                _ => None,
            });
        let subscription_label = oauth_login_label.unwrap_or_else(|| "Sign in with an account".to_string());
        let api_key_label = "Sign in with an API key".to_string();
        let available: HashSet<AuthSelectorProviderAuthType> = match &provider_options {
            Some(options) => options.iter().map(|p| p.auth_type).collect(),
            None => [AuthSelectorProviderAuthType::Oauth, AuthSelectorProviderAuthType::ApiKey].into_iter().collect(),
        };
        let mut options = Vec::new();
        if available.contains(&AuthSelectorProviderAuthType::Oauth) {
            options.push(subscription_label.clone());
        }
        if available.contains(&AuthSelectorProviderAuthType::ApiKey) {
            options.push(api_key_label);
        }
        if let Some(label) = &radius_label {
            options.push(label.clone());
        }
        if options.is_empty() {
            self.show_status("No login methods available.");
            return;
        }
        if let Some(provider_options) = &provider_options
            && options.len() == 1
        {
            if let Some(option) = provider_options.first() {
                self.spawn_provider_login(option.clone(), None);
            }
            return;
        }
        let title = match provider_options.as_ref().and_then(|o| o.first()) {
            Some(first) => format!("Select authentication method for {}:", first.name),
            None => "Select authentication method:".to_string(),
        };
        let this = self.this();
        self.show_selector(move |done| {
            let weak = this.weak.clone();
            let done_select = done.clone();
            let radius_label_select = radius_label.clone();
            let provider_options_select = provider_options.clone();
            let on_select = Arc::new(move |option: String| {
                done_select();
                let Some(this) = weak.upgrade() else { return };
                if let (Some(radius), Some(label)) = (&radius_option, &radius_label_select)
                    && &option == label
                {
                    let back_weak = this.weak.clone();
                    this.spawn_provider_login(
                        radius.clone(),
                        Some(Arc::new(move || {
                            if let Some(this) = back_weak.upgrade() {
                                this.show_login_auth_type_selector(None);
                            }
                        })),
                    );
                    return;
                }
                let auth_type = if option == subscription_label {
                    AuthSelectorProviderAuthType::Oauth
                } else {
                    AuthSelectorProviderAuthType::ApiKey
                };
                if let Some(provider_options) = &provider_options_select {
                    if let Some(provider) = provider_options.iter().find(|p| p.auth_type == auth_type) {
                        let back_weak = this.weak.clone();
                        let back_options = provider_options.clone();
                        this.spawn_provider_login(
                            provider.clone(),
                            Some(Arc::new(move || {
                                if let Some(this) = back_weak.upgrade() {
                                    this.show_login_auth_type_selector(Some(back_options.clone()));
                                }
                            })),
                        );
                    }
                    return;
                }
                this.show_login_provider_selector(Some(auth_type), None);
            });
            let on_cancel = this.cancel_handler(&done);
            match (&radius_label, &radius_text) {
                (Some(label), Some(text)) => {
                    let selector = create_login_menu_selector(
                        this.ui(),
                        &title,
                        &options,
                        &RadiusOption { label: label.clone(), text: text.clone() },
                        on_select,
                        on_cancel,
                    );
                    let dispose_target = selector.clone();
                    SelectorParts {
                        component: selector.clone(),
                        focus: selector,
                        dispose: Some(Arc::new(move || dispose_target.dispose())),
                    }
                }
                _ => {
                    let selector = Arc::new(ExtensionSelectorComponent::new(&title, &options, on_select, on_cancel, None));
                    let dispose_target = selector.clone();
                    SelectorParts {
                        component: selector.clone(),
                        focus: selector,
                        dispose: Some(Arc::new(move || dispose_target.dispose())),
                    }
                }
            }
        });
    }

    fn show_login_provider_selector(&self, auth_type: Option<AuthSelectorProviderAuthType>, initial_search_input: Option<String>) {
        let provider_options = self.get_login_provider_options(auth_type);
        if provider_options.is_empty() {
            self.show_status(match auth_type {
                Some(AuthSelectorProviderAuthType::Oauth) => "No account providers available.",
                Some(AuthSelectorProviderAuthType::ApiKey) => "No API key providers available.",
                None => "No login providers available.",
            });
            return;
        }
        let this = self.this();
        self.show_selector(move |done| {
            let weak = this.weak.clone();
            let done_select = done.clone();
            let options_select = provider_options.clone();
            let initial_select = initial_search_input.clone();
            let weak_cancel = this.weak.clone();
            let done_cancel = done.clone();
            let selector = Arc::new(OAuthSelectorComponent::new(
                OAuthSelectorComponentMode::Login,
                &provider_options,
                Arc::new(move |provider_id: String, selected: AuthSelectorProviderAuthType| {
                    done_select();
                    let Some(this) = weak.upgrade() else { return };
                    let Some(option) = options_select.iter().find(|p| p.id == provider_id && p.auth_type == selected).cloned()
                    else {
                        return;
                    };
                    let back_weak = this.weak.clone();
                    let back_initial = initial_select.clone();
                    this.spawn_provider_login(
                        option,
                        Some(Arc::new(move || {
                            if let Some(this) = back_weak.upgrade() {
                                this.show_login_provider_selector(auth_type, back_initial.clone());
                            }
                        })),
                    );
                }),
                Arc::new(move || {
                    done_cancel();
                    if let Some(this) = weak_cancel.upgrade() {
                        if auth_type.is_some() {
                            this.show_login_auth_type_selector(None);
                        } else {
                            this.request_render();
                        }
                    }
                }),
                initial_search_input.as_deref(),
            ));
            SelectorParts { component: selector.clone(), focus: selector, dispose: None }
        });
    }

    pub(crate) async fn show_oauth_selector_logout(self: Arc<Self>) {
        let provider_options = match self.get_logout_provider_options().await {
            Ok(options) => options,
            Err(error) => {
                self.show_error(&format!("Could not read stored credentials: {error}"));
                return;
            }
        };
        if provider_options.is_empty() {
            self.show_status(
                "No stored credentials to remove. /logout only removes credentials saved by /login; environment variables and models.json config are unchanged.",
            );
            return;
        }
        let this = self.clone();
        self.show_selector(move |done| {
            let weak = this.weak.clone();
            let done_select = done.clone();
            let options_select = provider_options.clone();
            let selector = Arc::new(OAuthSelectorComponent::new(
                OAuthSelectorComponentMode::Logout,
                &provider_options,
                Arc::new(move |provider_id: String, _auth_type| {
                    done_select();
                    let Some(this) = weak.upgrade() else { return };
                    let Some(option) = options_select.iter().find(|p| p.id == provider_id).cloned() else { return };
                    let target = this.clone();
                    this.spawn(async move {
                        let signal = CancellationToken::new();
                        let timer = {
                            let signal = signal.clone();
                            crate::utils::uncaught::spawn(async move {
                                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                                signal.cancel();
                            })
                        };
                        let result = target
                            .session()
                            .model_runtime()
                            .logout(&option.id, Some(&crate::compat::AuthOperationOptions { signal: Some(signal) }))
                            .await;
                        timer.abort();
                        match result {
                            Ok(()) => {
                                target.update_available_provider_count();
                                target.show_status(&if option.auth_type == AuthSelectorProviderAuthType::Oauth {
                                    format!("Logged out of {}", option.name)
                                } else {
                                    format!(
                                        "Removed stored API key for {}. Environment variables and models.json config are unchanged.",
                                        option.name
                                    )
                                });
                            }
                            Err(error) => {
                                let message = error.to_string();
                                target.show_error(
                                    &if error.downcast_ref::<crate::core::model_runtime::CredentialSynchronizationError>().is_some() {
                                        format!(
                                            "Credentials removed for {}, but local model state could not be synchronized: {message}",
                                            option.name
                                        )
                                    } else {
                                        format!("Logout failed: {message}")
                                    },
                                );
                            }
                        }
                    });
                }),
                this.cancel_handler(&done),
                None,
            ));
            SelectorParts { component: selector.clone(), focus: selector, dispose: None }
        });
    }

    async fn complete_provider_authentication(
        self: Arc<Self>,
        provider_id: &str,
        provider_name: &str,
        auth_type: AuthSelectorProviderAuthType,
        previous_model: Option<Model>,
    ) {
        let action_label = match auth_type {
            AuthSelectorProviderAuthType::Oauth => format!("Logged in to {provider_name}"),
            AuthSelectorProviderAuthType::ApiKey => format!("Saved API key for {provider_name}"),
        };
        let session = self.session();
        let default_model_id = DEFAULT_MODEL_PER_PROVIDER.get(provider_id).cloned();
        // Dynamic catalogs may be empty until the first authenticated network refresh.
        let defer_selection = is_unknown_model(previous_model.as_ref())
            && default_model_id.as_ref().is_some_and(|default_id| {
                !session
                    .model_runtime()
                    .get_available_snapshot()
                    .iter()
                    .any(|m| m.provider == provider_id && &m.id == default_id)
            });

        let finish = {
            let this = self.clone();
            let provider_id = provider_id.to_string();
            let action_label = action_label.clone();
            let previous_model = previous_model.clone();
            move || {
                let this = this.clone();
                let provider_id = provider_id.clone();
                let action_label = action_label.clone();
                let previous_model = previous_model.clone();
                async move {
                    let mut selected: Option<Model> = None;
                    let mut selection_error: Option<String> = None;
                    if is_unknown_model(previous_model.as_ref()) {
                        let provider_models: Vec<Model> = this
                            .session()
                            .model_runtime()
                            .get_available_snapshot()
                            .into_iter()
                            .filter(|m| m.provider == provider_id)
                            .collect();
                        if provider_id == "llama.cpp" {
                            selection_error = Some(llama_cpp_post_login_guidance(&action_label, provider_models.len()));
                        } else if let Some(default_id) = DEFAULT_MODEL_PER_PROVIDER.get(&provider_id) {
                            if provider_models.is_empty() {
                                selection_error = Some(format!(
                                    "{action_label}, but no models are available for that provider. Use /model to select a model."
                                ));
                            } else {
                                // Radius catalogs vary by account; prefer balanced, then use catalog order.
                                let model = provider_models
                                    .iter()
                                    .find(|m| &m.id == default_id)
                                    .cloned()
                                    .or_else(|| (provider_id == "radius").then(|| provider_models[0].clone()));
                                match model {
                                    None => {
                                        selection_error = Some(format!(
                                            "{action_label}, but its default model \"{default_id}\" is not available. Use /model to select a model."
                                        ))
                                    }
                                    Some(model) => {
                                        match this
                                            .session()
                                            .set_model(
                                                model.clone(),
                                                Some(&crate::core::agent_session::ModelMutationOptions { persist: Some(true) }),
                                            )
                                            .await
                                        {
                                            Ok(()) => selected = Some(model),
                                            Err(error) => {
                                                selection_error = Some(format!(
                                                    "{action_label}, but selecting its default model failed: {error}. Use /model to select a model."
                                                ))
                                            }
                                        }
                                    }
                                }
                            }
                        } else {
                            selection_error = Some(format!(
                                "{action_label}, but no default model is configured for provider \"{provider_id}\". Use /model to select a model."
                            ));
                        }
                    }
                    this.update_available_provider_count();
                    this.footer.invalidate();
                    this.update_editor_border_color();
                    match selected {
                        Some(model) => {
                            this.show_status(&format!(
                                "{action_label}. Selected {}. Credentials saved to {}",
                                model.id,
                                get_auth_path()
                            ));
                            this.maybe_warn_about_anthropic_subscription_auth(Some(model)).await;
                        }
                        None => {
                            this.show_status(&format!("{action_label}. Credentials saved to {}", get_auth_path()));
                            match selection_error {
                                Some(error) => this.show_error(&error),
                                None => this.maybe_warn_about_anthropic_subscription_auth(None).await,
                            }
                        }
                    }
                }
            }
        };

        if defer_selection {
            self.show_status(&format!(
                "{action_label}. Credentials saved to {}. Refreshing model catalog…",
                get_auth_path()
            ));
        } else {
            finish().await;
        }

        let this = self.clone();
        let provider_id = provider_id.to_string();
        self.spawn(async move {
            let signal = CancellationToken::new();
            let timer = {
                let signal = signal.clone();
                crate::utils::uncaught::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                    signal.cancel();
                })
            };
            let result = session
                .model_runtime()
                .refresh(Some(pi_ai::models::ModelsRefreshOptions {
                    providers: Some(vec![provider_id.clone()]),
                    signal: Some(signal),
                    ..Default::default()
                }))
                .await;
            timer.abort();
            match result {
                Ok(result) => {
                    if result.aborted {
                        this.show_warning(&format!(
                            "{action_label}, but its model catalog refresh timed out; using cached models."
                        ));
                    } else if !result.errors.is_empty() {
                        this.show_warning(&format!(
                            "{action_label}, but its model catalog could not be refreshed; using cached models."
                        ));
                    }
                    // Do not replace a model or session selected while the refresh was running.
                    if defer_selection && Arc::ptr_eq(&this.session(), &session) && session.model() == previous_model {
                        finish().await;
                    }
                    this.update_available_provider_count();
                    this.footer.invalidate();
                    this.request_render();
                }
                Err(error) => this.show_warning(&format!(
                    "{action_label}, but its model catalog could not be refreshed: {error}"
                )),
            }
        });
    }

    fn restore_editor_from_dialog(&self) {
        let editor = self.editor_ref();
        self.editor_container.clear();
        self.editor_container.add_child(editor.clone());
        let ui = self.ui();
        ui.set_focus(Some(editor));
        ui.request_render(false);
    }

    fn show_dialog(&self, dialog: ComponentRef) {
        self.editor_container.clear();
        self.editor_container.add_child(dialog.clone());
        let ui = self.ui();
        ui.set_focus(Some(dialog));
        ui.request_render(false);
    }

    fn show_ambient_auth_dialog(&self, option: &AuthSelectorProvider, on_back: Option<Handler>) {
        let weak = self.weak.clone();
        let dialog = Arc::new(LoginDialogComponent::new(
            self.ui(),
            &option.id,
            Arc::new(move |_success, _message| {
                if let Some(this) = weak.upgrade() {
                    this.restore_editor_from_dialog();
                }
                if let Some(back) = &on_back {
                    back();
                }
            }),
            Some(&option.name),
            Some(&format!("{} setup", option.name)),
        ));
        let method_name = match &option.method {
            Some(AuthSelectorProviderMethod::Arc(api)) => api.name().to_string(),
            Some(AuthSelectorProviderMethod::Arc2(oauth)) => oauth.name().to_string(),
            None => "Authentication".to_string(),
        };
        dialog.show_info(&format!("{method_name} is configured outside {APP_NAME}."), Some(&[]), Some(true));
        self.show_dialog(dialog);
    }

    async fn show_api_key_login_dialog(self: Arc<Self>, provider_id: &str, provider_name: &str, on_back: Option<Handler>) {
        let previous_model = self.session().model();
        let dialog = Arc::new(LoginDialogComponent::new(self.ui(), provider_id, Arc::new(|_, _| {}), Some(provider_name), None));
        if provider_id == "amazon-bedrock" {
            let t = theme();
            dialog.show_details(&[
                t.fg(&ThemeColor::Text, "You can also use an AWS profile, IAM keys, or role-based credentials."),
                t.fg(&ThemeColor::Muted, "See:"),
                t.fg(
                    &ThemeColor::Accent,
                    &format!("  {}", std::path::Path::new(&crate::config::get_docs_path()).join("providers.md").display()),
                ),
            ]);
        }
        self.show_dialog(dialog.clone());
        match self.login_provider(&dialog, provider_id, provider_name, AuthType::ApiKey).await {
            Ok(()) => {
                self.restore_editor_from_dialog();
                self.clone()
                    .complete_provider_authentication(provider_id, provider_name, AuthSelectorProviderAuthType::ApiKey, previous_model)
                    .await;
            }
            Err(error) => {
                self.restore_editor_from_dialog();
                let message = error.to_string();
                if error.downcast_ref::<crate::core::model_runtime::CredentialSynchronizationError>().is_some() {
                    self.show_error(&format!(
                        "Saved API key for {provider_name}, but local model state could not be synchronized: {message}"
                    ));
                } else if message == "Login cancelled" {
                    if let Some(back) = on_back {
                        back();
                    }
                } else {
                    self.show_error(&format!("Failed to save API key for {provider_name}: {message}"));
                }
            }
        }
    }

    async fn show_auth_select(&self, dialog: &Arc<LoginDialogComponent>, prompt: &AuthPrompt, provider_id: &str) -> anyhow::Result<String> {
        let (tx, rx) = tokio::sync::oneshot::channel::<Option<String>>();
        let tx = Arc::new(Mutex::new(Some(tx)));
        let restore = {
            let weak = self.weak.clone();
            let dialog = dialog.clone();
            move || {
                if let Some(this) = weak.upgrade() {
                    this.show_dialog(dialog.clone());
                }
            }
        };
        let restore = Arc::new(restore);
        let labels: Vec<String> = prompt.options.iter().map(|o| o.label.clone()).collect();
        let options = prompt.options.clone();
        let (r1, t1) = (restore.clone(), tx.clone());
        let (r2, t2) = (restore, tx);
        let selector = Arc::new(ExtensionSelectorComponent::new(
            &prompt.message,
            &labels,
            Arc::new(move |label: String| {
                r1();
                let id = options.iter().find(|o| o.label == label).map(|o| o.id.clone());
                if let Some(tx) = t1.lock().take() {
                    let _ = tx.send(id);
                }
            }),
            Arc::new(move || {
                r2();
                if let Some(tx) = t2.lock().take() {
                    let _ = tx.send(None);
                }
            }),
            Some(&ExtensionSelectorOptions {
                tui: None,
                timeout: None,
                on_toggle_tools_expanded: None,
                description: (provider_id == RADIUS_PROVIDER_ID).then(|| RADIUS_LOGIN_INTRO.to_string()),
            }),
        ));
        self.show_dialog(selector);
        match rx.await.ok().flatten() {
            Some(id) => Ok(id),
            None => Err(anyhow::anyhow!("Login cancelled")),
        }
    }

    async fn login_provider(
        &self,
        dialog: &Arc<LoginDialogComponent>,
        provider_id: &str,
        provider_name: &str,
        method: AuthType,
    ) -> anyhow::Result<()> {
        self.program_status.set_blocked(
            "login",
            Some(&BlockedStatus { kind: BlockedStatusKind::Auth, message: format!("Log in to {provider_name}") }),
        );
        let interaction = Arc::new(DialogAuthInteraction {
            mode: self.weak.clone(),
            dialog: dialog.clone(),
            provider_id: provider_id.to_string(),
        });
        let settings = self.settings_manager();
        let result = self
            .session()
            .model_runtime()
            .login(
                provider_id,
                method,
                interaction,
                Some(dialog.signal()),
                Some(pi_ai::auth::types::LoginOptions {
                    get_device_id: Some(Arc::new(move || settings.get_or_create_device_id())),
                    agent_name: None,
                }),
            )
            .await;
        self.program_status.set_blocked("login", None);
        result.map(|_| ())
    }

    async fn show_login_dialog(self: Arc<Self>, provider_id: &str, provider_name: &str, on_back: Option<Handler>) {
        let previous_model = self.session().model();
        let dialog = Arc::new(LoginDialogComponent::new(self.ui(), provider_id, Arc::new(|_, _| {}), Some(provider_name), None));
        self.show_dialog(dialog.clone());
        match self.login_provider(&dialog, provider_id, provider_name, AuthType::OAuth).await {
            Ok(()) => {
                self.restore_editor_from_dialog();
                self.clone()
                    .complete_provider_authentication(provider_id, provider_name, AuthSelectorProviderAuthType::Oauth, previous_model)
                    .await;
                if provider_id == RADIUS_PROVIDER_ID {
                    self.offer_radius_mcp_server(provider_id, provider_name);
                }
            }
            Err(error) => {
                self.restore_editor_from_dialog();
                let message = error.to_string();
                if error.downcast_ref::<crate::core::model_runtime::CredentialSynchronizationError>().is_some() {
                    self.show_error(&format!(
                        "Logged in to {provider_name}, but local model state could not be synchronized: {message}"
                    ));
                } else if message == "Login cancelled" {
                    if let Some(back) = on_back {
                        back();
                    }
                } else {
                    self.show_error(&format!("Failed to login to {provider_name}: {message}"));
                }
            }
        }
    }

    /// Offer to point the Radius MCP server in the global mcp.json at the Radius login.
    fn offer_radius_mcp_server(&self, provider_id: &str, provider_name: &str) {
        use crate::extensions::mcp::config::{LoadMcpConfigOptions, add_mcp_server_config_value, load_mcp_config};
        let agent_dir = get_agent_dir();
        let mcp_path = std::path::Path::new(&agent_dir).join("mcp.json").to_string_lossy().into_owned();
        let normalize = |url: &str| url.trim_end_matches('/').to_string();
        let radius_url = crate::core::radius::RADIUS_MCP_URL.clone();
        let loaded = load_mcp_config(&LoadMcpConfigOptions {
            agent_dir: agent_dir.clone(),
            cwd: self.session_manager().get_cwd(),
            project_trusted: false,
        });
        let servers: Vec<(String, Value)> =
            loaded.servers.iter().map(|s| (s.name.clone(), serde_json::to_value(&s.config).unwrap_or_default())).collect();
        let existing = servers.iter().find(|(_, config)| {
            config.get("url").and_then(Value::as_str).is_some_and(|url| normalize(url) == normalize(&radius_url))
        });
        if let Some((_, config)) = existing
            && config.pointer("/auth/provider").and_then(Value::as_str) == Some(provider_id)
        {
            return;
        }
        let mut name = existing.map(|(n, _)| n.clone()).unwrap_or_else(|| "radius".to_string());
        if existing.is_none() && servers.iter().any(|(n, _)| *n == name) {
            name = "radius-mcp".to_string();
        }
        let mut config = match existing {
            Some((_, config)) => config.clone(),
            None => serde_json::json!({ "url": radius_url }),
        };
        config["auth"] = serde_json::json!({ "provider": provider_id });
        // `auth` replaces the MCP OAuth sign-in.
        if let Value::Object(map) = &mut config {
            map.shift_remove("oauth");
        }
        let title = format!("Configure {provider_name} MCP in {mcp_path}?");
        let this = self.this();
        self.show_selector(move |done| {
            let weak = this.weak.clone();
            let done_select = done.clone();
            let selector = Arc::new(ExtensionSelectorComponent::new(
                &title,
                &["Yes".to_string(), "No".to_string()],
                Arc::new(move |option: String| {
                    done_select();
                    if option != "Yes" {
                        return;
                    }
                    let Some(this) = weak.upgrade() else { return };
                    if let Err(error) = add_mcp_server_config_value(&mcp_path, &name, config.clone()) {
                        this.show_error(&format!("Could not update {mcp_path}: {error}"));
                        return;
                    }
                    // The MCP extension reads mcp.json when the session starts.
                    let target = this.clone();
                    this.spawn(async move { target.handle_reload_command().await });
                }),
                this.cancel_handler(&done),
                None,
            ));
            SelectorParts { component: selector.clone(), focus: selector, dispose: None }
        });
    }
}
