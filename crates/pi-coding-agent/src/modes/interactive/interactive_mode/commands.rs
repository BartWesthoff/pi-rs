//! Editor submission, key handlers and slash command handlers.

use super::*;
use crate::core::agent_session::{AgentSessionCycleModelDirection, AgentSessionExportToHtmlOptions};
use crate::core::agent_session_runtime::{AgentSessionRuntimeForkOptions, AgentSessionRuntimeSwitchSessionOptions};
use crate::core::extensions::types::{UserBashEvent, UserBashEventResult, WithSessionFn};
use crate::core::session_cwd::MissingSessionCwdError;
use crate::modes::interactive::components::dynamic_border::DynamicBorder;
use crate::modes::interactive::components::easter_egg_3d::{EasterEgg3d, play_easter_egg3d};
use crate::modes::interactive::components::footer::format_tokens;
use crate::modes::interactive::components::keybinding_hints::{KeyTextFormatOptions, format_key_text, key_display_text};
use crate::core::tools::js_util::js_to_fixed;
use pi_tui::components::markdown::Markdown;

/// `Number.prototype.toLocaleString()` for integers (en-US grouping).
pub(crate) fn locale_int(value: f64) -> String {
    let negative = value < 0.0;
    let digits = format!("{}", value.abs().round() as u64);
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if negative { format!("-{out}") } else { out }
}

/// Plays a 3D easter egg. Only fullscreen mode can show it, because it dissolves the rendered screen.
/// Returns false when it cannot play.
pub(crate) fn play_easter_egg_3d(inner: &Inner, egg: EasterEgg3d) -> bool {
    let tui = inner.ui();
    if !tui.is_viewport_tui() {
        return false;
    }
    if tui.has_overlay() {
        return true;
    }
    let screen = tui.get_screen_lines();
    inner.spawn(async move {
        let _ = play_easter_egg3d(tui, &screen, &egg).await;
    });
    true
}

impl Inner {
    /// The editor's submit callback (TS `defaultEditor.onSubmit`).
    fn submit_callback(&self) -> pi_tui::components::editor::TextCallback {
        let weak = self.weak.clone();
        Arc::new(move |text: String| {
            if let Some(this) = weak.upgrade() {
                this.handle_submit(text);
            }
        })
    }

    pub(crate) fn install_submit_callback(&self) {
        self.default_editor.set_on_submit(Some(self.submit_callback()));
    }

    pub(crate) fn install_submit_callback_on(&self, editor: &Arc<dyn EditorComponent>) {
        editor.set_on_submit(Some(self.submit_callback()));
    }

    pub(crate) fn on_change_callback(&self) -> pi_tui::components::editor::TextCallback {
        let weak = self.weak.clone();
        Arc::new(move |text: String| {
            if let Some(this) = weak.upgrade() {
                let changed = {
                    let mut s = this.state.lock();
                    let was = s.is_bash_mode;
                    s.is_bash_mode = text.trim_start().starts_with('!');
                    was != s.is_bash_mode
                };
                if changed {
                    this.update_editor_border_color();
                }
            }
        })
    }

    fn action(&self, name: &str, f: impl Fn(Arc<Inner>) + Send + Sync + 'static) {
        let weak = self.weak.clone();
        self.default_editor.on_action(
            &name.to_string(),
            Arc::new(move || {
                if let Some(this) = weak.upgrade() {
                    f(this);
                }
            }),
        );
    }

    pub(crate) fn setup_key_handlers(&self) {
        // Handlers use the active editor for text access, so they work regardless of which editor is active.
        let weak = self.weak.clone();
        self.default_editor.set_on_escape(Some(Arc::new(move || {
            let Some(this) = weak.upgrade() else { return };
            let session = this.session();
            if session.is_streaming() {
                this.restore_queued_messages_to_editor(true, None);
            } else if session.is_bash_running() {
                session.abort_bash();
            } else if this.state.lock().is_bash_mode {
                this.editor().set_text("");
                this.state.lock().is_bash_mode = false;
                this.update_editor_border_color();
            } else if this.editor().get_text().trim().is_empty() {
                // Double-escape with empty editor triggers /tree, /fork, or nothing based on setting
                use crate::core::settings_manager::SettingsManagerGetDoubleEscapeActionResult as Action;
                let action = this.settings_manager().get_double_escape_action();
                if action != Action::None {
                    let now = now_ms();
                    let last = this.state.lock().last_escape_time;
                    if now - last < 500 {
                        if action == Action::Tree {
                            this.show_tree_selector(None);
                        } else {
                            this.show_user_message_selector();
                        }
                        this.state.lock().last_escape_time = 0;
                    } else {
                        this.state.lock().last_escape_time = now;
                    }
                }
            }
        })));

        self.action("app.clear", |this| this.handle_ctrl_c());
        let weak = self.weak.clone();
        self.default_editor.set_on_ctrl_d(Some(Arc::new(move || {
            if let Some(this) = weak.upgrade() {
                this.handle_ctrl_d();
            }
        })));
        self.action("app.suspend", |this| {
            // TS throws out of the key handler, which ends in the uncaught-exception handler.
            if let Err(error) = this.handle_ctrl_z() {
                this.uncaught_crash(&anyhow::Error::new(error));
            }
        });
        self.action("app.thinking.cycle", |this| this.cycle_thinking_level());
        self.action("app.model.cycleForward", |this| {
            let target = this.clone();
            this.spawn(async move { target.cycle_model(AgentSessionCycleModelDirection::Forward).await });
        });
        self.action("app.model.cycleBackward", |this| {
            let target = this.clone();
            this.spawn(async move { target.cycle_model(AgentSessionCycleModelDirection::Backward).await });
        });

        // Global debug handler on TUI (works regardless of focus)
        let weak = self.weak.clone();
        self.ui().set_on_debug(Some(Arc::new(move || {
            if let Some(this) = weak.upgrade() {
                this.handle_debug_command();
            }
        })));
        self.action("app.model.select", |this| this.show_model_selector(None));
        self.action("app.tools.expand", |this| this.toggle_tool_output_expansion());
        self.action("app.thinking.toggle", |this| this.toggle_thinking_block_visibility());
        self.action("app.editor.external", |this| {
            let target = this.clone();
            this.spawn(async move { target.handle_open_external_editor().await });
        });
        self.action("app.message.copy", |this| {
            let target = this.clone();
            this.spawn(async move { target.handle_copy_command(true, true).await });
        });
        self.action("app.message.followUp", |this| {
            let target = this.clone();
            this.spawn(async move { target.handle_follow_up().await });
        });
        self.action("app.message.dequeue", |this| this.handle_dequeue());
        self.action("app.session.new", |this| {
            let target = this.clone();
            this.spawn(async move { target.handle_clear_command().await });
        });
        self.action("app.session.tree", |this| this.show_tree_selector(None));
        self.action("app.session.fork", |this| this.show_user_message_selector());
        self.action("app.session.resume", |this| this.show_session_selector());

        self.default_editor.set_on_change(Some(self.on_change_callback()));

        // Handle clipboard paste (triggered on Ctrl+V). Copied files use their original paths,
        // images are attached via temporary files, and plain text is the final fallback.
        let weak = self.weak.clone();
        self.default_editor.set_on_paste_image(Some(Arc::new(move || {
            if let Some(this) = weak.upgrade() {
                let target = this.clone();
                this.spawn(async move { target.handle_clipboard_paste().await });
            }
        })));
    }

