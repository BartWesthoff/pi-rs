//! Ports of the remaining interactive component tests:
//! `tool-execution-component.test.ts` (fake clock, renderer slots, compact reads),
//! `auth-url-copy.test.ts` (fake clipboard), `first-time-setup.test.ts`,
//! `first-time-setup-fork.test.ts` and the config-selector cases of `package-command-paths.test.ts`.

// Tests serialize the process-wide fake clock and clipboard with a guard held across awaits;
// each test runs on its own current-thread runtime, so the guard cannot block an executor.
#![allow(clippy::await_holding_lock)]

use std::sync::Arc;

use parking_lot::{Mutex, MutexGuard};
use pi_agent::types::AgentToolResult;
use pi_coding_agent::config::get_readme_path;
use pi_coding_agent::core::extensions::types::{ToolDefinition, ToolRenderers};
use pi_coding_agent::core::keybindings::KeybindingsManager;
use pi_coding_agent::core::tools::bash::{
    BashOperations, BashOperationsResult, BashToolOptions, create_bash_tool_definition,
};
use pi_coding_agent::core::tools::read::{create_read_tool, create_read_tool_definition};
use pi_coding_agent::core::tools::render_utils::set_tool_timer_clock;
use pi_coding_agent::core::tools::renderers::with_built_in_renderers;
use pi_coding_agent::core::tools::write::create_write_tool_definition;
use pi_coding_agent::modes::interactive::components::tool_execution::{
    ToolExecutionComponent, ToolExecutionComponentToolDefinition, ToolExecutionComponentUpdateResultResult,
};
use pi_coding_agent::modes::interactive::theme::theme::{ThemeColor, init_theme, theme};
use pi_coding_agent::utils::ansi::strip_ansi;
use pi_tui::components::text::Text;
use pi_tui::event_loop::EventLoop;
use pi_tui::testing::VirtualTerminal;
use pi_tui::tui::{Component, ComponentRef, Tui, TuiOptions};
use serde_json::{Value, json};

static SERIAL: Mutex<()> = Mutex::new(());

fn setup() -> MutexGuard<'static, ()> {
    let guard = SERIAL.lock();
    init_theme(Some("dark"), Some(false));
    pi_tui::keybindings::set_keybindings(KeybindingsManager::new(None, None).tui_manager());
    guard
}

fn fake_tui() -> Arc<Tui> {
    Tui::main_screen_with(
        Arc::new(VirtualTerminal::new(120, 24)),
        TuiOptions { event_loop: Some(EventLoop::new_manual()), ..Default::default() },
    )
}

fn cwd() -> String {
    std::env::current_dir().unwrap().to_string_lossy().into_owned()
}

fn plain(component: &dyn Component, width: usize) -> String {
    strip_ansi(&component.render(width).join("\n")).unwrap()
}

fn result(value: Value) -> ToolExecutionComponentUpdateResultResult {
    serde_json::from_value(value).unwrap()
}

fn text_result(text: &str, is_error: bool) -> ToolExecutionComponentUpdateResultResult {
    result(json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }))
}

fn component(
    name: &str,
    args: Value,
    definition: Option<ToolExecutionComponentToolDefinition>,
) -> ToolExecutionComponent {
    ToolExecutionComponent::new(name, "tool-1", args, None, definition.as_ref(), fake_tui(), &cwd())
}

fn definition(def: ToolDefinition) -> Option<ToolExecutionComponentToolDefinition> {
    Some(ToolExecutionComponentToolDefinition::ToolDefinition(def))
}

fn renderers(r: Option<ToolRenderers>) -> Option<ToolExecutionComponentToolDefinition> {
    r.map(ToolExecutionComponentToolDefinition::ToolRenderers)
}

fn base_definition(name: &str) -> ToolDefinition {
    ToolDefinition::new(name, name, "custom tool", json!({}), |_id, _params, _signal, _update, _ctx| async {
        Ok(AgentToolResult::text("ok", Some(json!({}))))
    })
}

fn text_renderer(text: &'static str) -> ComponentRef {
    Arc::new(Text::new(text, 0, 0, None))
}

fn with_call(mut def: ToolDefinition, text: &'static str) -> ToolDefinition {
    def.render_call = Some(Arc::new(move |_, _, _| text_renderer(text)));
    def
}

fn with_result(mut def: ToolDefinition, text: &'static str) -> ToolDefinition {
    def.render_result = Some(Arc::new(move |_, _, _, _| text_renderer(text)));
    def
}

fn bash_definition() -> ToolDefinition {
    create_bash_tool_definition(
        &cwd(),
        Some(&BashToolOptions { expose_session_environment: Some(false), ..Default::default() }),
    )
}

