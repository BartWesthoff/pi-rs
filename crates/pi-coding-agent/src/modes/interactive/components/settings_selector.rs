//! Port of `packages/coding-agent/src/modes/interactive/components/settings-selector.ts`.

use crate::compat::WheelScrollLines;
use crate::core::settings_schema::CacheWarmingMode;
use crate::core::settings_schema::DefaultProjectTrust;
use crate::core::settings_schema::FullscreenExitOutput;
use crate::core::settings_schema::MermaidRenderingMode;
use crate::core::settings_schema::QuietStartup;
use crate::core::settings_schema::SessionShareDestination;
use crate::core::settings_schema::TuiMode;
use crate::core::settings_schema::WarningSettings;
use crate::modes::interactive::components::tree_selector::FilterMode;
use crate::core::http_dispatcher::{HTTP_IDLE_TIMEOUT_CHOICES, format_http_idle_timeout_ms};
use crate::core::settings_defaults;
use crate::core::settings_manager::CACHE_WARMING_MODES;
use crate::core::settings_schema::QuietStartupValue;
use crate::core::tools::js_util::locale_compare;
use crate::modes::interactive::components::dynamic_border::DynamicBorder;
use crate::modes::interactive::components::keybinding_hints::key_display_text;
use crate::modes::interactive::components::settings_submenu::{
    SelectSubmenu, SteppedSubmenu, SteppedSubmenuOptions, SteppedSubmenuStep, SteppedSubmenuStepDescription,
    SteppedSubmenuStepTitle,
};
use crate::modes::interactive::theme::theme::{
    SYSTEM_THEME_NAME, TerminalTheme, ThemeColor, get_settings_list_theme, parse_auto_theme_setting, theme,
};
use indexmap::IndexMap;
use parking_lot::Mutex;
use pi_agent::types::ThinkingLevel;
use pi_ai::models::get_supported_thinking_levels;
use pi_ai::types::Model;
use pi_ai::types::Transport;
use pi_tui::components::scroll_view::ScrollViewScrollbar;
use pi_tui::components::select_list::{SelectItem, SelectListLayoutOptions};
use pi_tui::components::settings_list::{
    SettingItem, SettingsList, SettingsListOptions, SubmenuDone, SubmenuFactory,
};
use pi_tui::components::spacer::Spacer;
use pi_tui::components::text::Text;
use pi_tui::terminal_image::get_capabilities;
use pi_tui::tui::{Component, ComponentRef, Container, MouseEvent, MouseResult};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Weak};

/// `"all" | "one-at-a-time"`; TS shows whatever the session holds, unrecognized values included.
pub type SettingsConfigSteeringMode = pi_agent::types::QueueMode;

/// `"fork" | "tree" | "none"`; TS shows whatever the settings hold, unrecognized values included.
pub type SettingsConfigDoubleEscapeAction = crate::core::settings_manager::SettingsManagerGetDoubleEscapeActionResult;

#[derive(Debug, Clone)]
pub struct SettingsConfig {
    pub auto_compact: bool,
    pub default_model: String,
    pub current_model: Option<Model>,
    pub available_default_models: Vec<Model>,
    pub show_images: bool,
    pub image_width_cells: i64,
    pub auto_resize_images: bool,
    pub block_images: bool,
    pub enable_skill_commands: bool,
    pub steering_mode: SettingsConfigSteeringMode,
    pub follow_up_mode: SettingsConfigSteeringMode,
    pub transport: crate::core::settings_manager::TransportSetting,
    pub session_share_destination: SessionShareDestination,
    pub http_idle_timeout_ms: i64,
    pub cache_warming_mode: CacheWarmingMode,
    pub thinking_level: crate::core::settings_manager::ThinkingLevelSetting,
    pub available_thinking_levels: Vec<ThinkingLevel>,
    pub model_thinking_levels: IndexMap<String, crate::core::settings_manager::ThinkingLevelSetting>,
    pub current_theme: String,
    pub terminal_theme: TerminalTheme,
    pub available_themes: Vec<String>,
    pub hide_thinking_block: bool,
    pub mermaid_rendering_mode: MermaidRenderingMode,
    pub show_cache_miss_notices: bool,
    pub collapse_changelog: bool,
    pub enable_install_telemetry: bool,
    pub double_escape_action: SettingsConfigDoubleEscapeAction,
    pub tree_filter_mode: FilterMode,
    pub show_hardware_cursor: bool,
    pub editor_padding_x: i64,
    pub output_pad: i64,
    pub autocomplete_max_visible: i64,
    pub quiet_startup: QuietStartup,
    pub default_project_trust: DefaultProjectTrust,
    pub clear_on_shrink: bool,
    pub show_terminal_progress: bool,
    pub tui_mode: TuiMode,
    pub fullscreen_exit_output: FullscreenExitOutput,
    pub fullscreen_scrollbar: ScrollViewScrollbar,
    pub fullscreen_copy_on_select: bool,
    pub fullscreen_wheel_scroll_lines: WheelScrollLines,
    pub warnings: WarningSettings,
}