    pub(crate) async fn handle_right_click_paste(self: Arc<Self>) {
        let renderer = self.ui();
        let Some(target) = renderer.get_focused_component() else { return };
        if !target.handles_input() {
            return;
        }
        if let Ok(Some(text)) = crate::utils::clipboard::read_clipboard_text().await {
            if text.is_empty() {
                return;
            }
            let focused = self.ui().get_focused_component();
            if !focused.is_some_and(|f| pi_tui::tui::same_component(&f, &target)) {
                return;
            }
            target.handle_input(&format!("\x1b[200~{text}\x1b[201~"));
            self.request_render();
        }
    }

    async fn handle_clipboard_paste(self: Arc<Self>) {
        let result: anyhow::Result<()> = async {
            if let Some(file_paths) = crate::utils::clipboard::read_clipboard_file_paths().await? {
                if file_paths.iter().any(|p| p.chars().any(|c| c.is_control())) {
                    anyhow::bail!("Clipboard file path contains control characters");
                }
                let is_bash = self.state.lock().is_bash_mode;
                let paths = if is_bash {
                    file_paths.iter().map(|p| quote_if_needed(p)).collect::<Vec<_>>().join(" ")
                } else {
                    file_paths.join("\n")
                };
                let editor = self.editor();
                let component: ComponentRef = editor.clone();
                let cursor = pi_tui::tui::downcast_component::<CustomEditor>(&component).map(|e| e.editor().get_cursor());
                let (before, after) = match cursor {
                    Some((line, col)) => {
                        let text = editor.get_text();
                        let current: Vec<char> = text.split('\n').nth(line).unwrap_or("").chars().collect();
                        let before = if col > 0 { current.get(col - 1).copied() } else { None };
                        (before, current.get(col).copied())
                    }
                    None => (None, None),
                };
                let leading = if before.is_some_and(|c| !c.is_whitespace()) { " " } else { "" };
                let trailing = if after.is_some_and(|c| !c.is_whitespace()) { " " } else { "" };
                editor.insert_text_at_cursor(&format!("{leading}{paths}{trailing}"));
                self.request_render();
                return Ok(());
            }

            if let Some(image) = crate::utils::clipboard_image::read_clipboard_image(None).await? {
                let ext = crate::utils::clipboard_image::extension_for_image_mime_type(&image.mime_type)
                    .unwrap_or_else(|| "png".to_string());
                let file_path = std::env::temp_dir().join(format!("pi-clipboard-{}.{ext}", uuid::Uuid::new_v4()));
                std::fs::write(&file_path, &image.bytes)?;
                self.editor().insert_text_at_cursor(&file_path.to_string_lossy());
                self.request_render();
                return Ok(());
            }

            if let Some(text) = crate::utils::clipboard::read_clipboard_text().await?
                && !text.is_empty()
            {
                self.editor().insert_text_at_cursor(&text);
                self.request_render();
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            self.show_error(&format!("Failed to paste from clipboard: {error}"));
        }
    }

    /// The editor submit handler (TS `defaultEditor.onSubmit`, async parts run on the runtime).
    pub(crate) fn handle_submit(self: Arc<Self>, text: String) {
        if !self.state.lock().submit_enabled {
            // Startup is still in progress.
            self.editor().set_text(&text);
            self.show_status("Startup is still in progress");
            return;
        }
        // The handler runs synchronously up to its first await, like the TS async handler, so
        // consecutive submissions keep their order and editor updates happen in input order.
        let this = self.clone();
        self.start_now(async move { this.handle_submit_async(text).await });
    }

    async fn handle_submit_async(self: Arc<Self>, text: String) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        let editor = self.editor();
        let clear = |editor: &Arc<dyn EditorComponent>| editor.set_text("");

        // Handle commands
        match text.as_str() {
            "/settings" => {
                self.show_settings_selector();
                clear(&editor);
                return;
            }
            "/scoped-models" => {
                clear(&editor);
                self.show_models_selector();
                return;
            }
            "/refresh-models" => {
                clear(&editor);
                self.clone().handle_refresh_models_command().await;
                return;
            }
            "/share" => {
                self.clone().handle_share_command().await;
                clear(&editor);
                return;
            }
            "/copy" => {
                self.clone().handle_copy_command(false, false).await;
                clear(&editor);
                return;
            }
            "/session" => {
                self.handle_session_command();
                clear(&editor);
                return;
            }
            "/changelog" => {
                self.handle_changelog_command();
                clear(&editor);
                return;
            }
            "/hotkeys" => {
                self.handle_hotkeys_command();
                clear(&editor);
                return;
            }
            "/fork" => {
                self.show_user_message_selector();
                clear(&editor);
                return;
            }
            "/clone" => {
                clear(&editor);
                self.clone().handle_clone_command().await;
                return;
            }
            "/tree" => {
                self.show_tree_selector(None);
                clear(&editor);
                return;
            }
            "/trust" => {
                self.show_trust_selector();
                clear(&editor);
                return;
            }
            "/logout" => {
                let this = self.clone();
                self.spawn(async move { this.show_oauth_selector_logout().await });
                clear(&editor);
                return;
            }
            "/new" => {
                clear(&editor);
                self.clone().handle_clear_command().await;
                return;
            }
            "/reload" => {
                clear(&editor);
                self.clone().handle_reload_command().await;
                return;
            }
            "/debug" => {
                self.handle_debug_command();
                clear(&editor);
                return;
            }
            "/arminsayshi" => {
                self.handle_armin_says_hi();
                clear(&editor);
                return;
            }
            "/dementedelves" => {
                self.handle_demented_delves();
                clear(&editor);
                return;
            }
            "/resume" => {
                self.show_session_selector();
                clear(&editor);
                return;
            }
            "/quit" => {
                clear(&editor);
                self.clone().shutdown(false).await;
                return;
            }
            _ => {}
        }
        if text == "/model" || text.starts_with("/model ") {
            let search = text.strip_prefix("/model ").map(|s| s.trim().to_string());
            clear(&editor);
            self.clone().handle_model_command(search).await;
            return;
        }
        if text == "/thinking" || text.starts_with("/thinking ") {
            let search = text.strip_prefix("/thinking ").map(|s| s.trim().to_string());
            clear(&editor);
            self.handle_thinking_command(search.as_deref());
            return;
        }
        if text == "/export" || text.starts_with("/export ") {
            self.clone().handle_export_command(&text).await;
            clear(&editor);
            return;
        }
        if text == "/import" || text.starts_with("/import ") {
            self.clone().handle_import_command(&text).await;
            clear(&editor);
            return;
        }
        if text == "/bug" || text.starts_with("/bug ") {
            let hint = text["/bug".len()..].trim().to_string();
            clear(&editor);
            self.clone().handle_bug_command(if hint.is_empty() { None } else { Some(hint) }).await;
            return;
        }
        if text == "/name" || text.starts_with("/name ") {
            self.handle_name_command(&text);
            clear(&editor);
            return;
        }
        if text == "/login" || text.starts_with("/login ") {
            let provider_ref = text.strip_prefix("/login ").map(|s| s.trim().to_string());
            clear(&editor);
            self.clone().handle_login_command(provider_ref).await;
            return;
        }
        if text == "/compact" || text.starts_with("/compact ") {
            let custom = text.strip_prefix("/compact ").map(|s| s.trim().to_string());
            clear(&editor);
            self.clone().handle_compact_command(custom).await;
            return;
        }

        // Handle bash command (! for normal, !! for excluded from context)
        if text.starts_with('!') {
            let excluded = text.starts_with("!!");
            let command = text.strip_prefix(if excluded { "!!" } else { "!" }).unwrap_or(&text).trim().to_string();
            if !command.is_empty() {
                if self.session().is_bash_running() {
                    self.show_warning("A bash command is already running. Press Esc to cancel it first.");
                    editor.set_text(&text);
                    return;
                }
                editor.add_to_history(&text);
                self.clone().handle_bash_command(&command, excluded).await;
                self.state.lock().is_bash_mode = false;
                self.update_editor_border_color();
                return;
            }
        }

        let session = self.session();
        // Queue input during compaction (extension commands execute immediately)
        if session.is_compacting() {
            if self.is_extension_command(&text) {
                editor.add_to_history(&text);
                editor.set_text("");
                if let Err(error) = session.prompt(&text, None).await {
                    self.show_error(&err_message(&error));
                }
            } else {
                self.queue_compaction_message(&text, CompactionQueuedMessageMode::Steer);
            }
            return;
        }

        // If streaming, use prompt() with steer behavior
        if session.is_streaming() {
            editor.add_to_history(&text);
            editor.set_text("");
            let options =
                PromptOptions { streaming_behavior: Some(PromptOptionsStreamingBehavior::Steer), ..Default::default() };
            if let Err(error) = session.prompt(&text, Some(&options)).await {
                self.show_error(&err_message(&error));
            }
            self.update_pending_messages_display();
            self.request_render();
            return;
        }

        // Normal message submission: first, move any pending bash components to chat
        self.flush_pending_bash_components();
        let _ = self.input_tx.send(text.clone());
        editor.add_to_history(&text);
    }

