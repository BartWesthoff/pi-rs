//! Port of `packages/coding-agent/src/utils/syntax-highlight.ts`.
//!
//! highlight.js (10.7.3) is ported in [`engine`]: the same grammars, scopes (`hljs-*` class
//! names), relevance scoring and HTML output, so themes map the same token classes.

pub mod engine;
pub mod js_regex;

use indexmap::IndexMap;
use std::sync::Arc;

use crate::utils::html::decode_html_entity_at;

/// Register all highlight.js languages, not only the eagerly loaded common ones.
pub async fn load_all_highlight_languages() -> anyhow::Result<()> {
    static LOADED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    LOADED
        .get_or_init(|| async {
            // Eager languages and plaintext fallback remain available if this fails.
            let _ = tokio::task::spawn_blocking(engine::register_all_languages).await;
        })
        .await;
    Ok(())
}

pub type HighlightFormatter = Arc<dyn Fn(String) -> String + Send + Sync>;

pub type HighlightTheme = IndexMap<String, HighlightFormatter>;

#[derive(Clone, Default)]
pub struct HighlightOptions {
    pub language: Option<String>,
    pub ignore_illegals: Option<bool>,
    pub language_subset: Option<Vec<String>>,
    pub theme: Option<HighlightTheme>,
}

const SPAN_CLOSE: &str = "</span>";
const HIGHLIGHT_CLASS_PREFIX: &str = "hljs-";

fn get_scope_from_span_tag(tag: &str) -> Option<String> {
    static CLASS_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"\sclass\s*=\s*(?:"([^"]*)"|'([^']*)')"#).expect("valid regex")
    });
    let caps = CLASS_RE.captures(tag)?;
    let class_value = caps.get(1).or_else(|| caps.get(2))?.as_str();
    if class_value.is_empty() {
        return None;
    }
    class_value
        .split(char::is_whitespace)
        .find_map(|class_name| class_name.strip_prefix(HIGHLIGHT_CLASS_PREFIX).map(str::to_string))
}

fn get_scope_formatter<'a>(scope: &str, theme: &'a HighlightTheme) -> Option<&'a HighlightFormatter> {
    if let Some(exact) = theme.get(scope) {
        return Some(exact);
    }
    if let Some(dot_index) = scope.find('.')
        && let Some(prefix) = theme.get(&scope[..dot_index])
    {
        return Some(prefix);
    }
    if let Some(dash_index) = scope.find('-')
        && let Some(prefix) = theme.get(&scope[..dash_index])
    {
        return Some(prefix);
    }
    None
}

fn get_active_formatter<'a>(scopes: &[Option<String>], theme: &'a HighlightTheme) -> Option<&'a HighlightFormatter> {
    for scope in scopes.iter().rev().flatten() {
        if let Some(formatter) = get_scope_formatter(scope, theme) {
            return Some(formatter);
        }
    }
    theme.get("default")
}

fn is_span_open_tag_start(html: &str, index: usize) -> bool {
    if !html[index..].starts_with("<span") {
        return false;
    }
    matches!(html.as_bytes().get(index + "<span".len()), Some(b'>' | b' ' | b'\t' | b'\n' | b'\r'))
}

/// Render highlight.js HTML (`<span class="hljs-...">`) with a theme of formatters. Each line
/// of a token is formatted separately so line breaks never sit inside escape sequences.
pub fn render_highlighted_html(html: &str, theme: Option<&HighlightTheme>) -> String {
    let empty = HighlightTheme::new();
    let theme = theme.unwrap_or(&empty);
    let mut output = String::with_capacity(html.len());
    let mut text_buffer = String::new();
    let mut scopes: Vec<Option<String>> = Vec::new();

    let flush_text = |text_buffer: &mut String, output: &mut String, scopes: &[Option<String>]| {
        if text_buffer.is_empty() {
            return;
        }
        match get_active_formatter(scopes, theme) {
            Some(formatter) => {
                let formatted: Vec<String> = text_buffer
                    .split('\n')
                    .map(|line| if line.is_empty() { String::new() } else { formatter(line.to_string()) })
                    .collect();
                output.push_str(&formatted.join("\n"));
            }
            None => output.push_str(text_buffer),
        }
        text_buffer.clear();
    };

    let mut index = 0;
    while index < html.len() {
        if is_span_open_tag_start(html, index)
            && let Some(offset) = html[index + 5..].find('>')
        {
            let tag_end_index = index + 5 + offset;
            flush_text(&mut text_buffer, &mut output, &scopes);
            scopes.push(get_scope_from_span_tag(&html[index..=tag_end_index]));
            index = tag_end_index + 1;
            continue;
        }

        if html[index..].starts_with(SPAN_CLOSE) {
            flush_text(&mut text_buffer, &mut output, &scopes);
            scopes.pop();
            index += SPAN_CLOSE.len();
            continue;
        }

        if html.as_bytes()[index] == b'&'
            && let Some(decoded) = decode_html_entity_at(html, index)
        {
            text_buffer.push_str(&decoded.text);
            index += decoded.length;
            continue;
        }

        let ch = html[index..].chars().next().expect("index is on a char boundary");
        text_buffer.push(ch);
        index += ch.len_utf8();
    }

    flush_text(&mut text_buffer, &mut output, &scopes);
    output
}

