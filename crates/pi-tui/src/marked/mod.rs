//! Markdown lexer: a port of the [marked](https://marked.js.org) lexer (GFM mode).
//!
//! Only the lexer is ported (the TUI renders tokens itself). The grammar is
//! marked's own, compiled through [`js_regex`] so that JavaScript regex semantics
//! are preserved. Optional extensions reproduce pi's configuration:
//! CJK-friendly emphasis (`marked-cjk-friendly`), strict `~~` strikethrough and
//! LaTeX math tokens.
//!
//! All string offsets are UTF-8 byte offsets.

pub mod js_regex;
mod lexer;
pub mod rules;

pub use lexer::{Lexer, Links};

/// Token kinds produced by the lexer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TokenType {
    Space,
    Code,
    Heading,
    Table,
    Hr,
    Blockquote,
    List,
    ListItem,
    Checkbox,
    Html,
    Def,
    Paragraph,
    #[default]
    Text,
    Escape,
    Link,
    Image,
    Strong,
    Em,
    Codespan,
    Br,
    Del,
    /// Inline LaTeX math (pi extension).
    Latex,
    /// Display LaTeX math (pi extension).
    LatexBlock,
}

impl TokenType {
    /// marked's `type` string.
    pub fn as_str(self) -> &'static str {
        match self {
            TokenType::Space => "space",
            TokenType::Code => "code",
            TokenType::Heading => "heading",
            TokenType::Table => "table",
            TokenType::Hr => "hr",
            TokenType::Blockquote => "blockquote",
            TokenType::List => "list",
            TokenType::ListItem => "list_item",
            TokenType::Checkbox => "checkbox",
            TokenType::Html => "html",
            TokenType::Def => "def",
            TokenType::Paragraph => "paragraph",
            TokenType::Text => "text",
            TokenType::Escape => "escape",
            TokenType::Link => "link",
            TokenType::Image => "image",
            TokenType::Strong => "strong",
            TokenType::Em => "em",
            TokenType::Codespan => "codespan",
            TokenType::Br => "br",
            TokenType::Del => "del",
            TokenType::Latex => "latex",
            TokenType::LatexBlock => "latexBlock",
        }
    }
}

/// Table column alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// A table cell.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TableCell {
    pub text: String,
    pub tokens: Vec<Token>,
    pub header: bool,
    pub align: Option<Align>,
    pub(crate) inline_src: Option<String>,
}

/// A marked token. Fields not relevant to a token's [`TokenType`] keep their defaults.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Token {
    pub kind: TokenType,
    pub raw: String,
    /// `text` (absent in marked for some token types; empty here).
    pub text: String,
    /// Child tokens (inline tokens for paragraphs/headings/text, block tokens for blockquotes/list items).
    pub tokens: Vec<Token>,
    /// Heading depth.
    pub depth: usize,
    /// Code block language (`None` when absent).
    pub lang: Option<String>,
    /// Indented (not fenced) code block.
    pub indented: bool,
    /// Link/image/def target.
    pub href: String,
    /// Link/image/def title.
    pub title: Option<String>,
    /// Def label.
    pub tag: String,
    /// List: ordered.
    pub ordered: bool,
    /// List: start number (ordered lists only).
    pub start: Option<u64>,
    /// List / list item: loose.
    pub loose: bool,
    /// List items.
    pub items: Vec<Token>,
    /// List item: task item.
    pub task: bool,
    /// List item / checkbox: checked state.
    pub checked: Option<bool>,
    /// Table header cells.
    pub header: Vec<TableCell>,
    /// Table rows.
    pub rows: Vec<Vec<TableCell>>,
    /// Table column alignments.
    pub align: Vec<Option<Align>>,
    /// HTML: block-level.
    pub block: bool,
    /// HTML: `<pre>`, `<script>` or `<style>` block.
    pub pre: bool,
    /// Inline HTML: inside a link.
    pub in_link: bool,
    /// Inline HTML / text: inside a raw block.
    pub in_raw_block: bool,
    /// Text: escaped (inside a raw HTML block).
    pub escaped: bool,
    /// LaTeX: unterminated (still streaming).
    pub pending: bool,
    pub(crate) inline_src: Option<String>,
}