    // ------------------------------------------------------------------
    // Command handlers
    // ------------------------------------------------------------------

    pub(crate) async fn handle_reload_command(self: Arc<Self>) {
        let session = self.session();
        if session.is_streaming() {
            self.show_warning("Wait for the current response to finish before reloading.");
            return;
        }
        if session.is_compacting() {
            self.show_warning("Wait for compaction to finish before reloading.");
            return;
        }
        self.reset_extension_ui();

        let reload_box = Container::new_ref();
        let border: Arc<dyn Fn(String) -> String + Send + Sync> = Arc::new(|s: String| theme().fg(&ThemeColor::Border, &s));
        reload_box.add_child(Arc::new(DynamicBorder::new(Some(border.clone()))));
        reload_box.add_child(Arc::new(Spacer::new(1)));
        reload_box.add_child(Arc::new(ThemedText::new(
            Arc::new(|| {
                theme().fg(&ThemeColor::Muted, "Reloading keybindings, extensions, skills, prompts, themes, and context files...")
            }),
            Some(1),
            Some(0),
        )));
        reload_box.add_child(Arc::new(Spacer::new(1)));
        reload_box.add_child(Arc::new(DynamicBorder::new(Some(border))));

        let previous_editor = self.editor_ref();
        self.editor_container.clear();
        self.editor_container.add_child(reload_box.clone());
        let ui = self.ui();
        ui.set_focus(Some(reload_box));
        ui.request_render(true);
        tokio::task::yield_now().await;

        let dismiss = |this: &Inner, editor: ComponentRef| {
            this.editor_container.clear();
            this.editor_container.add_child(editor.clone());
            let ui = this.ui();
            ui.set_focus(Some(editor));
            ui.request_render(false);
        };

        let restored = Arc::new(AtomicBool::new(false));
        let restore_chat = {
            let weak = self.weak.clone();
            let restored = restored.clone();
            move || {
                if restored.swap(true, Ordering::SeqCst) {
                    return;
                }
                if let Some(this) = weak.upgrade() {
                    let settings = this.settings_manager();
                    {
                        let mut s = this.state.lock();
                        s.hide_thinking_block = settings.get_hide_thinking_block();
                        s.output_pad = settings.get_output_pad();
                    }
                    this.rebuild_chat_from_messages();
                }
            }
        };
        let restore_chat = Arc::new(restore_chat);
        let before = restore_chat.clone();
        let options = crate::core::agent_session::AgentSessionReloadOptions {
            before_session_start: Some(Arc::new(move || {
                before();
                Box::pin(async { Ok(()) })
            })),
        };
        match session.reload(Some(&options)).await {
            Ok(()) => {
                restore_chat();
                self.keybindings.reload();
                let (header, expanded) = {
                    let s = self.state.lock();
                    (s.custom_header.clone().or_else(|| s.built_in_header.clone()), s.tool_output_expanded)
                };
                if let Some(header) = header {
                    set_expanded_on(&header, expanded);
                }
                set_registered_themes(self.session().resource_loader().get_themes().themes);
                self.apply_runtime_settings();
                self.theme_controller.apply_from_settings();
                self.setup_autocomplete_provider();
                self.setup_extension_shortcuts();
                self.show_loaded_resources(None, false, true);
                let saved_trust = self.maybe_save_implicit_project_trust_after_reload();
                if let Some(error) = self.session().model_runtime().get_error() {
                    self.show_error(&format!("models.json error: {error}"));
                }
                self.show_status(if saved_trust {
                    "Reloaded keybindings, extensions, skills, prompts, themes, and context files; saved project trust"
                } else {
                    "Reloaded keybindings, extensions, skills, prompts, themes, and context files"
                });
                dismiss(&self, self.editor_ref());
            }
            Err(error) => {
                dismiss(&self, previous_editor);
                self.show_error(&format!("Reload failed: {error}"));
            }
        }
    }