/// TS `vi.useFakeTimers(); vi.setSystemTime(0)`: a settable clock for tool timers.
struct FakeClock(Arc<Mutex<i64>>);

impl FakeClock {
    fn install() -> FakeClock {
        let now = Arc::new(Mutex::new(0));
        let reader = now.clone();
        set_tool_timer_clock(Some(Arc::new(move || *reader.lock())));
        FakeClock(now)
    }

    fn advance(&self, ms: i64) {
        *self.0.lock() += ms;
    }
}

impl Drop for FakeClock {
    fn drop(&mut self) {
        set_tool_timer_clock(None);
    }
}

// ---------------------------------------------------------------------------------------------
// tool-execution-component.test.ts
// ---------------------------------------------------------------------------------------------

/// Small 2x2 blue JPEG image.
const TINY_JPEG: &str = "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAMCAgMCAgMDAwMEAwMEBQgFBQQEBQoHBwYIDAoMDAsKCwsNDhIQDQ4RDgsLEBYQERMUFRUVDA8XGBYUGBIUFRT/2wBDAQMEBAUEBQkFBQkUDQsNFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBT/wAARCAACAAIDAREAAhEBAxEB/8QAFAABAAAAAAAAAAAAAAAAAAAACf/EABQQAQAAAAAAAAAAAAAAAAAAAAD/xAAVAQEBAAAAAAAAAAAAAAAAAAAGCf/EABQRAQAAAAAAAAAAAAAAAAAAAAD/2gAMAwEAAhEDEQA/AD3VTB3/2Q==";