#[derive(Clone)]
pub struct SettingsCallbacks {
    pub on_auto_compact_change: Arc<dyn Fn(bool) + Send + Sync>,
    pub on_show_images_change: Arc<dyn Fn(bool) + Send + Sync>,
    pub on_image_width_cells_change: Arc<dyn Fn(i64) + Send + Sync>,
    pub on_auto_resize_images_change: Arc<dyn Fn(bool) + Send + Sync>,
    pub on_block_images_change: Arc<dyn Fn(bool) + Send + Sync>,
    pub on_enable_skill_commands_change: Arc<dyn Fn(bool) + Send + Sync>,
    pub on_steering_mode_change: Arc<dyn Fn(SettingsConfigSteeringMode) + Send + Sync>,
    pub on_follow_up_mode_change: Arc<dyn Fn(SettingsConfigSteeringMode) + Send + Sync>,
    pub on_transport_change: Arc<dyn Fn(Transport) + Send + Sync>,
    pub on_session_share_destination_change: Arc<dyn Fn(SessionShareDestination) + Send + Sync>,
    pub on_http_idle_timeout_ms_change: Arc<dyn Fn(i64) + Send + Sync>,
    pub on_cache_warming_mode_change: Arc<dyn Fn(CacheWarmingMode) + Send + Sync>,
    pub on_model_thinking_level_change: Arc<dyn Fn(String, String, ThinkingLevel) + Send + Sync>,
    pub on_model_thinking_level_remove: Arc<dyn Fn(String, String) + Send + Sync>,
    pub on_theme_change: Arc<dyn Fn(String) + Send + Sync>,
    pub on_theme_preview: Option<Arc<dyn Fn(String) + Send + Sync>>,
    pub on_hide_thinking_block_change: Arc<dyn Fn(bool) + Send + Sync>,
    pub on_mermaid_rendering_mode_change: Arc<dyn Fn(MermaidRenderingMode) + Send + Sync>,
    pub on_show_cache_miss_notices_change: Arc<dyn Fn(bool) + Send + Sync>,
    pub on_collapse_changelog_change: Arc<dyn Fn(bool) + Send + Sync>,
    pub on_enable_install_telemetry_change: Arc<dyn Fn(bool) + Send + Sync>,
    pub on_double_escape_action_change: Arc<dyn Fn(SettingsConfigDoubleEscapeAction) + Send + Sync>,
    pub on_tree_filter_mode_change: Arc<dyn Fn(FilterMode) + Send + Sync>,
    pub on_show_hardware_cursor_change: Arc<dyn Fn(bool) + Send + Sync>,
    pub on_editor_padding_x_change: Arc<dyn Fn(i64) + Send + Sync>,
    pub on_output_pad_change: Arc<dyn Fn(i64) + Send + Sync>,
    pub on_autocomplete_max_visible_change: Arc<dyn Fn(i64) + Send + Sync>,
    pub on_quiet_startup_change: Arc<dyn Fn(QuietStartup) + Send + Sync>,
    pub on_default_project_trust_change: Arc<dyn Fn(DefaultProjectTrust) + Send + Sync>,
    pub on_clear_on_shrink_change: Arc<dyn Fn(bool) + Send + Sync>,
    pub on_show_terminal_progress_change: Arc<dyn Fn(bool) + Send + Sync>,
    pub on_tui_mode_change: Arc<dyn Fn(TuiMode) + Send + Sync>,
    pub on_fullscreen_exit_output_change: Arc<dyn Fn(FullscreenExitOutput) + Send + Sync>,
    pub on_fullscreen_scrollbar_change: Arc<dyn Fn(ScrollViewScrollbar) + Send + Sync>,
    pub on_fullscreen_copy_on_select_change: Arc<dyn Fn(bool) + Send + Sync>,
    pub on_fullscreen_wheel_scroll_lines_change: Arc<dyn Fn(WheelScrollLines) + Send + Sync>,
    pub on_warnings_change: Arc<dyn Fn(WarningSettings) + Send + Sync>,
    pub on_cancel: Arc<dyn Fn() + Send + Sync>,
}


fn model_picker_layout() -> SelectListLayoutOptions {
    SelectListLayoutOptions {
        min_primary_column_width: Some(12),
        max_primary_column_width: Some(46),
        truncate_primary: None,
    }
}

fn thinking_description(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Off => "No reasoning",
        ThinkingLevel::Minimal => "Very brief reasoning (~1k tokens)",
        ThinkingLevel::Low => "Light reasoning (~2k tokens)",
        ThinkingLevel::Medium => "Moderate reasoning (~8k tokens)",
        ThinkingLevel::High => "Deep reasoning (~16k tokens)",
        ThinkingLevel::Xhigh => "Extra-high reasoning (~32k tokens)",
        ThinkingLevel::Max => "Maximum reasoning",
    }
}

const DEFAULT_PROJECT_TRUST_LABELS: [(DefaultProjectTrust, &str); 3] = [
    (DefaultProjectTrust::Ask, "Ask"),
    (DefaultProjectTrust::Always, "Always trust"),
    (DefaultProjectTrust::Never, "Never trust"),
];

fn default_project_trust_label(value: DefaultProjectTrust) -> &'static str {
    DEFAULT_PROJECT_TRUST_LABELS.iter().find(|(v, _)| *v == value).map(|(_, l)| *l).unwrap_or("Ask")
}

/// Parse a string-valued enum through its serde representation (TS: `newValue as T`).
fn parse_enum<T: DeserializeOwned>(value: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(value.to_string())).ok()
}

fn enum_str<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

fn scrollbar_str(value: ScrollViewScrollbar) -> &'static str {
    match value {
        ScrollViewScrollbar::Hidden => "hidden",
        ScrollViewScrollbar::Auto => "auto",
        ScrollViewScrollbar::Always => "always",
    }
}

fn parse_scrollbar(value: &str) -> Option<ScrollViewScrollbar> {
    match value {
        "hidden" => Some(ScrollViewScrollbar::Hidden),
        "auto" => Some(ScrollViewScrollbar::Auto),
        "always" => Some(ScrollViewScrollbar::Always),
        _ => None,
    }
}

fn quiet_startup_str(value: &QuietStartup) -> String {
    match value {
        QuietStartupValue::Bool(b) => b.to_string(),
        QuietStartupValue::Header(h) => h.clone(),
    }
}

fn wheel_scroll_lines_str(value: &WheelScrollLines) -> String {
    match value {
        WheelScrollLines::Lines(n) => n.to_string(),
        WheelScrollLines::Auto(s) => s.clone(),
    }
}

fn values(list: &[&str]) -> Option<Vec<String>> {
    Some(list.iter().map(|s| s.to_string()).collect())
}

fn bool_str(value: bool) -> String {
    if value { "true" } else { "false" }.to_string()
}

fn setting(id: &str, label: &str, description: &str, current_value: String, vals: &[&str]) -> SettingItem {
    SettingItem {
        id: id.to_string(),
        label: label.to_string(),
        description: Some(description.to_string()),
        current_value,
        values: values(vals),
        submenu: None,
    }
}

/// JS `parseInt(value, 10)` for the numeric choices used here.
fn parse_int(value: &str) -> i64 {
    let trimmed = value.trim_start();
    let mut end = 0;
    for (i, c) in trimmed.char_indices() {
        if c.is_ascii_digit() || (i == 0 && (c == '-' || c == '+')) {
            end = i + c.len_utf8();
        } else {
            break;
        }
    }
    trimmed[..end].parse().unwrap_or(0)
}

/// A submenu component for selecting from a list of options.
/// TS: `extends Container`
pub struct WarningSettingsSubmenu {
    container: Container,
    settings_list: Arc<SettingsList>,
}