    async fn handle_export_command(self: Arc<Self>, text: &str) {
        let output_path = get_path_command_argument(text, "/export");
        let session = self.session();
        let result = if output_path.as_deref().is_some_and(|p| p.ends_with(".jsonl")) {
            session.export_to_jsonl(output_path.as_deref())
        } else {
            session
                .export_to_html(output_path.as_deref(), Some(&AgentSessionExportToHtmlOptions { theme_name: theme().name() }))
                .await
        };
        match result {
            Ok(file_path) => self.show_status(&format!("Session exported to: {file_path}")),
            Err(error) => self.show_error(&format!("Failed to export session: {error}")),
        }
    }

    async fn handle_import_command(self: Arc<Self>, text: &str) {
        let Some(input_path) = get_path_command_argument(text, "/import") else {
            self.show_error("Usage: /import <path.jsonl>");
            return;
        };
        let confirmed = self
            .show_extension_confirm(
                "Import session",
                &format!("Replace current session with {input_path}?"),
                Default::default(),
            )
            .await;
        if !confirmed {
            self.show_status("Import cancelled");
            return;
        }
        self.clear_status_indicator(None);
        match self.runtime_host.import_from_jsonl(&input_path, None).await {
            Ok(result) => {
                if result.cancelled {
                    self.show_status("Import cancelled");
                } else {
                    self.show_status(&format!("Session imported from: {input_path}"));
                }
            }
            Err(error) => {
                if let Some(missing) = error.downcast_ref::<MissingSessionCwdError>() {
                    let Some(selected) = self.prompt_for_missing_session_cwd(missing).await else {
                        self.show_status("Import cancelled");
                        return;
                    };
                    match self.runtime_host.import_from_jsonl(&input_path, Some(&selected)).await {
                        Ok(result) if result.cancelled => self.show_status("Import cancelled"),
                        Ok(_) => self.show_status(&format!("Session imported from: {input_path}")),
                        Err(error) => self.handle_fatal_runtime_error("Failed to import session", &error),
                    }
                    return;
                }
                if error.downcast_ref::<crate::core::agent_session_runtime::SessionImportFileNotFoundError>().is_some() {
                    self.show_error(&format!("Failed to import session: {error}"));
                    return;
                }
                self.handle_fatal_runtime_error("Failed to import session", &error);
            }
        }
    }

    async fn handle_share_command(self: Arc<Self>) {
        let context = crate::modes::interactive::session_share::SessionShareContext {
            session: self.session(),
            ui: self.ui(),
            editor_container: self.editor_container.clone(),
            editor: self.editor(),
            show_status: self.status_fn(),
            show_error: self.error_fn(),
        };
        if let Err(error) = crate::modes::interactive::session_share::share_session(&context).await {
            self.show_error(&format!("Failed to export session: {error}"));
        }
    }

    async fn handle_bug_command(self: Arc<Self>, hint: Option<String>) {
        let context = crate::modes::interactive::bug_report::BugReportContext {
            session: self.session(),
            ui: self.ui(),
            editor_container: self.editor_container.clone(),
            editor: self.editor(),
            keybindings: self.keybindings.clone(),
            show_status: self.status_fn(),
            show_error: self.error_fn(),
        };
        if let Err(error) = crate::modes::interactive::bug_report::report_bug(&context, hint.as_deref()).await {
            self.show_error(&error.to_string());
        }
    }

    pub(crate) fn status_fn(&self) -> Arc<dyn Fn(String) + Send + Sync> {
        let weak = self.weak.clone();
        Arc::new(move |message: String| {
            if let Some(this) = weak.upgrade() {
                this.show_status(&message);
            }
        })
    }