impl Token {
    pub(crate) fn new(kind: TokenType, raw: impl Into<String>) -> Self {
        Self { kind, raw: raw.into(), ..Default::default() }
    }
}

/// Lexer configuration.
#[derive(Debug, Clone, Copy, Default)]
pub struct MarkedOptions {
    /// Use `marked-cjk-friendly`'s emphasis tokenizer.
    pub cjk_friendly: bool,
    /// Only `~~` (not `~`) creates strikethrough.
    pub strict_strikethrough: bool,
    /// Recognize `$…$`, `$$…$$`, `\(…\)` and `\[…\]` LaTeX math.
    pub latex: bool,
}

impl MarkedOptions {
    /// The configuration used by pi's Markdown component.
    pub fn pi() -> Self {
        Self { cjk_friendly: true, strict_strikethrough: true, latex: true }
    }
}

/// Lex markdown into block tokens (with inline tokens resolved), like `marked.lexer()`.
pub fn lexer(src: &str, options: MarkedOptions) -> Vec<Token> {
    lexer_with_link_defs(src, options).0
}

/// [`lexer`], plus whether the source defines reference links (marked's non-empty `tokens.links`).
///
/// A reference link resolves through a definition anywhere in the text, so with definitions the
/// rendering of a block can depend on other blocks.
pub fn lexer_with_link_defs(src: &str, options: MarkedOptions) -> (Vec<Token>, bool) {
    // A frame lexing while the grammar is still being compiled in the background would wait for it
    // with the UI lock held (about a second in debug builds), stalling every agent-event update.
    if !WARM_UP_THREAD.with(std::cell::Cell::get) {
        crate::ui_lock::unlocked(wait_for_warm_up);
    }
    let mut lexer = Lexer::new(options);
    let tokens = lexer.lex(src);
    (tokens, !lexer.links.is_empty())
}

thread_local! {
    static WARM_UP_THREAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Build the lexer's lazily compiled grammar on a background thread, once per process.
///
/// The grammar is a few hundred regexes compiled on first use; doing that inside the first
/// markdown render stalls that frame (about a second in debug builds), so the TUI starts it early.
pub fn warm_up_in_background() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        *WARM_UP.0.lock().unwrap_or_else(|e| e.into_inner()) = WarmUp::Running;
        let spawned = std::thread::Builder::new().name("pi-tui-marked-warmup".to_string()).spawn(|| {
            struct Done;
            impl Drop for Done {
                fn drop(&mut self) {
                    *WARM_UP.0.lock().unwrap_or_else(|e| e.into_inner()) = WarmUp::Done;
                    WARM_UP.1.notify_all();
                }
            }
            let _done = Done;
            WARM_UP_THREAD.with(|w| w.set(true));
            const DOC: &str = "# Heading\n\nSome *emphasis*, **strong**, `code`, ~~strike~~, [link](https://example.com) \
                               <https://example.com> $x^2$ and a\\\nbreak.\n\n> quote\n\n- item\n  1. nested\n\n\
                               | a | b |\n|---|---|\n| 1 | 2 |\n\n```rust\nfn main() {}\n```\n\n$$\nx\n$$\n\n---\n\n<div>html</div>\n";
            let _ = lexer(DOC, MarkedOptions::pi());
            let _ = lexer(DOC, MarkedOptions::default());
        });
        if spawned.is_err() {
            *WARM_UP.0.lock().unwrap_or_else(|e| e.into_inner()) = WarmUp::Done;
        }
    });
}

#[derive(PartialEq)]
enum WarmUp {
    NotStarted,
    Running,
    Done,
}

static WARM_UP: (std::sync::Mutex<WarmUp>, std::sync::Condvar) =
    (std::sync::Mutex::new(WarmUp::NotStarted), std::sync::Condvar::new());

/// Block until a running [`warm_up_in_background`] has finished (returns at once otherwise).
pub fn wait_for_warm_up() {
    let mut state = WARM_UP.0.lock().unwrap_or_else(|e| e.into_inner());
    while *state == WarmUp::Running {
        state = WARM_UP.1.wait(state).unwrap_or_else(|e| e.into_inner());
    }
}