impl WarningSettingsSubmenu {
    pub fn new(
        warnings: &WarningSettings,
        on_change: Arc<dyn Fn(WarningSettings) + Send + Sync>,
        on_cancel: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let state = Arc::new(Mutex::new(warnings.clone()));
        let current = state
            .lock()
            .anthropic_extra_usage
            .unwrap_or(settings_defaults::warnings::ANTHROPIC_EXTRA_USAGE);
        let items = vec![setting(
            "anthropic-extra-usage",
            "Anthropic extra usage",
            "Warn when Anthropic subscription auth may use paid extra usage",
            bool_str(current),
            &["true", "false"],
        )];
        let max_visible = items.len().min(10);
        let settings_list = SettingsList::new(
            items,
            max_visible,
            (*get_settings_list_theme()).clone(),
            Arc::new(move |id: String, new_value: String| {
                if id == "anthropic-extra-usage" {
                    let next = {
                        let mut s = state.lock();
                        s.anthropic_extra_usage = Some(new_value == "true");
                        s.clone()
                    };
                    on_change(next);
                }
            }),
            on_cancel,
            SettingsListOptions::default(),
        );
        let container = Container::new();
        container.add_child(settings_list.clone());
        Self { container, settings_list }
    }
}

impl Component for WarningSettingsSubmenu {
    fn render(&self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    fn handle_input(&self, data: &str) {
        self.settings_list.handle_input(data);
    }

    fn handles_input(&self) -> bool {
        true
    }

    fn invalidate(&self) {
        self.container.invalidate();
    }

    fn handle_mouse(&self, event: &MouseEvent) -> Option<MouseResult> {
        self.container.handle_mouse(event)
    }

    fn child_components(&self) -> Option<Vec<ComponentRef>> {
        self.container.child_components()
    }
}

const CLEAR_OVERRIDE_VALUE: &str = "__clear__";

fn model_setting_key(model: &Model) -> String {
    format!("{}/{}", model.provider, model.id)
}

fn model_display_label(model: &Model) -> String {
    format!("{} [{}]", model.id, model.provider)
}

fn model_thinking_overrides_summary(
    overrides: &IndexMap<String, crate::core::settings_manager::ThinkingLevelSetting>,
) -> String {
    if overrides.is_empty() { "none".to_string() } else { format!("{} configured", overrides.len()) }
}

fn model_item_label(model: &Model) -> String {
    format!("{} {}", model.id, theme().fg(&ThemeColor::Muted, &format!("[{}]", model.provider)))
}

fn theme_items(available_themes: &[String], current_theme: &str) -> Vec<SelectItem> {
    available_themes
        .iter()
        .map(|name| SelectItem {
            value: name.clone(),
            label: format!("{}{name}", if name == current_theme { "✓ " } else { "  " }),
            description: if name == SYSTEM_THEME_NAME {
                Some("Theme created from your terminal's colors".to_string())
            } else {
                None
            },
        })
        .collect()
}

const AUTOMATIC_THEME_VALUE: &str = "/";

/// The system theme comes first, then automatic mode, then the remaining themes.
fn single_mode_theme_items(available_themes: &[String], current_theme: &str) -> Vec<SelectItem> {
    let mut items = theme_items(available_themes, current_theme);
    let system: Vec<SelectItem> = match items.iter().position(|item| item.value == SYSTEM_THEME_NAME) {
        Some(index) => vec![items.remove(index)],
        None => Vec::new(),
    };
    let mut result = system;
    result.push(SelectItem {
        value: AUTOMATIC_THEME_VALUE.to_string(),
        label: "  automatic".to_string(),
        description: Some("Use separate themes for light and dark terminal appearance".to_string()),
    });
    result.extend(items);
    result
}

fn preferred_theme(available_themes: &[String], preferred: Option<&str>, fallback: &str) -> String {
    if let Some(preferred) = preferred.filter(|p| !p.is_empty())
        && available_themes.iter().any(|t| t == preferred)
    {
        return preferred.to_string();
    }
    if available_themes.iter().any(|t| t == fallback) {
        return fallback.to_string();
    }
    available_themes.first().cloned().unwrap_or_else(|| fallback.to_string())
}

fn default_automatic_themes(current_theme_setting: &str, available_themes: &[String]) -> (String, String) {
    if let Some(auto) = parse_auto_theme_setting(Some(current_theme_setting)) {
        return (auto.light_theme, auto.dark_theme);
    }
    let current_fixed_theme = if current_theme_setting.contains('/') { None } else { Some(current_theme_setting) };
    let theme_name = preferred_theme(available_themes, current_fixed_theme, SYSTEM_THEME_NAME);
    (theme_name.clone(), theme_name)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ThemeMode {
    Single,
    Automatic,
}

struct ThemeSubmenuState {
    input_component: Option<ComponentRef>,
    mode: ThemeMode,
    single_theme: String,
    light_theme: String,
    dark_theme: String,
}

struct ThemeSubmenuInner {
    container: Container,
    on_theme_preview: Option<Arc<dyn Fn(String) + Send + Sync>>,
    available_themes: Vec<String>,
    terminal_theme: TerminalTheme,
    on_done: Arc<dyn Fn(Option<String>) + Send + Sync>,
    original_theme_setting: String,
    state: Mutex<ThemeSubmenuState>,
    weak_self: Weak<ThemeSubmenuInner>,
}

impl ThemeSubmenuInner {
    fn preview(&self, value: String) {
        if let Some(cb) = &self.on_theme_preview {
            cb(value);
        }
    }

    fn set_content(&self, render_component: ComponentRef, input_component: Option<ComponentRef>) {
        self.container.clear();
        self.container.add_child(render_component.clone());
        self.state.lock().input_component = Some(input_component.unwrap_or(render_component));
    }

    fn show_single_menu(&self) {
        let (single_theme,) = {
            let mut s = self.state.lock();
            s.mode = ThemeMode::Single;
            (s.single_theme.clone(),)
        };
        let w1 = self.weak_self.clone();
        let w2 = self.weak_self.clone();
        let w3 = self.weak_self.clone();
        let menu = SelectSubmenu::new(
            "Theme",
            "Select a theme, or choose automatic to follow terminal appearance.",
            single_mode_theme_items(&self.available_themes, &single_theme),
            &single_theme,
            Arc::new(move |value: String| {
                let Some(this) = w1.upgrade() else { return };
                if value == AUTOMATIC_THEME_VALUE {
                    this.state.lock().mode = ThemeMode::Automatic;
                    this.preview(this.get_theme_setting());
                    this.show_automatic_menu();
                    return;
                }
                this.state.lock().single_theme = value.clone();
                this.apply(value);
            }),
            Arc::new(move || {
                if let Some(this) = w2.upgrade() {
                    this.cancel();
                }
            }),
            Some(Arc::new(move |value: String| {
                let Some(this) = w3.upgrade() else { return };
                let preview = if value == AUTOMATIC_THEME_VALUE { this.get_automatic_theme_setting() } else { value };
                this.preview(preview);
            })),
            None,
        );
        self.set_content(Arc::new(menu), None);
    }

    fn show_automatic_menu(&self) {
        let (light, dark) = {
            let mut s = self.state.lock();
            s.mode = ThemeMode::Automatic;
            (s.light_theme.clone(), s.dark_theme.clone())
        };
        let t = theme();
        let content = Arc::new(Container::new());
        content.add_child(Arc::new(Text::new(t.bold(&t.fg(&ThemeColor::Accent, "Automatic Theme")), 0, 0, None)));
        content.add_child(Arc::new(Spacer::new(1)));
        content.add_child(Arc::new(Text::new(
            t.fg(&ThemeColor::Muted, "Choose themes for terminal light and dark appearance."),
            0,
            0,
            None,
        )));
        content.add_child(Arc::new(Text::new(
            t.fg(&ThemeColor::Muted, "Light/dark detection requires terminal support."),
            0,
            0,
            None,
        )));
        content.add_child(Arc::new(Spacer::new(1)));

        let make_submenu = |title: &'static str, description: &'static str, is_light: bool| -> SubmenuFactory {
            let weak = self.weak_self.clone();
            Arc::new(move |current_value: String, done: SubmenuDone| -> ComponentRef {
                let Some(this) = weak.upgrade() else {
                    return Arc::new(Container::new());
                };
                let weak2 = weak.clone();
                let done2 = done.clone();
                this.create_theme_select(
                    title,
                    description,
                    &current_value,
                    done,
                    Arc::new(move |value: String| {
                        let Some(this) = weak2.upgrade() else { return };
                        {
                            let mut s = this.state.lock();
                            if is_light {
                                s.light_theme = value.clone();
                            } else {
                                s.dark_theme = value.clone();
                            }
                        }
                        this.preview(this.get_theme_setting());
                        done2(Some(value), None);
                    }),
                )
            })
        };

        let items = vec![
            SettingItem {
                id: "light-theme".into(),
                label: "Light theme".into(),
                description: Some("Theme to use in automatic mode when the terminal is light".into()),
                current_value: light,
                values: None,
                submenu: Some(make_submenu(
                    "Light Theme",
                    "Select the theme to use for light terminal appearance",
                    true,
                )),
            },
            SettingItem {
                id: "dark-theme".into(),
                label: "Dark theme".into(),
                description: Some("Theme to use in automatic mode when the terminal is dark".into()),
                current_value: dark,
                values: None,
                submenu: Some(make_submenu("Dark Theme", "Select the theme to use for dark terminal appearance", false)),
            },
            setting("apply", "Apply", "Save and go back", "save and go back".into(), &["save and go back"]),
            setting(
                "single-mode",
                "Change mode",
                "Switch to one theme for light and dark",
                "switch to single theme".into(),
                &["switch to single theme"],
            ),
        ];

        let w_change = self.weak_self.clone();
        let w_cancel = self.weak_self.clone();
        let max_visible = items.len().min(10);
        let settings_list = SettingsList::new(
            items,
            max_visible,
            (*get_settings_list_theme()).clone(),
            Arc::new(move |id: String, _value: String| {
                let Some(this) = w_change.upgrade() else { return };
                match id.as_str() {
                    "single-mode" => {
                        let single = {
                            let mut s = this.state.lock();
                            s.mode = ThemeMode::Single;
                            let active = if this.terminal_theme == TerminalTheme::Light {
                                s.light_theme.clone()
                            } else {
                                s.dark_theme.clone()
                            };
                            s.single_theme = active.clone();
                            active
                        };
                        this.preview(single);
                        this.show_single_menu();
                    }
                    "apply" => {
                        let setting = this.get_automatic_theme_setting();
                        this.apply(setting);
                    }
                    _ => {}
                }
            }),
            Arc::new(move || {
                if let Some(this) = w_cancel.upgrade() {
                    this.cancel();
                }
            }),
            SettingsListOptions::default(),
        );
        content.add_child(settings_list.clone());
        self.set_content(content, Some(settings_list));
    }

    fn create_theme_select(
        &self,
        title: &str,
        description: &str,
        current_value: &str,
        done: SubmenuDone,
        on_select: Arc<dyn Fn(String) + Send + Sync>,
    ) -> ComponentRef {
        let w_cancel = self.weak_self.clone();
        let w_change = self.weak_self.clone();
        Arc::new(SelectSubmenu::new(
            title,
            description,
            theme_items(&self.available_themes, current_value),
            current_value,
            on_select,
            Arc::new(move || {
                if let Some(this) = w_cancel.upgrade() {
                    this.preview(this.get_theme_setting());
                }
                done(None, None);
            }),
            Some(Arc::new(move |value: String| {
                if let Some(this) = w_change.upgrade() {
                    this.preview(value);
                }
            })),
            None,
        ))
    }

    fn get_theme_setting(&self) -> String {
        let mode = self.state.lock().mode;
        if mode == ThemeMode::Automatic { self.get_automatic_theme_setting() } else { self.state.lock().single_theme.clone() }
    }

    fn get_automatic_theme_setting(&self) -> String {
        let s = self.state.lock();
        format!("{}/{}", s.light_theme, s.dark_theme)
    }

    fn apply(&self, theme_setting: String) {
        (self.on_done)(Some(theme_setting));
    }

    fn cancel(&self) {
        self.preview(self.original_theme_setting.clone());
        (self.on_done)(None);
    }
}

/// TS: `extends Container`
pub struct ThemeSubmenu {
    inner: Arc<ThemeSubmenuInner>,
}

impl ThemeSubmenu {
    pub fn new(
        current_theme_setting: &str,
        terminal_theme: &TerminalTheme,
        available_themes: &[String],
        callbacks: &SettingsCallbacks,
        on_done: Arc<dyn Fn(Option<String>) + Send + Sync>,
    ) -> Self {
        let auto_theme = parse_auto_theme_setting(Some(current_theme_setting));
        let (light_theme, dark_theme) = default_automatic_themes(current_theme_setting, available_themes);
        let fixed_theme = if auto_theme.is_some() || current_theme_setting.contains('/') {
            None
        } else {
            Some(current_theme_setting.to_string())
        };
        let mode = if auto_theme.is_some() { ThemeMode::Automatic } else { ThemeMode::Single };
        let active_automatic =
            if *terminal_theme == TerminalTheme::Light { light_theme.clone() } else { dark_theme.clone() };
        let single_theme = preferred_theme(
            available_themes,
            fixed_theme.as_deref().or(if auto_theme.is_some() { Some(active_automatic.as_str()) } else { None }),
            SYSTEM_THEME_NAME,
        );
        let inner = Arc::new_cyclic(|weak: &Weak<ThemeSubmenuInner>| ThemeSubmenuInner {
            container: Container::new(),
            on_theme_preview: callbacks.on_theme_preview.clone(),
            available_themes: available_themes.to_vec(),
            terminal_theme: *terminal_theme,
            on_done,
            original_theme_setting: current_theme_setting.to_string(),
            state: Mutex::new(ThemeSubmenuState { input_component: None, mode, single_theme, light_theme, dark_theme }),
            weak_self: weak.clone(),
        });
        if mode == ThemeMode::Automatic {
            inner.show_automatic_menu();
        } else {
            inner.show_single_menu();
        }
        Self { inner }
    }
}

impl Component for ThemeSubmenu {
    fn render(&self, width: usize) -> Vec<String> {
        self.inner.container.render(width)
    }

    fn handle_input(&self, data: &str) {
        let input = self.inner.state.lock().input_component.clone();
        if let Some(input) = input {
            input.handle_input(data);
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
}

/// Main settings selector component.
/// TS: `extends Container`
pub struct SettingsSelectorComponent {
    container: Container,
    settings_list: Arc<SettingsList>,
}

impl SettingsSelectorComponent {
    pub fn new(config: &SettingsConfig, callbacks: &SettingsCallbacks) -> Self {
        let supports_images = get_capabilities().images.is_some();
        let follow_up_key = key_display_text("app.message.followUp".into());
        let cycle_thinking_key = key_display_text("app.thinking.cycle".into());
        let current_warnings = Arc::new(Mutex::new(config.warnings.clone()));
        let current_model_thinking_levels = Arc::new(Mutex::new(config.model_thinking_levels.clone()));
        let default_model_by_value: Arc<HashMap<String, Model>> = Arc::new(
            config.available_default_models.iter().map(|m| (model_setting_key(m), m.clone())).collect(),
        );
        let current_default_model_key =
            if default_model_by_value.contains_key(&config.default_model) { Some(config.default_model.clone()) } else { None };
        let current_model_key = config.current_model.as_ref().map(model_setting_key);

        let mut wheel_values: Vec<u32> = vec![1, 2, 3, 5, 10];
        if let WheelScrollLines::Lines(n) = config.fullscreen_wheel_scroll_lines
            && !wheel_values.contains(&n)
        {
            wheel_values.push(n);
        }
        wheel_values.sort();
        let mut wheel_value_strs = vec!["auto".to_string()];
        wheel_value_strs.extend(wheel_values.iter().map(|n| n.to_string()));

        let warnings_submenu: SubmenuFactory = {
            let current_warnings = current_warnings.clone();
            let on_warnings_change = callbacks.on_warnings_change.clone();
            Arc::new(move |_current: String, done: SubmenuDone| -> ComponentRef {
                let current_warnings2 = current_warnings.clone();
                let on_warnings_change = on_warnings_change.clone();
                let warnings = current_warnings.lock().clone();
                Arc::new(WarningSettingsSubmenu::new(
                    &warnings,
                    Arc::new(move |warnings: WarningSettings| {
                        *current_warnings2.lock() = warnings.clone();
                        on_warnings_change(warnings);
                    }),
                    Arc::new(move || done(None, None)),
                ))
            })
        };

        let model_thinking_submenu: SubmenuFactory = {
            let levels = current_model_thinking_levels.clone();
            let by_value = default_model_by_value.clone();
            let available = config.available_default_models.clone();
            let current_model_key = current_model_key.clone();
            let current_default_model_key = current_default_model_key.clone();
            let global_thinking = config.thinking_level.to_js_string();
            let on_change = callbacks.on_model_thinking_level_change.clone();
            let on_remove = callbacks.on_model_thinking_level_remove.clone();
            Arc::new(move |_current: String, done: SubmenuDone| -> ComponentRef {
                let options_levels = levels.clone();
                let available = available.clone();
                let cmk = current_model_key.clone();
                let cdk = current_default_model_key.clone();
                let preselect_cmk = current_model_key.clone();
                let preselect_cdk = current_default_model_key.clone();
                let title_by_value = by_value.clone();
                let level_by_value = by_value.clone();
                let level_levels = levels.clone();
                let preselect_levels = levels.clone();
                let global_thinking = global_thinking.clone();
                let steps = vec![
                    SteppedSubmenuStep {
                        key: "model".into(),
                        title: SteppedSubmenuStepTitle::String("Per-Model Thinking Level".into()),
                        description: SteppedSubmenuStepDescription::String("Select a model to configure".into()),
                        options: Arc::new(move |_ctx| {
                            let mut sorted = available.clone();
                            sorted.sort_by(|a, b| {
                                use std::cmp::Ordering;
                                let a_key = model_setting_key(a);
                                let b_key = model_setting_key(b);
                                if Some(&a_key) == cmk.as_ref() {
                                    return Ordering::Less;
                                }
                                if Some(&b_key) == cmk.as_ref() {
                                    return Ordering::Greater;
                                }
                                if Some(&a_key) == cdk.as_ref() {
                                    return Ordering::Less;
                                }
                                if Some(&b_key) == cdk.as_ref() {
                                    return Ordering::Greater;
                                }
                                locale_compare(&a.provider, &b.provider)
                            });
                            let overrides = options_levels.lock().clone();
                            let mut items: Vec<SelectItem> = sorted
                                .iter()
                                .map(|model| {
                                    let key = model_setting_key(model);
                                    SelectItem {
                                        description: overrides.get(&key).map(|l| l.to_js_string()),
                                        value: key,
                                        label: model_item_label(model),
                                    }
                                })
                                .collect();
                            if items.is_empty() {
                                items.push(SelectItem {
                                    value: "__none__".into(),
                                    label: "No models available".into(),
                                    description: Some("Log in to a provider or configure an API key first".into()),
                                });
                            }
                            items
                        }),
                        preselect: Some(Arc::new(move |_ctx| preselect_cmk.clone().or_else(|| preselect_cdk.clone()))),
                        searchable: Some(true),
                        layout: Some(model_picker_layout()),
                    },
                    SteppedSubmenuStep {
                        key: "level".into(),
                        title: SteppedSubmenuStepTitle::Fn(Arc::new(move |ctx: IndexMap<String, String>| {
                            let model_key = ctx.get("model").cloned().unwrap_or_default();
                            match title_by_value.get(&model_key) {
                                Some(m) => format!("Thinking Level for {}", model_display_label(m)),
                                None => format!("Thinking Level for {model_key}"),
                            }
                        })),
                        description: SteppedSubmenuStepDescription::String(
                            "Select default thinking level for this model".into(),
                        ),
                        options: Arc::new(move |ctx: IndexMap<String, String>| {
                            let model_key = ctx.get("model").cloned().unwrap_or_default();
                            let Some(model) = level_by_value.get(&model_key) else {
                                return Vec::new();
                            };
                            let levels: Vec<ThinkingLevel> = if model.reasoning {
                                get_supported_thinking_levels(model)
                            } else {
                                vec![ThinkingLevel::Off]
                            };
                            // TS marks the level equal to the stored value and offers "clear" for
                            // any stored value, recognized or not.
                            let stored = level_levels.lock().get(&model_key).cloned();
                            let active_level = stored.as_ref().and_then(|l| l.known_level());
                            let mut items: Vec<SelectItem> = levels
                                .iter()
                                .map(|&level| SelectItem {
                                    value: level.as_str().to_string(),
                                    label: format!(
                                        "{}{}",
                                        if Some(level) == active_level { "✓ " } else { "  " },
                                        level.as_str()
                                    ),
                                    description: Some(thinking_description(level).to_string()),
                                })
                                .collect();
                            if stored.as_ref().is_some_and(|l| l.is_truthy()) {
                                items.push(SelectItem {
                                    value: CLEAR_OVERRIDE_VALUE.into(),
                                    label: "  (clear override)".into(),
                                    description: Some(format!("Revert to global default ({})", global_thinking)),
                                });
                            }
                            items
                        }),
                        preselect: Some(Arc::new(move |ctx: IndexMap<String, String>| {
                            let model_key = ctx.get("model").cloned().unwrap_or_default();
                            preselect_levels.lock().get(&model_key).map(|l| l.to_js_string())
                        })),
                        searchable: None,
                        layout: None,
                    },
                ];

                let complete_levels = levels.clone();
                let complete_by_value = by_value.clone();
                let on_change = on_change.clone();
                let on_remove = on_remove.clone();
                let summary_levels = levels.clone();
                Arc::new(SteppedSubmenu::new(
                    &steps,
                    Arc::new(move |selections: IndexMap<String, String>| {
                        let model_key = selections.get("model").cloned().unwrap_or_default();
                        let Some(model) = complete_by_value.get(&model_key) else { return };
                        let level = selections.get("level").cloned().unwrap_or_default();
                        if level == CLEAR_OVERRIDE_VALUE {
                            on_remove(model.provider.clone(), model.id.clone());
                            complete_levels.lock().shift_remove(&model_key);
                        } else if let Some(level) = ThinkingLevel::parse(&level) {
                            on_change(model.provider.clone(), model.id.clone(), level);
                            complete_levels.lock().insert(model_key, level.into());
                        }
                    }),
                    Arc::new(move || {
                        let summary = model_thinking_overrides_summary(&summary_levels.lock());
                        done(Some(summary), None);
                    }),
                    Some(&SteppedSubmenuOptions { r#loop: Some(true), ..Default::default() }),
                ))
            })
        };

        let theme_submenu: SubmenuFactory = {
            let terminal_theme = config.terminal_theme;
            let available_themes = config.available_themes.clone();
            let callbacks = callbacks.clone();
            Arc::new(move |current_value: String, done: SubmenuDone| -> ComponentRef {
                Arc::new(ThemeSubmenu::new(
                    &current_value,
                    &terminal_theme,
                    &available_themes,
                    &callbacks,
                    Arc::new(move |selected: Option<String>| done(selected, None)),
                ))
            })
        };

        let mut items: Vec<SettingItem> = vec![
            setting(
                "autocompact",
                "Auto-compact",
                "Automatically compact context when it gets too large",
                bool_str(config.auto_compact),
                &["true", "false"],
            ),
            setting(
                "steering-mode",
                "Steering mode",
                "Enter while streaming queues steering messages. 'one-at-a-time': deliver one, wait for response. 'all': deliver all at once.",
                config.steering_mode.to_js_string(),
                &["one-at-a-time", "all"],
            ),
            setting(
                "follow-up-mode",
                "Follow-up mode",
                &format!(
                    "{follow_up_key} queues follow-up messages until agent stops. 'one-at-a-time': deliver one, wait for response. 'all': deliver all at once."
                ),
                config.follow_up_mode.to_js_string(),
                &["one-at-a-time", "all"],
            ),
            setting(
                "transport",
                "Transport",
                "Preferred transport for providers that support multiple transports",
                enum_str(&config.transport),
                &["sse", "websocket", "websocket-cached", "auto"],
            ),
            setting(
                "share-destination",
                "Session share destination",
                "Where to upload sessions with /share (auto prefers Radius when authenticated, otherwise GitHub Gist)",
                config.session_share_destination.as_str().to_string(),
                &["auto", "gist", "radius"],
            ),
            SettingItem {
                id: "http-idle-timeout".into(),
                label: "HTTP idle timeout".into(),
                description: Some(
                    "Maximum idle gap while waiting for HTTP headers or body chunks. Disable for local models that pause longer than five minutes."
                        .into(),
                ),
                current_value: format_http_idle_timeout_ms(config.http_idle_timeout_ms),
                values: Some(HTTP_IDLE_TIMEOUT_CHOICES.iter().map(|c| c.label.to_string()).collect()),
                submenu: None,
            },
            setting(
                "cache-warming-mode",
                "Cache warming",
                "off; streaming while the agent runs; idle also between runs while continuation stays profitable",
                config.cache_warming_mode.as_str().to_string(),
                CACHE_WARMING_MODES,
            ),
            setting(
                "hide-thinking",
                "Hide thinking",
                "Hide thinking blocks in assistant responses",
                bool_str(config.hide_thinking_block),
                &["true", "false"],
            ),
            setting(
                "mermaid-rendering",
                "Mermaid diagrams",
                "Render Mermaid code blocks as Unicode diagrams",
                config.mermaid_rendering_mode.as_str().to_string(),
                &["off", "final", "streaming"],
            ),
            setting(
                "cache-miss-notices",
                "Cache miss notices",
                "Show transcript notices for cache costs and provider recovery diagnostics",
                bool_str(config.show_cache_miss_notices),
                &["true", "false"],
            ),
            setting(
                "collapse-changelog",
                "Collapse changelog",
                "Show condensed changelog after updates",
                bool_str(config.collapse_changelog),
                &["true", "false"],
            ),
            setting(
                "quiet-startup",
                "Quiet startup",
                "Disable verbose printing at startup (header: keep only the startup header)",
                quiet_startup_str(&config.quiet_startup),
                &["true", "header", "false"],
            ),
            setting(
                "install-telemetry",
                "Install telemetry",
                "Off by default in this build. Send an anonymous version/update ping after changelog-detected updates",
                // pi-rs divergence from TS Pi: telemetry is opt-in.
                bool_str(config.enable_install_telemetry),
                &["true", "false"],
            ),
            SettingItem {
                id: "default-project-trust".into(),
                label: "Default project trust".into(),
                description: Some(
                    "Fallback behavior when no extension or saved trust decision decides project trust".into(),
                ),
                current_value: default_project_trust_label(config.default_project_trust).to_string(),
                values: Some(DEFAULT_PROJECT_TRUST_LABELS.iter().map(|(_, l)| l.to_string()).collect()),
                submenu: None,
            },
            setting(
                "double-escape-action",
                "Double-escape action",
                "Action when pressing Escape twice with empty editor",
                config.double_escape_action.to_js_string(),
                &["tree", "fork", "none"],
            ),
            setting(
                "tree-filter-mode",
                "Tree filter mode",
                "Default filter when opening /tree",
                config.tree_filter_mode.as_str().to_string(),
                &["default", "no-tools", "user-only", "labeled-only", "all"],
            ),
            SettingItem {
                id: "warnings".into(),
                label: "Warnings".into(),
                description: Some("Enable or disable individual warnings".into()),
                current_value: "configure".into(),
                values: None,
                submenu: Some(warnings_submenu),
            },
            SettingItem {
                id: "model-thinking".into(),
                label: "Default thinking level per model".into(),
                description: Some(format!(
                    "Override the default thinking level for specific models. {cycle_thinking_key} cycles in-session."
                )),
                current_value: model_thinking_overrides_summary(&current_model_thinking_levels.lock()),
                values: None,
                submenu: Some(model_thinking_submenu),
            },
            setting(
                "tui-mode",
                "TUI mode",
                "Interface layout; regular mode uses the terminal's normal scrollback",
                config.tui_mode.as_str().to_string(),
                &["regular", "fullscreen"],
            ),
            setting(
                "fullscreen-exit-output",
                "Fullscreen exit output",
                "Print the transcript or only a session resume hint when exiting fullscreen mode",
                config.fullscreen_exit_output.as_str().to_string(),
                &["transcript", "resume-hint"],
            ),
            setting(
                "fullscreen-scrollbar",
                "Fullscreen scrollbar",
                "Scrollbar behavior in fullscreen mode; has no effect in regular mode",
                scrollbar_str(config.fullscreen_scrollbar).to_string(),
                &["auto", "always", "hidden"],
            ),
            setting(
                "fullscreen-copy-on-select",
                "Fullscreen copy on select",
                "Automatically copy selected text in fullscreen mode; disable to copy selections with Ctrl+X",
                bool_str(config.fullscreen_copy_on_select),
                &["true", "false"],
            ),
            SettingItem {
                id: "fullscreen-wheel-scroll-lines".into(),
                label: "Fullscreen wheel scrolling".into(),
                description: Some(
                    "Lines per mouse-wheel event in fullscreen mode; 'auto' speeds up fast wheel spins where the terminal does not"
                        .into(),
                ),
                current_value: wheel_scroll_lines_str(&config.fullscreen_wheel_scroll_lines),
                values: Some(wheel_value_strs),
                submenu: None,
            },
            SettingItem {
                id: "theme".into(),
                label: "Theme".into(),
                description: Some("Color theme for the interface".into()),
                current_value: config.current_theme.clone(),
                values: None,
                submenu: Some(theme_submenu),
            },
        ];

        // Only show image toggle if terminal supports it
        if supports_images {
            // Insert after autocompact
            items.insert(
                1,
                setting(
                    "show-images",
                    "Show images",
                    "Render images inline in terminal",
                    bool_str(config.show_images),
                    &["true", "false"],
                ),
            );
            items.insert(
                2,
                setting(
                    "image-width-cells",
                    "Image width",
                    "Preferred inline image width in terminal cells",
                    config.image_width_cells.to_string(),
                    &["60", "80", "120"],
                ),
            );
        }

        // Image auto-resize toggle (always available, affects both attached and read images)
        items.insert(
            if supports_images { 3 } else { 1 },
            setting(
                "auto-resize-images",
                "Auto-resize images",
                "Resize large images to 2000x2000 max for better model compatibility",
                bool_str(config.auto_resize_images),
                &["true", "false"],
            ),
        );

        let insert_after = |items: &mut Vec<SettingItem>, after: &str, item: SettingItem| {
            let index = items.iter().position(|i| i.id == after).map(|i| i as i64).unwrap_or(-1);
            items.insert((index + 1) as usize, item);
        };

        // Block images toggle (always available, insert after auto-resize-images)
        insert_after(
            &mut items,
            "auto-resize-images",
            setting(
                "block-images",
                "Block images",
                "Prevent images from being sent to LLM providers",
                bool_str(config.block_images),
                &["true", "false"],
            ),
        );
        // Skill commands toggle (insert after block-images)
        insert_after(
            &mut items,
            "block-images",
            setting(
                "skill-commands",
                "Skill commands",
                "Register skills as /skill:name commands",
                bool_str(config.enable_skill_commands),
                &["true", "false"],
            ),
        );
        // Hardware cursor toggle (insert after skill-commands)
        insert_after(
            &mut items,
            "skill-commands",
            setting(
                "show-hardware-cursor",
                "Show hardware cursor",
                "Use the terminal cursor instead of Pi's drawn cursor",
                bool_str(config.show_hardware_cursor),
                &["true", "false"],
            ),
        );
        // Editor padding toggle (insert after show-hardware-cursor)
        insert_after(
            &mut items,
            "show-hardware-cursor",
            setting(
                "editor-padding",
                "Editor padding",
                "Horizontal padding for input editor (0-3)",
                config.editor_padding_x.to_string(),
                &["0", "1", "2", "3"],
            ),
        );
        // Output padding toggle (insert after editor-padding)
        insert_after(
            &mut items,
            "editor-padding",
            setting(
                "output-padding",
                "Output padding",
                "Horizontal padding for messages, tool output, and command output",
                config.output_pad.to_string(),
                &["0", "1"],
            ),
        );
        // Autocomplete max visible toggle (insert after output-padding)
        insert_after(
            &mut items,
            "output-padding",
            setting(
                "autocomplete-max-visible",
                "Autocomplete max items",
                "Max visible items in autocomplete dropdown (3-20)",
                config.autocomplete_max_visible.to_string(),
                &["3", "5", "7", "10", "15", "20"],
            ),
        );
        // Clear on shrink toggle (insert after autocomplete-max-visible)
        insert_after(
            &mut items,
            "autocomplete-max-visible",
            setting(
                "clear-on-shrink",
                "Clear on shrink",
                "Clear empty rows when content shrinks (may cause flicker)",
                bool_str(config.clear_on_shrink),
                &["true", "false"],
            ),
        );
        // Terminal progress toggle (insert after clear-on-shrink)
        insert_after(
            &mut items,
            "clear-on-shrink",
            setting(
                "terminal-progress",
                "Terminal progress",
                "Show OSC 9;4 progress indicators in the terminal tab bar",
                bool_str(config.show_terminal_progress),
                &["true", "false"],
            ),
        );

        let container = Container::new();
        // Add borders
        container.add_child(Arc::new(DynamicBorder::new(None)));

        let cb = callbacks.clone();
        let settings_list = SettingsList::new(
            items,
            10,
            (*get_settings_list_theme()).clone(),
            Arc::new(move |id: String, new_value: String| {
                let v = new_value.as_str();
                match id.as_str() {
                    "autocompact" => (cb.on_auto_compact_change)(v == "true"),
                    "show-images" => (cb.on_show_images_change)(v == "true"),
                    "image-width-cells" => (cb.on_image_width_cells_change)(parse_int(v)),
                    "auto-resize-images" => (cb.on_auto_resize_images_change)(v == "true"),
                    "block-images" => (cb.on_block_images_change)(v == "true"),
                    "skill-commands" => (cb.on_enable_skill_commands_change)(v == "true"),
                    "steering-mode" => {
                        if let Some(mode) = parse_enum(v) {
                            (cb.on_steering_mode_change)(mode);
                        }
                    }
                    "follow-up-mode" => {
                        if let Some(mode) = parse_enum(v) {
                            (cb.on_follow_up_mode_change)(mode);
                        }
                    }
                    "transport" => {
                        if let Some(transport) = parse_enum::<Transport>(v) {
                            (cb.on_transport_change)(transport);
                        }
                    }
                    "share-destination" => {
                        if let Some(dest) = parse_enum(v) {
                            (cb.on_session_share_destination_change)(dest);
                        }
                    }
                    "http-idle-timeout" => {
                        if let Some(choice) = HTTP_IDLE_TIMEOUT_CHOICES.iter().find(|c| c.label == v) {
                            (cb.on_http_idle_timeout_ms_change)(choice.timeout_ms);
                        }
                    }
                    "cache-warming-mode" => {
                        if let Some(mode) = parse_enum(v) {
                            (cb.on_cache_warming_mode_change)(mode);
                        }
                    }
                    "hide-thinking" => (cb.on_hide_thinking_block_change)(v == "true"),
                    "mermaid-rendering" => {
                        if let Some(mode) = parse_enum(v) {
                            (cb.on_mermaid_rendering_mode_change)(mode);
                        }
                    }
                    "cache-miss-notices" => (cb.on_show_cache_miss_notices_change)(v == "true"),
                    "collapse-changelog" => (cb.on_collapse_changelog_change)(v == "true"),
                    "quiet-startup" => (cb.on_quiet_startup_change)(if v == "header" {
                        QuietStartupValue::Header("header".into())
                    } else {
                        QuietStartupValue::Bool(v == "true")
                    }),
                    "install-telemetry" => (cb.on_enable_install_telemetry_change)(v == "true"),
                    "default-project-trust" => {
                        if let Some((value, _)) = DEFAULT_PROJECT_TRUST_LABELS.iter().find(|(_, l)| *l == v) {
                            (cb.on_default_project_trust_change)(*value);
                        }
                    }
                    "double-escape-action" => {
                        if let Some(action) = parse_enum(v) {
                            (cb.on_double_escape_action_change)(action);
                        }
                    }
                    "tree-filter-mode" => {
                        if let Some(mode) = parse_enum(v) {
                            (cb.on_tree_filter_mode_change)(mode);
                        }
                    }
                    "show-hardware-cursor" => (cb.on_show_hardware_cursor_change)(v == "true"),
                    "editor-padding" => (cb.on_editor_padding_x_change)(parse_int(v)),
                    "output-padding" => (cb.on_output_pad_change)(if v == "0" { 0 } else { 1 }),
                    "autocomplete-max-visible" => (cb.on_autocomplete_max_visible_change)(parse_int(v)),
                    "clear-on-shrink" => (cb.on_clear_on_shrink_change)(v == "true"),
                    "terminal-progress" => (cb.on_show_terminal_progress_change)(v == "true"),
                    "tui-mode" => {
                        if let Some(mode) = parse_enum(v) {
                            (cb.on_tui_mode_change)(mode);
                        }
                    }
                    "fullscreen-exit-output" => {
                        if let Some(output) = parse_enum(v) {
                            (cb.on_fullscreen_exit_output_change)(output);
                        }
                    }
                    "fullscreen-scrollbar" => {
                        if let Some(mode) = parse_scrollbar(v) {
                            (cb.on_fullscreen_scrollbar_change)(mode);
                        }
                    }
                    "fullscreen-copy-on-select" => (cb.on_fullscreen_copy_on_select_change)(v == "true"),
                    "fullscreen-wheel-scroll-lines" => {
                        (cb.on_fullscreen_wheel_scroll_lines_change)(if v == "auto" {
                            WheelScrollLines::Auto("auto".into())
                        } else {
                            WheelScrollLines::Lines(parse_int(v).max(0) as u32)
                        })
                    }
                    "theme" => (cb.on_theme_change)(new_value.clone()),
                    _ => {}
                }
            }),
            callbacks.on_cancel.clone(),
            SettingsListOptions { enable_search: true },
        );

        container.add_child(settings_list.clone());
        container.add_child(Arc::new(DynamicBorder::new(None)));
        Self { container, settings_list }
    }

    pub fn get_settings_list(&self) -> Arc<SettingsList> {
        self.settings_list.clone()
    }
}

impl Component for SettingsSelectorComponent {
    fn render(&self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    fn invalidate(&self) {
        self.container.invalidate();
    }

    fn handle_mouse(&self, event: &MouseEvent) -> Option<MouseResult> {
        self.container.handle_mouse(event)
    }

    fn child_components(&self) -> Option<Vec<ComponentRef>> {
        self.container.child_components()
    }
}