    pub(crate) fn error_fn(&self) -> Arc<dyn Fn(String) + Send + Sync> {
        let weak = self.weak.clone();
        Arc::new(move |message: String| {
            if let Some(this) = weak.upgrade() {
                this.show_error(&message);
            }
        })
    }

    pub(crate) async fn handle_copy_command(self: Arc<Self>, flash_confirmation: bool, prefer_selection: bool) {
        let ui = self.ui();
        if prefer_selection && ui.is_viewport_tui() && !ui.get_copy_on_select() && ui.has_active_selection() {
            let _ = ui.copy_active_selection_to_clipboard().await;
            return;
        }
        let Some(text) = self.session().get_last_assistant_text() else {
            self.show_error("No agent messages to copy yet.");
            return;
        };
        match crate::utils::clipboard::copy_to_clipboard(&text).await {
            Ok(()) => {
                if flash_confirmation && ui.is_viewport_tui() {
                    ui.flash("Copied!", None);
                } else {
                    self.show_status("Copied last agent message to clipboard");
                }
            }
            Err(error) => self.show_error(&error.to_string()),
        }
    }

    fn handle_name_command(&self, text: &str) {
        static NAME_PREFIX: std::sync::LazyLock<regex::Regex> =
            std::sync::LazyLock::new(|| regex::Regex::new(r"^/name\s*").unwrap());
        let name = NAME_PREFIX.replace(text, "").trim().to_string();
        let sm = self.session_manager();
        if name.is_empty() {
            match sm.get_session_name() {
                Some(current) => {
                    self.add_spacer();
                    self.add_text(
                        &self.chat_container,
                        Arc::new(move || theme().fg(&ThemeColor::Dim, &format!("Session name: {current}"))),
                        1,
                        0,
                    );
                }
                None => self.show_warning("Usage: /name <name>"),
            }
            self.request_render();
            return;
        }
        self.session().set_session_name(&name);
        let session_name = sm.get_session_name();
        if session_name.as_deref() != Some(name.as_str()) {
            self.show_warning(&format!(
                "Session name was normalized from {} to {}",
                serde_json::to_string(&name).unwrap_or_default(),
                serde_json::to_string(&session_name).unwrap_or_default()
            ));
        }
        self.add_spacer();
        let display = session_name.unwrap_or(name);
        self.add_text(
            &self.chat_container,
            Arc::new(move || theme().fg(&ThemeColor::Dim, &format!("Session name set: {display}"))),
            1,
            0,
        );
        self.request_render();
    }

    fn handle_session_command(&self) {
        let session = self.session();
        let stats = session.get_session_stats();
        let sm = self.session_manager();
        let session_name = sm.get_session_name();
        let entries = sm.get_entries();
        let cache_waste = crate::core::cache_stats::compute_cache_waste(
            &entries,
            Arc::new(super::events::RuntimePrices(session.model_runtime())),
        );
        let usage_breakdown = crate::core::usage_totals::get_usage_cost_breakdown(&entries);
        let cache_warming_status = session.cache_warming_status();
        let cache_warming_mode = self.settings_manager().get_cache_warming_mode();
        let model = session.model();
        let selected_model_key = format!(
            "{}/{}",
            model.as_ref().map(|m| m.provider.to_string()).unwrap_or_else(|| "undefined".to_string()),
            model.as_ref().map(|m| m.id.clone()).unwrap_or_else(|| "undefined".to_string())
        );
        let render_info = move || -> String {
            let t = theme();
            let dim = |s: &str| t.fg(&ThemeColor::Dim, s);
            let mut info = format!("{}\n\n", t.bold("Session Info"));
            if let Some(name) = &session_name {
                info += &format!("{} {name}\n", dim("Name:"));
            }
            info += &format!("{} {}\n", dim("File:"), stats.session_file.clone().unwrap_or_else(|| "In-memory".to_string()));
            info += &format!("{} {}\n\n", dim("ID:"), stats.session_id);
            info += &format!("{}\n", t.bold("Messages"));
            info += &format!("{} {}\n", dim("Total:"), stats.total_messages);
            info += &format!("{} {}\n", dim("User:"), stats.user_messages);
            info += &format!("{} {}\n", dim("Assistant:"), stats.assistant_messages);
            info += &format!("{} {} calls, {} results\n\n", dim("Tools:"), stats.tool_calls, stats.tool_results);
            info += &format!("{}\n", t.bold("Tokens"));
            let (input, cache_read, cache_write) = (stats.tokens.input, stats.tokens.cache_read, stats.tokens.cache_write);
            let prompt_tokens = input + cache_read + cache_write;
            info += &format!("{} {}\n", dim("Input:"), locale_int(prompt_tokens));
            if prompt_tokens > 0.0 && (cache_read > 0.0 || cache_write > 0.0) {
                let hit_rate = dim(&format!("({}%)", js_to_fixed(cache_read / prompt_tokens * 100.0, 1)));
                info += &format!("  {} {} {hit_rate}\n", dim("Cached:"), locale_int(cache_read));
                let written = if cache_write > 0.0 {
                    format!(" {}", dim(&format!("({} written to cache)", locale_int(cache_write))))
                } else {
                    String::new()
                };
                info += &format!("  {} {}{written}\n", dim("Uncached:"), locale_int(input + cache_write));
            }
            info += &format!("{} {}\n", dim("Output:"), locale_int(stats.tokens.output));
            info += &format!("{} {}\n", dim("Total:"), locale_int(stats.tokens.total as f64));

            info += &format!("\n{}\n", t.bold("Cache Warming"));
            info += &format!("{} {}\n", dim("Mode:"), cache_warming_mode.as_str());
            let status_text = match &cache_warming_status {
                Some(status) => crate::core::cache_warmer::format_cache_warming_status(status, None),
                None => "Inactive (cache warming unavailable)".to_string(),
            };
            info += &format!("{} {status_text}\n", dim("Status:"));
            if let Some(decision) = cache_warming_status.as_ref().and_then(|s| s.decision.as_ref())
                && decision.economics_available
            {
                info += &format!("{} ${}\n", dim("Cache miss penalty:"), js_to_fixed(decision.miss_cost, 3));
                info += &format!("{} ${}\n", dim("Refresh cost:"), js_to_fixed(decision.warm_cost, 3));
            }

            if stats.cost > 0.0 || cache_waste.missed_tokens > 0 {
                info += &format!("\n{}\n", t.bold("Cost"));
                info += &format!("{} ${}", dim("Total:"), js_to_fixed(stats.cost, 3));
                if usage_breakdown.len() > 1 || usage_breakdown.first().map(|e| e.key.as_str()) != Some(selected_model_key.as_str()) {
                    for entry in &usage_breakdown {
                        info += &format!(
                            "\n  {} ${} {}",
                            dim(&format!("{}:", entry.key)),
                            js_to_fixed(entry.cost, 3),
                            dim(&format!("({} tokens)", format_tokens(entry.tokens as i64)))
                        );
                    }
                }
                if cache_waste.missed_tokens > 0 {
                    let miss_label = if cache_waste.miss_count == 1 {
                        "1 miss".to_string()
                    } else {
                        format!("{} misses", cache_waste.miss_count)
                    };
                    let detail = format!("{} tokens, {miss_label}", locale_int(cache_waste.missed_tokens as f64));
                    if cache_waste.missed_cost >= 0.0001 {
                        info += &format!("\n{} ${} {}", dim("Cache Re-billed:"), js_to_fixed(cache_waste.missed_cost, 3), dim(&format!("({detail})")));
                    } else {
                        info += &format!("\n{} {detail}", dim("Cache Re-billed:"));
                    }
                }
            }
            info
        };
        self.add_spacer();
        self.add_text(&self.chat_container, Arc::new(render_info), 1, 0);
        self.request_render();
    }

