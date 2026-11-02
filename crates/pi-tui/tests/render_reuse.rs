//! Reuse of rendered output across renders: `Markdown` block reuse and `Text::set_text` with
//! unchanged text. Ports the "Block reuse" cases of `markdown.test.ts` and `text.test.ts` (upstream
//! 0f21a33, #10971).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pi_tui::components::markdown::{Markdown, MarkdownTheme};
use pi_tui::components::text::Text;
use pi_tui::terminal_image::{TerminalCapabilities, reset_capabilities_cache, set_capabilities};
use pi_tui::tui::Component;

/// Terminal capabilities are process-wide; tests that set them run one at a time.
static CAPABILITIES: Mutex<()> = Mutex::new(());

fn counting_theme(calls: Arc<Mutex<Vec<String>>>) -> MarkdownTheme {
    let mut theme = MarkdownTheme::plain();
    theme.highlight_code = Some(Arc::new(move |code: &str, lang: Option<&str>| {
        calls
            .lock()
            .unwrap()
            .push(format!("{}:{code}", lang.unwrap_or("")));
        code.split('\n').map(|line| format!("<{line}>")).collect()
    }));
    theme
}

fn trimmed(lines: &[String]) -> Vec<String> {
    lines.iter().map(|line| line.trim().to_string()).collect()
}

#[test]
fn highlights_a_code_block_once_across_streamed_text_and_resizes_and_again_after_invalidate() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let markdown = Markdown::new(
        "```ts\nx\n```",
        0,
        0,
        counting_theme(calls.clone()),
        None,
        None,
    );
    markdown.render(80);
    markdown.set_text("```ts\nx\n```\n\nafter");
    assert_eq!(
        trimmed(&markdown.render(40)),
        ["```ts", "<x>", "```", "", "after"]
    );
    assert_eq!(*calls.lock().unwrap(), ["ts:x"]);
    markdown.invalidate();
    markdown.render(40);
    assert_eq!(*calls.lock().unwrap(), ["ts:x", "ts:x"]);
}

#[test]
fn renders_a_block_again_when_the_block_after_it_changes_its_spacing() {
    let markdown = Markdown::new("# Title", 0, 0, MarkdownTheme::plain(), None, None);
    markdown.render(80);
    markdown.set_text("# Title\nfoo");
    assert_eq!(trimmed(&markdown.render(80)), ["Title", "", "foo"]);
}

#[test]
fn follows_changed_reference_link_definitions() {
    let _guard = CAPABILITIES.lock().unwrap_or_else(|e| e.into_inner());
    set_capabilities(TerminalCapabilities {
        images: None,
        true_color: false,
        hyperlinks: false,
    });
    let markdown = Markdown::new(
        "[a][x]\n\n[x]: https://one.example",
        0,
        0,
        MarkdownTheme::plain(),
        None,
        None,
    );
    markdown.render(80);
    markdown.set_text("[a][x]\n\n[x]: https://two.example");
    let output = markdown.render(80).join("\n");
    reset_capabilities_cache();
    assert!(output.contains("https://two.example"), "{output}");
}

/// While a message streams only its last block renders again; invalidation renders every block.
#[test]
fn a_streamed_message_renders_only_its_last_block_per_delta() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let paragraphs = Arc::new(AtomicUsize::new(0));
    let mut theme = counting_theme(calls.clone());
    let counter = paragraphs.clone();
    // Every paragraph below is bold, so the bold style counts paragraph renders.
    theme.bold = Arc::new(move |text: &str| {
        counter.fetch_add(1, Ordering::SeqCst);
        format!("*{text}*")
    });
    let full = "**one**\n\n```rs\na\n```\n\n**two**\n\n```py\nb\n```\n\n**three**\n\n```js\nc1\nc2\nc3\n```";
    let markdown = Markdown::new("", 0, 0, theme, None, None);
    let mut last_highlights = 0;
    for end in 1..=full.len() {
        markdown.set_text(&full[..end]);
        markdown.render(60);
        let highlights = calls.lock().unwrap().len();
        // A delta highlights at most the code block it extends.
        assert!(
            highlights - last_highlights <= 1,
            "delta {end} highlighted {} blocks",
            highlights - last_highlights
        );
        last_highlights = highlights;
    }
    let streamed = calls.lock().unwrap().clone();
    // A code block highlights while its own source streams (including its closing fence) and never
    // again once later blocks follow.
    let last = |lang: &str| streamed.iter().rposition(|c| c.starts_with(lang));
    let first = |lang: &str| streamed.iter().position(|c| c.starts_with(lang));
    assert!(last("rs:") < first("py:"), "{streamed:?}");
    assert!(last("py:") < first("js:"), "{streamed:?}");
    assert_eq!(
        trimmed(&markdown.render(60)),
        [
            "*one*", "", "```rs", "<a>", "```", "", "*two*", "", "```py", "<b>", "```", "",
            "*three*", "", "```js", "<c1>", "<c2>", "<c3>", "```"
        ]
    );

    // A theme change (invalidate) renders every block again, once.
    let (highlights_before, paragraphs_before) = (
        calls.lock().unwrap().len(),
        paragraphs.load(Ordering::SeqCst),
    );
    markdown.invalidate();
    markdown.render(60);
    assert_eq!(
        calls.lock().unwrap()[highlights_before..],
        ["rs:a", "py:b", "js:c1\nc2\nc3"]
    );
    assert_eq!(paragraphs.load(Ordering::SeqCst) - paragraphs_before, 3);
    // An unchanged render reuses everything.
    markdown.render(60);
    assert_eq!(calls.lock().unwrap().len(), highlights_before + 3);
}

#[test]
fn text_keeps_its_wrapped_lines_when_the_same_text_is_set_again() {
    let renders = Arc::new(AtomicUsize::new(0));
    let counter = renders.clone();
    let text = Text::new(
        "hello world",
        0,
        0,
        Some(Arc::new(move |line: &str| {
            counter.fetch_add(1, Ordering::SeqCst);
            line.to_string()
        })),
    );
    let lines = text.render(5);
    let after_first = renders.load(Ordering::SeqCst);
    text.set_text("hello world");
    assert_eq!(text.render(5), lines);
    assert_eq!(renders.load(Ordering::SeqCst), after_first);
    text.set_text("hello there");
    text.render(5);
    assert!(renders.load(Ordering::SeqCst) > after_first);
}