// Issue #10292: the component loads the PNG transcoder itself, so this works in any TUI host.
// Issue #8577: a replaced partial image must not resurface.
#[test]
fn converts_non_png_tool_images_once_the_transcoder_loads() {
    let _g = setup();
    pi_tui::terminal_image::set_capabilities(pi_tui::terminal_image::TerminalCapabilities {
        images: Some(pi_tui::terminal_image::ImageProtocol::Kitty),
        true_color: true,
        hyperlinks: true,
    });
    let c = ToolExecutionComponent::new("tool", "id", json!({}), None, None, fake_tui(), &cwd());
    c.update_result(
        &result(json!({ "content": [{ "type": "image", "data": "cGFydGlhbA==", "mimeType": "image/jpeg" }], "isError": false })),
        Some(true),
    );
    c.update_result(
        &result(json!({ "content": [{ "type": "image", "data": TINY_JPEG, "mimeType": "image/jpeg" }], "isError": false })),
        None,
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !c.render(120).join("\n").contains(";iVBORw0KGgo") {
        assert!(std::time::Instant::now() < deadline, "the image was not converted:\n{}", c.render(120).join("\n"));
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let rendered = c.render(120).join("\n");
    assert!(!rendered.contains("cGFydGlhbA=="));

    // Invalidation reuses the converted image, so the Kitty image ID stays the same.
    c.invalidate();
    assert_eq!(c.render(120).join("\n"), rendered);
    pi_tui::terminal_image::reset_capabilities_cache();
}

#[test]
fn uses_built_in_rendering_for_built_in_overrides_without_custom_renderers() {
    let _g = setup();
    let c = component(
        "edit",
        json!({ "path": "README.md", "oldText": "before", "newText": "after" }),
        renderers(with_built_in_renderers("edit", Some(base_definition("edit").renderers()))),
    );
    c.update_result(
        &result(json!({ "content": [], "details": { "diff": "+1 after", "firstChangedLine": 1 }, "isError": false })),
        None,
    );
    let rendered = plain(&c, 120);
    assert!(rendered.contains("edit"), "{rendered}");
    assert!(rendered.contains("README.md"), "{rendered}");
    assert!(!rendered.contains(":1"), "{rendered}");
}

#[test]
fn preserves_legacy_file_path_rendering_for_built_in_tools() {
    let _g = setup();
    let c = component("read", json!({ "file_path": "README.md" }), None);
    let rendered = plain(&c, 120);
    assert!(rendered.contains("read") && rendered.contains("README.md"), "{rendered}");
}

#[tokio::test]
async fn bash_execute_emits_an_initial_empty_partial_update_before_output_arrives() {
    let updates: Arc<Mutex<Vec<Value>>> = Arc::default();
    let operations = BashOperations {
        exec: Arc::new(|_command, _cwd, _options| {
            Box::pin(async {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                Ok(BashOperationsResult { exit_code: Some(0) })
            })
        }),
    };
    let tool = create_bash_tool_definition(
        &cwd(),
        Some(&BashToolOptions { operations: Some(operations), expose_session_environment: Some(false), ..Default::default() }),
    );
    let sink = updates.clone();
    let on_update: pi_agent::types::AgentToolUpdateCallback =
        Arc::new(move |update: AgentToolResult| sink.lock().push(serde_json::to_value(&update).unwrap()));
    let promise = (tool.execute)("tool-bash-1".into(), json!({ "command": "sleep 10" }), None, Some(on_update), None);
    let handle = tokio::spawn(promise);
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    assert_eq!(updates.lock().clone(), vec![json!({ "content": [] })]);
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn bash_renderer_does_not_duplicate_final_full_output_truncation_details() {
    let _g = setup();
    let operations = BashOperations {
        exec: Arc::new(|_command, _cwd, options| {
            Box::pin(async move {
                for i in 1..=4000 {
                    (options.on_data)(format!("line-{i:04}\n").into_bytes());
                }
                Ok(BashOperationsResult { exit_code: Some(0) })
            })
        }),
    };
    let tool = create_bash_tool_definition(
        &cwd(),
        Some(&BashToolOptions { operations: Some(operations), expose_session_environment: Some(false), ..Default::default() }),
    );
    let output = (tool.execute)("tool-bash-1b".into(), json!({ "command": "generate output" }), None, None, None)
        .await
        .unwrap();
    let c = component("bash", json!({ "command": "generate output" }), definition(tool));
    c.set_expanded(true);
    let mut value = serde_json::to_value(&output).unwrap();
    value["isError"] = json!(false);
    c.update_result(&result(value), Some(false));

    let rendered = plain(&c, 200);
    assert_eq!(rendered.matches("Full output:").count(), 1, "{rendered}");
    assert!(regex::Regex::new(r"line-4000[^\n]*\n[^\S\n]*\n \[Full output:").unwrap().is_match(&rendered), "{rendered}");
    assert!(
        !regex::Regex::new(r"line-4000[^\n]*\n[^\S\n]*\n[^\S\n]*\n \[Full output:").unwrap().is_match(&rendered),
        "{rendered}"
    );
    assert!(rendered.contains("Truncated: showing 2000 of 4000 lines"), "{rendered}");
    assert!(!rendered.contains("[Showing lines 2001-4000 of 4000. Full output:"), "{rendered}");
}

// Issue #9628: keep short durations precise and make long shell durations readable.
#[test]
fn bash_renderer_formats_running_and_completed_durations() {
    let _g = setup();
    for (ms, formatted) in [
        (0, "0.0s"),
        (4_200, "4.2s"),
        (59_900, "59.9s"),
        (59_999, "60.0s"),
        (60_000, "1m 0s"),
        (90_900, "1m 30s"),
        (1_592_200, "26m 32s"),
        (3_599_999, "59m 59s"),
        (3_600_000, "1h 0m 0s"),
        (7_384_900, "2h 3m 4s"),
    ] {
        let clock = FakeClock::install();
        let c = component("bash", json!({ "command": "long-running-command" }), definition(bash_definition()));
        c.mark_execution_started();
        c.update_result(&result(json!({ "content": [], "isError": false })), Some(true));

        clock.advance(ms);
        c.invalidate();
        let running = plain(&c, 120);

        c.update_result(&result(json!({ "content": [], "isError": false })), Some(false));
        let completed = plain(&c, 120);

        clock.advance(1_000);
        c.invalidate();
        assert_eq!(plain(&c, 120), completed, "{ms}");
        assert!(running.contains(&format!("Elapsed {formatted}")), "{ms}: {running}");
        assert!(completed.contains(&format!("Took {formatted}")), "{ms}: {completed}");
    }
}

// #10549
#[test]
fn bash_renderer_shows_a_results_recorded_duration_also_when_restored_without_a_live_start() {
    let _g = setup();
    let render = |live: bool| -> String {
        let clock = FakeClock::install();
        let c = component("bash", json!({ "command": "sleep 4" }), definition(bash_definition()));
        if live {
            c.mark_execution_started();
            c.update_result(&result(json!({ "content": [], "isError": false })), Some(true));
            // The wall clock jumps; the recorded duration does not.
            clock.advance(3_600_000);
        }
        c.update_result(&result(json!({ "content": [], "isError": false, "durationMs": 4_200 })), Some(false));
        plain(&c, 120)
    };
    assert!(render(true).contains("Took 4.2s"));
    assert!(render(false).contains("Took 4.2s"));
}

#[test]
fn does_not_duplicate_built_in_headers_when_passed_the_active_built_in_definition() {
    let _g = setup();
    let c = component("read", json!({ "path": "README.md" }), definition(create_read_tool_definition(&cwd(), None)));
    c.update_result(&text_result("hello", false), Some(false));
    let rendered = plain(&c, 120);
    assert_eq!(regex::Regex::new(r"\bread\b").unwrap().find_iter(&rendered).count(), 1, "{rendered}");
}

// Issue #9996: strict tool schemas make models send null for omitted optional fields.
#[test]
fn renders_read_calls_with_null_offset_and_limit_as_full_file_reads() {
    let _g = setup();
    let c = component(
        "read",
        json!({ "path": "src/example.ts", "offset": null, "limit": null }),
        definition(create_read_tool_definition(&cwd(), None)),
    );
    let rendered = plain(&c, 120);
    assert!(rendered.contains("read src/example.ts"), "{rendered}");
    assert!(!rendered.contains("src/example.ts:"), "{rendered}");
}

#[test]
fn inherits_missing_built_in_result_renderer_slot() {
    let _g = setup();
    let def = with_call(base_definition("read"), "override call");
    let c = component("read", json!({ "path": "notes.txt" }), renderers(with_built_in_renderers("read", Some(def.renderers()))));
    c.update_result(&text_result("hello", false), Some(false));
    c.set_expanded(true);
    let rendered = plain(&c, 120);
    assert!(rendered.contains("override call") && rendered.contains("hello"), "{rendered}");
}

#[test]
fn inherits_missing_built_in_call_renderer_slot() {
    let _g = setup();
    let def = with_result(base_definition("read"), "override result");
    let c = component("read", json!({ "path": "README.md" }), renderers(with_built_in_renderers("read", Some(def.renderers()))));
    c.update_result(&text_result("hello", false), Some(false));
    let rendered = plain(&c, 120);
    assert!(rendered.contains("read") && rendered.contains("README.md"), "{rendered}");
    assert!(rendered.contains("override result"), "{rendered}");
}

#[test]
fn uses_custom_renderers_for_overrides_that_reuse_built_in_definition_parameters() {
    let _g = setup();
    let def = with_result(with_call(create_read_tool_definition(&cwd(), None), "override call"), "override result");
    let c = component("read", json!({ "path": "README.md" }), definition(def));
    c.update_result(&text_result("hello", false), Some(false));
    let rendered = plain(&c, 120);
    assert!(rendered.contains("override call") && rendered.contains("override result"), "{rendered}");
    assert!(!rendered.contains("read README.md"), "{rendered}");
}

#[test]
fn uses_custom_renderers_for_overrides_that_reuse_wrapped_built_in_tool_parameters() {
    let _g = setup();
    let built_in = create_read_tool(&cwd(), None);
    let mut def = base_definition("read");
    def.parameters = built_in.parameters().clone();
    let def = with_result(with_call(def, "wrapped override call"), "wrapped override result");
    let c = component("read", json!({ "path": "README.md" }), definition(def));
    c.update_result(&text_result("hello", false), Some(false));
    let rendered = plain(&c, 120);
    assert!(rendered.contains("wrapped override call") && rendered.contains("wrapped override result"), "{rendered}");
}

#[test]
fn trims_trailing_blank_display_lines_from_write_previews() {
    let _g = setup();
    let c = component(
        "write",
        json!({ "path": "README.md", "content": "one\ntwo\n" }),
        definition(create_write_tool_definition(&cwd(), None)),
    );
    let rendered = plain(&c, 120);
    assert!(rendered.contains("one") && rendered.contains("two"), "{rendered}");
    assert!(!rendered.contains("two\n\n"), "{rendered}");
}

#[test]
fn trims_trailing_blank_display_lines_from_read_results() {
    let _g = setup();
    let c = component("read", json!({ "path": "notes.txt" }), definition(create_read_tool_definition(&cwd(), None)));
    c.update_result(&text_result("one\ntwo\n", false), Some(false));
    c.set_expanded(true);
    let rendered = plain(&c, 120);
    assert!(rendered.contains("one") && rendered.contains("two"), "{rendered}");
    assert!(!rendered.contains("two\n\n"), "{rendered}");
}

#[test]
fn does_not_syntax_highlight_read_errors_based_on_the_requested_file_path() {
    let _g = setup();
    let c = component(
        "read",
        json!({ "path": "config.exs", "offset": 120, "limit": 130 }),
        definition(create_read_tool_definition(&cwd(), None)),
    );
    let error = "Offset 120 is beyond end of file (96 lines total)";
    c.update_result(&text_result(error, true), Some(false));
    let rendered = c.render(120).join("\n");
    assert!(strip_ansi(&rendered).unwrap().contains(error));
    assert!(rendered.contains(&theme().fg(&ThemeColor::ToolOutput, error)), "{rendered:?}");
}

#[test]
fn collapses_ordinary_read_results_until_expanded() {
    let _g = setup();
    let c = component("read", json!({ "path": "notes.txt" }), definition(create_read_tool_definition(&cwd(), None)));
    c.update_result(&text_result("hidden content", false), Some(false));
    let collapsed = plain(&c, 120);
    assert!(collapsed.contains("read") && collapsed.contains("notes.txt"), "{collapsed}");
    assert!(!collapsed.contains("hidden content"), "{collapsed}");
    c.set_expanded(true);
    assert!(plain(&c, 120).contains("hidden content"));
}

#[test]
fn renders_resource_reads_compactly_until_expanded() {
    let _g = setup();
    let cwd = cwd();
    let parent_agents = std::path::Path::new(&cwd).parent().unwrap().join("AGENTS.md").to_string_lossy().replace('\\', "/");
    let scenarios: Vec<(String, &str, String, &str, Option<&str>)> = vec![
        (
            format!("{cwd}/attio/SKILL.md"),
            "---\nname: attio\ndescription: CRM helper\n---\n\n# Hidden skill instructions",
            "[skill] attio".into(),
            "Hidden skill instructions",
            Some("read skill attio"),
        ),
        (format!("{cwd}/.pi/AGENTS.md"), "Hidden resource instructions", "read resource .pi/AGENTS.md".into(), "Hidden resource instructions", None),
        (
            format!("{cwd}/.pi/AGENTS.override.md"),
            "Hidden override instructions",
            "read resource .pi/AGENTS.override.md".into(),
            "Hidden override instructions",
            None,
        ),
        (
            parent_agents.clone(),
            "Hidden outside resource instructions",
            format!("read resource {parent_agents}"),
            "Hidden outside resource instructions",
            None,
        ),
        (get_readme_path(), "Hidden docs content", "read docs README.md".into(), "Hidden docs content", None),
    ];
    for (path, content, compact, hidden, absent) in scenarios {
        let c = component("read", json!({ "path": path }), definition(create_read_tool_definition(&cwd, None)));
        c.update_result(&text_result(content, false), Some(false));
        let collapsed = plain(&c, 120);
        assert!(collapsed.contains(&compact), "{path}: {collapsed}");
        assert!(!collapsed.contains(hidden), "{path}: {collapsed}");
        if let Some(absent) = absent {
            assert!(!collapsed.contains(absent), "{path}: {collapsed}");
        }
        c.set_expanded(true);
        assert!(plain(&c, 120).contains(hidden), "{path}");
    }
}

#[test]
fn shows_the_read_line_range_in_compact_reads_before_the_expand_hint() {
    let _g = setup();
    let cwd = cwd();
    for (path, compact) in [
        (format!("{cwd}/attio/SKILL.md"), "[skill] attio:120-329"),
        (get_readme_path(), "read docs README.md:120-329"),
    ] {
        let c = component(
            "read",
            json!({ "path": path, "offset": 120, "limit": 210 }),
            definition(create_read_tool_definition(&cwd, None)),
        );
        let collapsed = plain(&c, 120);
        assert!(collapsed.contains(compact), "{collapsed}");
        assert!(collapsed.find(":120-329").unwrap() < collapsed.find("to expand").unwrap(), "{collapsed}");
    }
}

// ---------------------------------------------------------------------------------------------
// auth-url-copy.test.ts
// ---------------------------------------------------------------------------------------------

mod auth_url_copy {
    use super::*;
    use pi_coding_agent::extensions::mcp::ui::{McpManagerView, McpUi};
    use pi_coding_agent::modes::interactive::components::login_dialog::LoginDialogComponent;
    use pi_coding_agent::utils::clipboard::set_copy_to_clipboard_override;
    use pi_coding_agent::utils::open_browser::set_open_browser_override;
    use std::time::{Duration, Instant};
    use tokio_util::sync::CancellationToken;

    const CTRL_X: &str = "\x18";

    fn url() -> String {
        format!("https://auth.example.invalid/authorize?{}", "x".repeat(300))
    }

    /// TS `vi.mock` of the clipboard and browser modules; restored on drop.
    struct Mocks {
        copied: Arc<Mutex<Vec<String>>>,
    }

    impl Mocks {
        fn install() -> Mocks {
            let copied: Arc<Mutex<Vec<String>>> = Arc::default();
            let sink = copied.clone();
            set_copy_to_clipboard_override(Some(Arc::new(move |text: String| {
                sink.lock().push(text);
                Box::pin(async { Ok(()) })
            })));
            set_open_browser_override(Some(Arc::new(|_| {})));
            Mocks { copied }
        }
    }

    impl Drop for Mocks {
        fn drop(&mut self) {
            set_copy_to_clipboard_override(None);
            set_open_browser_override(None);
        }
    }

    fn rendered(component: &dyn Component) -> String {
        plain(component, 80)
    }

    async fn wait_for(component: &dyn Component, text: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !rendered(component).contains(text) {
            assert!(Instant::now() < deadline, "timed out waiting for {text}:\n{}", rendered(component));
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn login_dialog_copies_the_auth_url_instead_of_typing_into_the_code_input() {
        let _g = setup();
        let mocks = Mocks::install();
        let dialog = Arc::new(LoginDialogComponent::new(fake_tui(), "test", Arc::new(|_, _| {}), None, None));
        dialog.show_auth(&url(), None);
        let pending = dialog.clone();
        tokio::spawn(async move {
            let _ = pending.show_manual_input("Paste the code:").await;
        });
        wait_for(dialog.as_ref(), "Paste the code:").await;
        assert!(rendered(dialog.as_ref()).contains("ctrl+x to copy"));

        dialog.handle_input(CTRL_X);
        wait_for(dialog.as_ref(), "Copied URL to clipboard").await;
        assert_eq!(mocks.copied.lock().clone(), vec![url()]);
    }

    #[tokio::test]
    async fn login_dialog_ignores_the_copy_key_without_an_auth_url() {
        let _g = setup();
        let mocks = Mocks::install();
        let dialog = LoginDialogComponent::new(fake_tui(), "test", Arc::new(|_, _| {}), None, None);
        dialog.show_device_code(pi_ai::auth::extension_oauth_types::OAuthDeviceCodeInfo {
            user_code: "ABCD".into(),
            verification_uri: "https://example.invalid/device".into(),
            interval_seconds: None,
            expires_in_seconds: None,
        });
        dialog.handle_input(CTRL_X);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(mocks.copied.lock().is_empty());
    }

    #[tokio::test]
    async fn mcp_sign_in_screen_copies_the_authorization_url() {
        let _g = setup();
        let mocks = Mocks::install();
        let view = McpManagerView::new(fake_tui(), theme(), Arc::new(KeybindingsManager::new(None, None)));
        let pending = view.clone();
        tokio::spawn(async move {
            let _ = pending.redirect_url("Sign in to issues", &url(), CancellationToken::new()).await;
        });
        wait_for(view.as_ref(), "ctrl+x to copy").await;

        view.handle_input(CTRL_X);
        wait_for(view.as_ref(), "Copied URL to clipboard").await;
        assert_eq!(mocks.copied.lock().clone(), vec![url()]);
    }
}

// ---------------------------------------------------------------------------------------------
// first-time-setup.test.ts (analytics settings)
// ---------------------------------------------------------------------------------------------

mod analytics_settings {
    use pi_coding_agent::core::settings_manager::SettingsManager;

    #[test]
    fn defaults_to_disabled_with_no_tracking_identifier() {
        let manager = SettingsManager::in_memory(None, None);
        assert!(!manager.get_enable_analytics());
        assert_eq!(manager.get_tracking_id(), None);
    }

    #[test]
    fn generates_a_tracking_identifier_on_opt_in() {
        let manager = SettingsManager::in_memory(None, None);
        manager.set_enable_analytics(true);
        assert!(manager.get_enable_analytics());
        let id = manager.get_tracking_id().unwrap();
        assert!(regex::Regex::new(r"^[0-9a-f-]{36}$").unwrap().is_match(&id), "{id}");
    }

    #[test]
    fn does_not_generate_a_tracking_identifier_on_opt_out() {
        let manager = SettingsManager::in_memory(None, None);
        manager.set_enable_analytics(false);
        assert!(!manager.get_enable_analytics());
        assert_eq!(manager.get_tracking_id(), None);
    }

    #[test]
    fn keeps_the_tracking_identifier_when_toggling_analytics() {
        let manager = SettingsManager::in_memory(None, None);
        manager.set_enable_analytics(true);
        let tracking_id = manager.get_tracking_id();
        manager.set_enable_analytics(false);
        manager.set_enable_analytics(true);
        assert_eq!(manager.get_tracking_id(), tracking_id);
    }
}

// ---------------------------------------------------------------------------------------------
// package-command-paths.test.ts (config selector cases)
// ---------------------------------------------------------------------------------------------

mod config_selector {
    use super::*;
    use indexmap::IndexMap;
    use pi_coding_agent::core::package_manager::{
        DefaultPackageManager, PackageManager, PackageManagerOptions, ResolvedPaths,
    };
    use pi_coding_agent::core::settings_manager::{
        InMemorySettingsStorage, SettingsManager, SettingsManagerCreateOptions, SettingsScope, SettingsStorage,
    };
    use pi_coding_agent::modes::interactive::components::config_selector::{
        ConfigSelectorComponent, ConfigWriteScope,
    };

    struct Dirs {
        _temp: tempfile::TempDir,
        project: String,
        agent: String,
        root: String,
    }

    fn dirs() -> Dirs {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_string_lossy().into_owned();
        let project = temp.path().join("project");
        let agent = temp.path().join("agent");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&agent).unwrap();
        Dirs {
            project: project.to_string_lossy().into_owned(),
            agent: agent.to_string_lossy().into_owned(),
            root,
            _temp: temp,
        }
    }

    fn settings(global: Value) -> Arc<SettingsManager> {
        let storage = Arc::new(InMemorySettingsStorage::new());
        let content = global.to_string();
        storage.with_lock(SettingsScope::Global, &mut |_| Ok(Some(content.clone()))).unwrap();
        SettingsManager::from_storage(
            storage as Arc<dyn SettingsStorage>,
            Some(&SettingsManagerCreateOptions { project_trusted: Some(true) }),
        )
    }

    fn selector(paths: &ResolvedPaths, settings: &Arc<SettingsManager>, d: &Dirs, scope: ConfigWriteScope) -> ConfigSelectorComponent {
        let mut scoped = IndexMap::new();
        scoped.insert(ConfigWriteScope::Global, paths.clone());
        scoped.insert(ConfigWriteScope::Project, paths.clone());
        ConfigSelectorComponent::new(
            &scoped,
            settings.clone(),
            &d.project,
            &d.agent,
            Arc::new(|| {}),
            Arc::new(|| {}),
            Arc::new(|| {}),
            Some(24),
            Some(&scope),
            None,
        )
    }

    async fn builtin_paths(settings: &Arc<SettingsManager>, d: &Dirs, builtins: &[&str]) -> ResolvedPaths {
        let mut paths = DefaultPackageManager::new(&PackageManagerOptions {
            cwd: d.project.clone(),
            agent_dir: d.agent.clone(),
            settings_manager: settings.clone(),
            builtin_extensions: Some(builtins.iter().map(|s| s.to_string()).collect()),
        })
        .resolve(None)
        .await
        .unwrap();
        // Resolution also auto-discovers the real user's `~/.agents/skills`, which would put user
        // skills before the built-ins in the list. The TS test assumes a home without them.
        paths
            .extensions
            .retain(|resource| resource.metadata.source == "builtin");
        paths.skills.clear();
        paths.prompts.clear();
        paths.themes.clear();
        paths
    }

    #[test]
    fn cycles_project_package_overrides_in_config_local_mode() {
        let _g = setup();
        let d = dirs();
        let settings = settings(json!({ "packages": ["npm:pi-tools"] }));
        let package_root = format!("{}/pkg", d.root);
        let paths: ResolvedPaths = serde_json::from_value(json!({
            "extensions": [{
                "path": format!("{package_root}/extensions/bar.ts"),
                "enabled": true,
                "metadata": { "source": "npm:pi-tools", "scope": "user", "origin": "package", "baseDir": package_root },
            }],
            "skills": [], "prompts": [], "themes": [],
        }))
        .unwrap();
        let selector = selector(&paths, &settings, &d, ConfigWriteScope::Project);
        let packages = || settings.get_project_settings_json().get("packages").cloned().unwrap_or(json!([]));

        selector.get_resource_list().handle_input(" ");
        assert_eq!(
            packages(),
            json!([{ "source": "npm:pi-tools", "autoload": false, "extensions": ["-extensions/bar.ts"] }])
        );
        selector.get_resource_list().handle_input(" ");
        assert_eq!(
            packages(),
            json!([{ "source": "npm:pi-tools", "autoload": false, "extensions": ["+extensions/bar.ts"] }])
        );
        selector.get_resource_list().handle_input(" ");
        assert_eq!(packages(), json!([]));
    }

    #[tokio::test]
    async fn toggles_built_in_extensions_in_config_global_mode() {
        let _g = setup();
        let d = dirs();
        let settings = settings(json!({}));
        let paths = builtin_paths(&settings, &d, &["llama.cpp", "mcp"]).await;
        let selector = selector(&paths, &settings, &d, ConfigWriteScope::Global);
        let list = selector.get_resource_list();
        let rendered = list.render(80).join("\n");
        assert!(rendered.contains("Built-in") && rendered.contains("llama.cpp"), "{rendered}");
        let extensions = || settings.get_global_settings_json().get("extensions").cloned().unwrap_or(json!([]));
        list.handle_input(" ");
        assert_eq!(extensions(), json!(["-builtin:llama.cpp"]));
        list.handle_input(" ");
        assert_eq!(extensions(), json!(["+builtin:llama.cpp"]));
    }

    #[tokio::test]
    async fn cycles_project_built_in_extension_overrides_in_config_local_mode() {
        let _g = setup();
        let d = dirs();
        let settings = settings(json!({ "extensions": ["-builtin:mcp"] }));
        let paths = builtin_paths(&settings, &d, &["mcp"]).await;
        let selector = selector(&paths, &settings, &d, ConfigWriteScope::Project);
        let list = selector.get_resource_list();
        let extensions = || settings.get_project_settings_json().get("extensions").cloned().unwrap_or(json!([]));
        list.handle_input(" ");
        assert_eq!(extensions(), json!(["+builtin:mcp"]));
        list.handle_input(" ");
        assert_eq!(extensions(), json!(["-builtin:mcp"]));
        list.handle_input(" ");
        assert_eq!(extensions(), json!([]));
    }
}

// ---------------------------------------------------------------------------------------------
// interactive-tui.test.ts (renderer selection)
// ---------------------------------------------------------------------------------------------

mod interactive_tui {
    use super::*;
    use pi_coding_agent::core::settings_schema::TuiModeValue;
    use pi_coding_agent::modes::interactive::tui_renderer::{InteractiveTuiOptions, create_interactive_tui};
    use pi_tui::components::scroll_view::{ScrollFollow, ScrollView, ScrollViewOptions};
    use pi_tui::terminal::Terminal;
    use std::time::{Duration, Instant};

    fn options(mode: TuiModeValue, terminal: &VirtualTerminal) -> InteractiveTuiOptions {
        InteractiveTuiOptions {
            tui_mode: mode,
            show_hardware_cursor: false,
            log_directory: std::env::temp_dir().to_string_lossy().into_owned(),
            terminal: Some(Arc::new(terminal.clone()) as Arc<dyn Terminal>),
            on_right_click_paste: None,
            fullscreen_copy_on_select: None,
            fullscreen_wheel_scroll_lines: None,
        }
    }

    fn wait_until(what: &str, predicate: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !predicate() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn selects_the_alternate_screen_renderer_only_when_requested() {
        let _g = setup();
        let main_terminal = VirtualTerminal::new(80, 24);
        let main = create_interactive_tui(&options(TuiModeValue::Regular, &main_terminal));
        assert_eq!(main.mode(), pi_tui::tui::TuiMode::Regular);
        assert!(!main.is_viewport_tui());
        main.start();
        main.request_render(false);
        wait_until("the main screen render", || !main_terminal.get_writes().is_empty());
        assert!(!main_terminal.get_writes().contains("\x1b[?1049h"));
        main.stop(Default::default());

        let alt_terminal = VirtualTerminal::new(80, 24);
        let alt = create_interactive_tui(&options(TuiModeValue::Fullscreen, &alt_terminal));
        assert_eq!(alt.mode(), pi_tui::tui::TuiMode::Fullscreen);
        assert!(alt.is_viewport_tui());
        alt.start();
        alt.request_render(false);
        wait_until("the alternate screen", || alt_terminal.get_writes().contains("\x1b[?1049h"));
        alt.stop(Default::default());
    }

    #[test]
    fn shows_the_configured_jump_to_bottom_shortcut_while_scrolled_up() {
        let _g = setup();
        let config = [("tui.altScreen.bottom".to_string(), Some(pi_tui::keybindings::KeySpec::from("ctrl+j")))]
            .into_iter()
            .collect();
        pi_tui::keybindings::set_keybindings(KeybindingsManager::new(Some(config), None).tui_manager());
        let terminal = VirtualTerminal::new(50, 4);
        let ui = create_interactive_tui(&options(TuiModeValue::Fullscreen, &terminal));
        let text = (1..=8).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        ui.set_layout_root(Some(ScrollView::new(
            Arc::new(Text::new(text, 0, 0, None)),
            ScrollViewOptions { follow: Some(ScrollFollow::End), primary: Some(true), ..Default::default() },
        )));
        ui.start();
        ui.request_render(false);
        wait_until("the first render", || terminal.get_viewport().iter().any(|l| l.contains("line 8")));
        terminal.send_input("\x1b[<64;1;1M");
        wait_until("the jump hint", || terminal.get_viewport()[3].contains("↓ Jump to latest message · Ctrl+J"));
        ui.stop(Default::default());
        pi_tui::keybindings::set_keybindings(KeybindingsManager::new(None, None).tui_manager());
    }
}