    fn handle_changelog_command(&self) {
        use crate::utils::changelog::{get_changelog_path, normalize_changelog_links, parse_changelog};
        let mut entries = parse_changelog(&get_changelog_path());
        let markdown = if entries.is_empty() {
            "No changelog entries found.".to_string()
        } else {
            entries.reverse();
            entries.iter().map(|e| normalize_changelog_links(&e.content, &e.into())).collect::<Vec<_>>().join("\n\n")
        };
        self.add_spacer();
        self.chat_container.add_child(Arc::new(DynamicBorder::new(None)));
        self.add_text(&self.chat_container, Arc::new(|| theme().bold(&theme().fg(&ThemeColor::Accent, "What's New"))), 1, 0);
        self.add_spacer();
        self.chat_container
            .add_child(Arc::new(Markdown::new(markdown, 1, 1, self.get_markdown_theme_with_settings(), None, None)));
        self.chat_container.add_child(Arc::new(DynamicBorder::new(None)));
        self.request_render();
    }

    fn handle_hotkeys_command(&self) {
        let k = |action: &str| key_display_text(action.to_string());
        let new_line_note = if cfg!(windows) { " (Ctrl+Enter on Windows Terminal)" } else { "" };
        let mut hotkeys = format!(
            "
**Navigation**
| Key | Action |
|-----|--------|
| `{}` / `{}` / `{}` / `{}` | Move cursor / browse history |
| `{}` / `{}` | Move by word |
| `{}` | Start of line |
| `{}` | End of line |
| `{}` | Jump forward to character |
| `{}` | Jump backward to character |
| `{}` / `{}` | Scroll by page |

**Editing**
| Key | Action |
|-----|--------|
| `{}` | Send message |
| `{}` | New line{new_line_note} |
| `{}` | Delete word backwards |
| `{}` | Delete word forwards |
| `{}` | Delete to start of line |
| `{}` | Delete to end of line |
| `{}` | Paste the most-recently-deleted text |
| `{}` | Cycle through the deleted text after pasting |
| `{}` | Undo |

**Other**
| Key | Action |
|-----|--------|
| `{}` | Path completion / accept autocomplete |
| `{}` | Cancel autocomplete / abort streaming |
| `{}` | Clear editor (first) / exit (second) |
| `{}` | Exit (when editor is empty) |
| `{}` | Suspend to background |
| `{}` | Cycle thinking level |
| `{}` / `{}` | Cycle models |
| `{}` | Open model selector |
| `{}` | Toggle tool output expansion |
| `{}` | Toggle thinking block visibility |
| `{}` | Edit message in external editor |
| `{}` | Copy selection or last assistant message |
| `{}` | Queue follow-up message |
| `{}` | Restore queued messages |
| `{}` | Paste files on macOS, images, or text from clipboard |
| `/` | Slash commands |
| `!` | Run bash command |
| `!!` | Run bash command (excluded from context) |
",
            k("tui.editor.cursorUp"),
            k("tui.editor.cursorDown"),
            k("tui.editor.cursorLeft"),
            k("tui.editor.cursorRight"),
            k("tui.editor.cursorWordLeft"),
            k("tui.editor.cursorWordRight"),
            k("tui.editor.cursorLineStart"),
            k("tui.editor.cursorLineEnd"),
            k("tui.editor.jumpForward"),
            k("tui.editor.jumpBackward"),
            k("tui.editor.pageUp"),
            k("tui.editor.pageDown"),
            k("tui.input.submit"),
            k("tui.input.newLine"),
            k("tui.editor.deleteWordBackward"),
            k("tui.editor.deleteWordForward"),
            k("tui.editor.deleteToLineStart"),
            k("tui.editor.deleteToLineEnd"),
            k("tui.editor.yank"),
            k("tui.editor.yankPop"),
            k("tui.editor.undo"),
            k("tui.input.tab"),
            k("app.interrupt"),
            k("app.clear"),
            k("app.exit"),
            k("app.suspend"),
            k("app.thinking.cycle"),
            k("app.model.cycleForward"),
            k("app.model.cycleBackward"),
            k("app.model.select"),
            k("app.tools.expand"),
            k("app.thinking.toggle"),
            k("app.editor.external"),
            k("app.message.copy"),
            k("app.message.followUp"),
            k("app.message.dequeue"),
            k("app.clipboard.pasteImage"),
        );

        // Add extension-registered shortcuts
        let shortcuts = self.session().extension_runner().get_shortcuts(&self.keybindings.get_effective_config());
        if !shortcuts.is_empty() {
            hotkeys += "\n**Extensions**\n| Key | Action |\n|-----|--------|\n";
            for (key, shortcut) in &shortcuts {
                let description = shortcut.description.clone().unwrap_or_else(|| shortcut.extension_path.clone());
                let key_display = format_key_text(key, Some(&KeyTextFormatOptions { capitalize: Some(true) }));
                hotkeys += &format!("| `{key_display}` | {description} |\n");
            }
        }

        self.add_spacer();
        self.chat_container.add_child(Arc::new(DynamicBorder::new(None)));
        self.add_text(
            &self.chat_container,
            Arc::new(|| theme().bold(&theme().fg(&ThemeColor::Accent, "Keyboard Shortcuts"))),
            1,
            0,
        );
        self.add_spacer();
        self.chat_container.add_child(Arc::new(Markdown::new(
            hotkeys.trim().to_string(),
            1,
            1,
            self.get_markdown_theme_with_settings(),
            None,
            None,
        )));
        self.chat_container.add_child(Arc::new(DynamicBorder::new(None)));
        self.request_render();
    }