static HIGHLIGHT_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Number of [`highlight`] calls in this process, for tests that check highlighting is reused.
#[doc(hidden)]
pub fn highlight_call_count() -> u64 {
    HIGHLIGHT_CALLS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Highlight `code` (with `options.language`, or auto-detection among `language_subset`) and
/// render it with `options.theme`. Errors for unknown languages, like highlight.js.
pub fn highlight(code: &str, options: Option<&HighlightOptions>) -> anyhow::Result<String> {
    HIGHLIGHT_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let default_options = HighlightOptions::default();
    let options = options.unwrap_or(&default_options);
    let html = match options.language.as_deref().filter(|l| !l.is_empty()) {
        Some(language) => {
            engine::highlight(code, language, options.ignore_illegals.unwrap_or(false))
                .map_err(|e| anyhow::anyhow!(e))?
                .value
        }
        None => engine::highlight_auto(code, options.language_subset.clone()).value,
    };
    Ok(render_highlighted_html(&html, options.theme.as_ref()))
}

pub fn supports_language(name: &str) -> bool {
    engine::get_language(name).is_some()
}

/// Number of registered grammars. It grows when the deferred grammars load, which can change the
/// highlighting of code that embeds another language, such as HTML in a JavaScript template literal.
pub fn get_highlight_language_count() -> usize {
    engine::language_count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn formatter(name: &'static str) -> HighlightFormatter {
        Arc::new(move |text| format!("[{name}:{text}]"))
    }

    const EAGER_LANGUAGES: [&str; 20] = [
        "python", "java", "go", "javascript", "cpp", "typescript", "php", "ruby", "c", "csharp", "nix", "bash",
        "rust", "scala", "kotlin", "swift", "dart", "groovy", "perl", "lua",
    ];

    #[tokio::test]
    async fn loads_the_twenty_most_common_languages_at_startup_and_defers_the_rest() {
        assert!(EAGER_LANGUAGES.iter().all(|l| supports_language(l)));
        // `ada` is only available after loading all languages (other tests may have loaded them).
        load_all_highlight_languages().await.unwrap();
        assert!(supports_language("ada"));
    }

    #[test]
    fn renders_highlighted_spans_with_the_provided_theme() {
        let theme: HighlightTheme = IndexMap::from([("keyword".to_string(), formatter("keyword"))]);
        let rendered = render_highlighted_html("<span class=\"hljs-keyword\">const</span> value", Some(&theme));
        assert_eq!(rendered, "[keyword:const] value");
    }

    #[test]
    fn decodes_html_entities_emitted_by_highlight_js() {
        let rendered = render_highlighted_html("&lt;tag attr=&quot;value&quot;&gt;&amp;#x41;&#65;&lt;/tag&gt;", None);
        assert_eq!(rendered, "<tag attr=\"value\">&#x41;A</tag>");
    }

    #[test]
    fn inherits_parent_formatting_for_unmapped_nested_scopes() {
        let theme: HighlightTheme = IndexMap::from([("string".to_string(), formatter("string"))]);
        let rendered = render_highlighted_html(
            "<span class=\"hljs-string\">a<span class=\"hljs-subst\">${x}</span>b</span>",
            Some(&theme),
        );
        assert_eq!(rendered, "[string:a][string:${x}][string:b]");
    }

    #[test]
    fn keeps_parent_formatting_across_unscoped_nested_spans() {
        let theme: HighlightTheme = IndexMap::from([("string".to_string(), formatter("string"))]);
        let rendered =
            render_highlighted_html("<span class=\"hljs-string\">a<span class=\"language-xml\">b</span>c</span>", Some(&theme));
        assert_eq!(rendered, "[string:a][string:b][string:c]");
    }

    #[test]
    fn highlights_code_through_highlight_js() {
        assert!(supports_language("typescript"));
        let theme: HighlightTheme = IndexMap::from([
            ("keyword".to_string(), formatter("keyword")),
            ("number".to_string(), formatter("number")),
        ]);
        let rendered = highlight(
            "const value = 1",
            Some(&HighlightOptions {
                language: Some("typescript".into()),
                ignore_illegals: Some(true),
                language_subset: None,
                theme: Some(theme),
            }),
        )
        .unwrap();
        assert!(rendered.contains("[keyword:const]"), "{rendered}");
        assert!(rendered.contains("[number:1]"), "{rendered}");
    }

    #[test]
    fn unknown_languages_error() {
        let err = highlight("x", Some(&HighlightOptions { language: Some("nope".into()), ..Default::default() }))
            .unwrap_err();
        assert_eq!(err.to_string(), "Unknown language: \"nope\"");
    }
}
