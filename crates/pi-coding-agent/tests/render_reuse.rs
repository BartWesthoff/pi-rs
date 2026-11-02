//! Reuse of rendering work while replies and tool calls stream (PERF-03, PERF-04).
//!
//! Ports `code-highlighter.test.ts`, the Markdown reuse case of `assistant-message.test.ts`
//! (upstream 0f21a33, #10971) and the once-per-frame case of `tool-execution-component.test.ts`
//! (upstream f715a15, #9265). Highlighting is counted with the process-wide `highlight()` call
//! counter, so these tests run one at a time.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use parking_lot::{Mutex, MutexGuard};
use pi_ai::providers::faux::{
    FauxMessageOptions, faux_assistant_message, faux_text, faux_thinking,
};
use pi_ai::types::{AssistantContent, AssistantMessage};
use pi_coding_agent::core::extensions::types::{MarkdownTransformContext, ToolRenderers};
use pi_coding_agent::core::keybindings::KeybindingsManager;
use pi_coding_agent::core::tools::renderers::write_renderers;
use pi_coding_agent::extensions::codemode::renderer::codemode_renderers;
use pi_coding_agent::modes::interactive::components::assistant_message::AssistantMessageComponent;
use pi_coding_agent::modes::interactive::components::tool_execution::{
    ToolExecutionComponent, ToolExecutionComponentToolDefinition,
    ToolExecutionComponentUpdateResultResult,
};
use pi_coding_agent::modes::interactive::theme::theme::{
    CodeHighlighter, highlight_code, init_theme,
};
use pi_coding_agent::utils::ansi::strip_ansi;
use pi_coding_agent::utils::syntax_highlight::{
    highlight_call_count, load_all_highlight_languages,
};
use pi_tui::event_loop::EventLoop;
use pi_tui::testing::VirtualTerminal;
use pi_tui::tui::{Component, Tui, TuiOptions};
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
        TuiOptions {
            event_loop: Some(EventLoop::new_manual()),
            ..Default::default()
        },
    )
}

fn row(name: &str, args: Value, renderers: ToolRenderers) -> ToolExecutionComponent {
    let definition = ToolExecutionComponentToolDefinition::ToolRenderers(renderers);
    ToolExecutionComponent::new(name, "id", args, None, Some(&definition), fake_tui(), "/")
}

fn plain(component: &dyn Component, width: usize) -> String {
    strip_ansi(&component.render(width).join("\n")).unwrap()
}

fn result(value: Value) -> ToolExecutionComponentUpdateResultResult {
    serde_json::from_value(value).unwrap()
}

fn message(content: Vec<AssistantContent>) -> AssistantMessage {
    faux_assistant_message(content, FauxMessageOptions::default())
}

#[test]
fn assistant_message_reuses_its_markdown_across_streaming_and_its_end_and_renders_like_a_new_component_after_a_theme_change()
 {
    let _guard = setup();
    let streaming = Arc::new(Mutex::new(Vec::<bool>::new()));
    let seen = streaming.clone();
    let transformer = Arc::new(move |markdown: &str, context: &MarkdownTransformContext| {
        seen.lock().push(context.is_streaming);
        markdown.to_string()
    });
    let code = "```ts\nconst a = 1;\n```";
    let content = |text: &str| {
        vec![
            faux_thinking("Thinking with **bold** text"),
            faux_text(text),
        ]
    };
    let component = AssistantMessageComponent::new(
        None,
        Some(false),
        None,
        Some("Thinking..."),
        Some(1),
        Some(&[transformer]),
        None,
        None,
    );
    let before = highlight_call_count();
    component.update_content(message(content(code)), Some(true));
    component.render(80);
    let ended = message(content(&format!("{code}\n\nThe end.")));
    component.update_content(ended.clone(), Some(false));
    component.render(80);
    assert_eq!(highlight_call_count() - before, 1);
    assert_eq!(streaming.lock().last(), Some(&false));

    init_theme(Some("light"), Some(false));
    component.invalidate();
    let fresh =
        AssistantMessageComponent::new(Some(ended), None, None, None, None, None, None, None);
    assert_eq!(component.render(80), fresh.render(80));
    init_theme(Some("dark"), Some(false));
}