    pub(crate) async fn handle_clear_command(self: Arc<Self>) {
        self.clear_status_indicator(None);
        match self.runtime_host.new_session(None).await {
            Ok(result) => {
                if result.cancelled {
                    return;
                }
                self.add_spacer();
                self.add_text(
                    &self.chat_container,
                    Arc::new(|| theme().fg(&ThemeColor::Accent, "✓ New session started")),
                    1,
                    1,
                );
                self.request_render();
            }
            Err(error) => self.handle_fatal_runtime_error("Failed to create session", &error),
        }
    }

    pub(crate) fn handle_debug_command(&self) {
        let ui = self.ui();
        let width = ui.terminal().columns();
        let height = ui.terminal().rows();
        let all_lines = ui.render_root(width);
        let debug_log_path = crate::config::get_debug_log_path();
        let mut lines = vec![
            format!("Debug output at {}", chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
            format!("Terminal: {width}x{height}"),
            format!("Total lines: {}", all_lines.len()),
            String::new(),
            "=== All rendered lines with visible widths ===".to_string(),
        ];
        for (index, line) in all_lines.iter().enumerate() {
            let vw = pi_tui::utils::visible_width(line);
            lines.push(format!("[{index}] (w={vw}) {}", serde_json::to_string(line).unwrap_or_default()));
        }
        lines.push(String::new());
        lines.push("=== Agent messages (JSONL) ===".to_string());
        for message in self.session().messages() {
            lines.push(serde_json::to_string(&message).unwrap_or_default());
        }
        lines.push(String::new());
        if let Some(parent) = std::path::Path::new(&debug_log_path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&debug_log_path, lines.join("\n"));
        self.add_spacer();
        self.add_text(
            &self.chat_container,
            Arc::new(move || {
                format!("{}\n{}", theme().fg(&ThemeColor::Accent, "✓ Debug log written"), theme().fg(&ThemeColor::Muted, &debug_log_path))
            }),
            1,
            1,
        );
        self.request_render();
    }

    fn handle_armin_says_hi(&self) {
        if play_easter_egg_3d(self, EasterEgg3d::Armin {}) {
            return;
        }
        self.add_spacer();
        self.chat_container
            .add_child(Arc::new(crate::modes::interactive::components::armin::ArminComponent::new(self.ui())));
        self.request_render();
    }

    fn handle_demented_delves(&self) {
        self.add_spacer();
        self.chat_container.add_child(Arc::new(
            crate::modes::interactive::components::earendil_announcement::EarendilAnnouncementComponent::new(),
        ));
        self.request_render();
    }

    async fn handle_bash_command(self: Arc<Self>, command: &str, exclude_from_context: bool) {
        let session = self.session();
        let runner = session.extension_runner();
        // Emit user_bash event to let extensions intercept
        let event_result = match runner
            .emit_user_bash(&UserBashEvent {
                command: command.to_string(),
                exclude_from_context,
                cwd: self.session_manager().get_cwd(),
            })
            .await
        {
            Ok(result) => result,
            // The extension runner already reported the error. Do not fall back to local execution.
            Err(_) => return,
        };

        let pad = self.output_pad();
        let place = |this: &Inner, component: &Arc<BashExecutionComponent>, deferred: bool| {
            if deferred {
                this.pending_messages_container.add_child(component.clone());
                this.state.lock().pending_bash_components.push(component.clone());
            } else {
                this.chat_container.add_child(component.clone());
            }
        };

        let operations = match event_result {
            // If extension returned a full result, use it directly
            Some(UserBashEventResult::Result(result)) => {
                let component =
                    Arc::new(BashExecutionComponent::new(command, self.ui(), Some(exclude_from_context), Some(pad)));
                place(&self, &component, session.is_streaming());
                if !result.output.is_empty() {
                    component.append_output(&result.output);
                }
                let truncation = result.truncated.then(|| super::events::truncated_marker(&result.output));
                component.set_complete(result.exit_code, result.cancelled, truncation.as_ref(), result.full_output_path.as_deref());
                session.record_bash_result(
                    command,
                    &result,
                    Some(&crate::core::agent_session::AgentSessionRecordBashResultOptions {
                        exclude_from_context: Some(exclude_from_context),
                    }),
                );
                self.request_render();
                return;
            }
            Some(UserBashEventResult::Operations(operations)) => Some(operations),
            None => None,
        };

        // Normal execution path (possibly with custom operations)
        let deferred = session.is_streaming();
        let component = Arc::new(BashExecutionComponent::new(command, self.ui(), Some(exclude_from_context), Some(pad)));
        self.state.lock().bash_component = Some(component.clone());
        place(&self, &component, deferred);
        self.request_render();

        let weak = self.weak.clone();
        let on_chunk: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |chunk: String| {
            if let Some(this) = weak.upgrade() {
                // Runs on the command's reader thread: update the row as one step for the renderer.
                let _ui = pi_tui::ui_lock::lock_ui();
                let component = this.state.lock().bash_component.clone();
                if let Some(component) = component {
                    component.append_output(&chunk);
                    this.request_render();
                }
            }
        });
        let result = session
            .execute_bash(
                command,
                Some(on_chunk),
                Some(&crate::core::agent_session::AgentSessionExecuteBashOptions {
                    exclude_from_context: Some(exclude_from_context),
                    id: None,
                    operations,
                }),
            )
            .await;
        let component = self.state.lock().bash_component.take();
        match result {
            Ok(result) => {
                if let Some(component) = component {
                    let truncation = result.truncated.then(|| super::events::truncated_marker(&result.output));
                    component.set_complete(result.exit_code, result.cancelled, truncation.as_ref(), result.full_output_path.as_deref());
                }
            }
            Err(error) => {
                if let Some(component) = component {
                    component.set_complete(None, false, None, None);
                }
                self.show_error(&format!("Bash command failed: {error}"));
            }
        }
        self.request_render();
    }

    async fn handle_compact_command(self: Arc<Self>, custom_instructions: Option<String>) {
        self.clear_status_indicator(None);
        // Errors are emitted as events
        let _ = self.session().compact(custom_instructions.as_deref()).await;
    }

    pub(crate) async fn handle_resume_session(
        &self,
        session_path: &str,
        with_session: Option<WithSessionFn>,
    ) -> crate::core::extensions::types::SessionReplaceOutcome {
        use crate::core::extensions::types::SessionReplaceOutcome;
        self.clear_status_indicator(None);
        let weak = self.weak.clone();
        let trust_factory: Arc<dyn Fn(String) -> crate::core::extensions::types::ProjectTrustContext + Send + Sync> =
            Arc::new(move |cwd: String| match weak.upgrade() {
                Some(this) => this.create_project_trust_context(cwd),
                None => crate::core::extensions::types::ProjectTrustContext {
                    cwd,
                    mode: crate::core::extensions::types::ExtensionMode::Tui,
                    has_ui: false,
                    ui: Arc::new(crate::core::extensions::runner::NoOpUIContext),
                },
            });
        let options = AgentSessionRuntimeSwitchSessionOptions {
            cwd_override: None,
            with_session: with_session.clone(),
            project_trust_context_factory: Some(trust_factory.clone()),
        };
        match self.runtime_host.switch_session(session_path, Some(&options)).await {
            Ok(result) => {
                if !result.cancelled {
                    self.show_status("Resumed session");
                }
                SessionReplaceOutcome { cancelled: result.cancelled }
            }
            Err(error) => {
                if let Some(missing) = error.downcast_ref::<MissingSessionCwdError>() {
                    let Some(selected) = self.prompt_for_missing_session_cwd(missing).await else {
                        self.show_status("Resume cancelled");
                        return SessionReplaceOutcome { cancelled: true };
                    };
                    let options = AgentSessionRuntimeSwitchSessionOptions {
                        cwd_override: Some(selected),
                        with_session,
                        project_trust_context_factory: Some(trust_factory),
                    };
                    return match self.runtime_host.switch_session(session_path, Some(&options)).await {
                        Ok(result) => {
                            if !result.cancelled {
                                self.show_status("Resumed session in current cwd");
                            }
                            SessionReplaceOutcome { cancelled: result.cancelled }
                        }
                        Err(error) => self.handle_fatal_runtime_error("Failed to resume session", &error),
                    };
                }
                self.handle_fatal_runtime_error("Failed to resume session", &error)
            }
        }
    }

    async fn handle_clone_command(self: Arc<Self>) {
        let Some(leaf_id) = self.session_manager().get_leaf_id() else {
            self.show_status("Nothing to clone yet");
            return;
        };
        let options = AgentSessionRuntimeForkOptions {
            position: Some(crate::core::agent_session_runtime::AgentSessionRuntimeForkOptionsPosition::At),
            with_session: None,
        };
        match self.runtime_host.fork(&leaf_id, Some(&options)).await {
            Ok(result) => {
                if result.cancelled {
                    self.request_render();
                    return;
                }
                self.editor().set_text("");
                self.show_status("Cloned to new session");
            }
            Err(error) => self.show_error(&error.to_string()),
        }
    }
}

/// The path argument of `/export` or `/import` (quoted or up to the first whitespace).
pub(crate) fn get_path_command_argument(text: &str, command: &str) -> Option<String> {
    if text == command {
        return None;
    }
    let args = text.strip_prefix(&format!("{command} "))?.trim_start();
    let first = args.chars().next()?;
    if first == '"' || first == '\'' {
        let rest = &args[1..];
        let close = rest.find(first)?;
        return Some(rest[..close].to_string());
    }
    match args.find(char::is_whitespace) {
        Some(index) => Some(args[..index].to_string()),
        None => Some(args.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_path_command_arguments() {
        assert_eq!(get_path_command_argument("/export", "/export"), None);
        assert_eq!(get_path_command_argument("/export out.html", "/export").as_deref(), Some("out.html"));
        assert_eq!(get_path_command_argument("/export \"my file.html\" x", "/export").as_deref(), Some("my file.html"));
        assert_eq!(get_path_command_argument("/export 'a b.jsonl'", "/export").as_deref(), Some("a b.jsonl"));
        assert_eq!(get_path_command_argument("/export \"unclosed", "/export"), None);
        assert_eq!(get_path_command_argument("/import a.jsonl rest", "/import").as_deref(), Some("a.jsonl"));
        assert_eq!(get_path_command_argument("/exportx", "/export"), None);
    }

    #[test]
    fn formats_locale_integers() {
        assert_eq!(locale_int(0.0), "0");
        assert_eq!(locale_int(999.0), "999");
        assert_eq!(locale_int(1234.0), "1,234");
        assert_eq!(locale_int(1234567.0), "1,234,567");
    }
}