#[tokio::test]
async fn code_highlighter_reuses_its_last_result_until_the_code_the_theme_or_the_grammars_change() {
    let _guard = setup();
    let code = "const page = html`<div class=\"x\">ok</div>`;";
    let highlighter = CodeHighlighter::new();
    let first = highlighter.highlight(code, Some("javascript"));
    assert!(Arc::ptr_eq(
        &highlighter.highlight(code, Some("javascript")),
        &first
    ));
    assert!(!Arc::ptr_eq(
        &highlighter.highlight("const a = 1;", Some("javascript")),
        &first
    ));

    init_theme(Some("light"), Some(false));
    assert_eq!(
        *highlighter.highlight(code, Some("javascript")),
        highlight_code(code, Some("javascript"))
    );
    let light = highlighter.highlight(code, Some("javascript"));
    load_all_highlight_languages().await.unwrap();
    assert_ne!(*highlighter.highlight(code, Some("javascript")), *light);
    init_theme(Some("dark"), Some(false));
}

#[test]
fn codemode_highlights_the_script_once_across_progress_timer_ticks_expanding_and_completion() {
    let _guard = setup();
    let before = highlight_call_count();
    let component = row(
        "codemode",
        json!({ "code": "await tools.read({});" }),
        codemode_renderers(),
    );
    component.set_args_complete();
    component.mark_execution_started();
    component.update_result(
        &result(json!({ "content": [], "details": { "calls": [] }, "isError": false })),
        Some(true),
    );
    // Timer ticks redraw the row through the renderer context's invalidate.
    component.invalidate();
    component.invalidate();
    component.set_expanded(true);
    component.update_result(
        &result(json!({ "content": [{ "type": "text", "text": "done" }], "details": { "calls": [] }, "isError": false })),
        Some(false),
    );
    component.render(80);
    assert_eq!(highlight_call_count() - before, 1);
}

#[test]
fn write_highlights_unchanged_first_lines_and_a_complete_file_only_once() {
    let _guard = setup();
    let content: String = (0..60)
        .map(|i| format!("const v{i} = {i};"))
        .collect::<Vec<_>>()
        .join("\n");
    let component = row(
        "write",
        json!({ "path": "a.ts", "content": content }),
        write_renderers(),
    );
    component.update_args(json!({ "path": "a.ts", "content": format!("{content} //") }));
    component.render(80);
    let before = highlight_call_count();
    // Only the changed last line is highlighted again.
    component.update_args(json!({ "path": "a.ts", "content": format!("{content} // more") }));
    component.render(80);
    assert_eq!(highlight_call_count() - before, 1);

    component.set_args_complete();
    component.set_expanded(true);
    component.set_expanded(false);
    component.render(80);
    assert_eq!(highlight_call_count() - before, 2);
}

/// #9265: every argument delta re-ran the call renderer; a streamed `write` re-highlighted its
/// whole content each time.
#[test]
fn renders_streamed_partial_arguments_once_per_frame_with_the_latest_arguments() {
    let _guard = setup();
    let write = write_renderers();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let inner = write.render_call.clone().unwrap();
    let counted = ToolRenderers {
        render_call: Some(Arc::new(move |args, theme, context| {
            counter.fetch_add(1, Ordering::SeqCst);
            inner(args, theme, context)
        })),
        ..write.clone()
    };
    let component = row("write", json!({}), counted);
    let calls_before = calls.load(Ordering::SeqCst);
    let mut content = String::new();
    for index in 0..200 {
        content.push_str(&format!("const line{index} = {index};\n"));
        component.update_args(json!({ "path": "src/file.ts", "content": content }));
    }
    assert_eq!(calls.load(Ordering::SeqCst), calls_before);

    let streamed = plain(&component, 120);
    assert_eq!(calls.load(Ordering::SeqCst) - calls_before, 1);
    // The same preview as rendering the final arguments directly.
    let direct = row(
        "write",
        json!({ "path": "src/file.ts", "content": content }),
        write.clone(),
    );
    assert_eq!(streamed, plain(&direct, 120));

    // More deltas before the next frame: one more renderer run, which shows them all.
    component.update_args(
        json!({ "path": "src/file.ts", "content": format!("{content}const tail = 1;\n") }),
    );
    let last = json!({ "path": "src/file.ts", "content": format!("{content}const tail = 2;\n") });
    component.update_args(last.clone());
    let next = plain(&component, 120);
    assert_eq!(calls.load(Ordering::SeqCst) - calls_before, 2);
    assert!(next.contains("201 total"), "{next}");
    assert_eq!(next, plain(&row("write", last, write), 120));
    // Rendering again without new arguments does not run the renderer.
    component.render(120);
    assert_eq!(calls.load(Ordering::SeqCst) - calls_before, 2);
}
