//! Port of `packages/coding-agent/src/extensions/codemode/renderer.ts`.
//!
//! Presentation for the codemode tool.
//!
//! The call shows the script; the result lists the nested tool calls with their status as they
//! run and the cost of its model calls, followed by the script output without the "Script completed"
//! header, and the time the script has been running (`Elapsed`) or took (`Took`), like bash rows.
//! Nested calls are not separate tool rows because they never reach the model as tool calls.

use crate::core::extensions::types::{ToolRenderContext, ToolRenderResultOptions, ToolRenderers};
use crate::core::tools::js_util::{js_to_fixed, js_trim, js_trim_end, utf16_len, utf16_prefix};
use crate::core::tools::render_utils::{
    get_content_text_output, replace_tabs, start_tool_timer, str_ref, tool_timer_text,
    with_render_state,
};
use crate::core::tools::renderers::reuse_container;
use crate::extensions::codemode::tool::{CodemodeNestedCall, CodemodeNestedCallStatus, CodemodeToolDetails};
use crate::modes::interactive::components::keybinding_hints::key_hint;
use crate::modes::interactive::components::visual_truncate::{
    TruncateToVisualLinesKeep, VisualLinePreview, VisualLinePreviewOptions,
};
use crate::modes::interactive::theme::theme::{CodeHighlighter, Theme};
use crate::modes::interactive::theme::theme_tokens::ThemeColor;
use pi_agent::types::AgentToolResult;
use pi_ai::types::UserContentBlock;
use pi_tui::components::spacer::Spacer;
use pi_tui::components::text::Text;
use pi_tui::tui::ComponentRef;
use regex::Regex;
use serde_json::Value;
use std::sync::Arc;
use std::sync::LazyLock;

const CODE_PREVIEW_LINES: i64 = 10;
const CALL_PREVIEW_COUNT: usize = 8;
const OUTPUT_PREVIEW_LINES: i64 = 5;
const COLLAPSED_ARGS_CHARS: usize = 80;
static SCRIPT_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\AScript (completed|failed)\nWall time [\d.]+ seconds\nOutput:\n\z").unwrap());

/// Renderer state of a codemode row.
#[derive(Default)]
struct CodemodeRenderState {
    /// Progress, timer ticks and expanding redraw the call, so the row keeps the script's highlighting.
    script_highlighter: Arc<CodeHighlighter>,
}

fn expand_hint(theme: &Theme, hidden: f64, noun: &str) -> String {
    format!(
        "{} {}{}",
        theme.fg(&ThemeColor::Muted, &format!("... ({hidden} more {noun},")),
        key_hint("app.tools.expand".to_string(), "to expand"),
        theme.fg(&ThemeColor::Muted, ")")
    )
}

fn format_duration(ms: Option<f64>) -> String {
    match ms {
        None => String::new(),
        Some(ms) if ms < 1000.0 => format!("{}ms", (ms + 0.5).floor()),
        Some(ms) => format!("{}s", js_to_fixed(ms / 1000.0, 1)),
    }
}

/// `Number.prototype.toPrecision(2)`.
fn to_precision_2(value: f64) -> String {
    if value == 0.0 {
        return "0.0".to_string();
    }
    // `{:.1e}` rounds to two significant digits and gives the exponent after rounding.
    let formatted = format!("{:.1e}", value);
    let (mantissa, exponent) = formatted.split_once('e').unwrap_or((&formatted, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let negative = mantissa.starts_with('-');
    let digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
    let sign = if negative { "-" } else { "" };
    if !(-6..2).contains(&exponent) {
        let exp_sign = if exponent < 0 { "-" } else { "+" };
        return format!("{sign}{}.{}e{exp_sign}{}", &digits[..1], &digits[1..], exponent.abs());
    }
    if exponent < 0 {
        format!("{sign}0.{}{digits}", "0".repeat((-exponent - 1) as usize))
    } else if exponent == 0 {
        format!("{sign}{}.{}", &digits[..1], &digits[1..])
    } else {
        format!("{sign}{digits}")
    }
}

/// Cents for larger amounts, two significant digits for the fractions of a cent classifier calls cost.
fn format_cost(cost: f64) -> String {
    format!("${}", if cost >= 0.01 { js_to_fixed(cost, 2) } else { to_precision_2(cost) })
}

fn status_icon(call: &CodemodeNestedCall, theme: &Theme) -> String {
    match call.status {
        CodemodeNestedCallStatus::Running => theme.fg(&ThemeColor::Warning, "…"),
        CodemodeNestedCallStatus::Ok => theme.fg(&ThemeColor::Success, "✓"),
        CodemodeNestedCallStatus::Error => theme.fg(&ThemeColor::Error, "✗"),
        CodemodeNestedCallStatus::Cancelled => theme.fg(&ThemeColor::Muted, "⊘"),
    }
}

fn format_call(call: &CodemodeNestedCall, theme: &Theme, expanded: bool) -> String {
    let args = if !expanded && utf16_len(&call.args) > COLLAPSED_ARGS_CHARS {
        format!("{}...", utf16_prefix(&call.args, COLLAPSED_ARGS_CHARS - 3))
    } else {
        call.args.clone()
    };
    let duration = format_duration(call.duration_ms);
    let mut line = format!("{} {}", status_icon(call, theme), theme.fg(&ThemeColor::ToolTitle, &call.name));
    if !args.is_empty() {
        line.push_str(&format!(" {}", theme.fg(&ThemeColor::Muted, &args)));
    }
    if !duration.is_empty() {
        line.push_str(&format!(" {}", theme.fg(&ThemeColor::Dim, &duration)));
    }
    if let Some(cost) = call.cost.filter(|c| *c != 0.0 && !c.is_nan()) {
        line.push_str(&format!(" {}", theme.fg(&ThemeColor::Dim, &format_cost(cost))));
    }
    if expanded && let Some(error) = call.error.as_deref().filter(|e| !e.is_empty()) {
        line.push_str(&format!("\n    {}", theme.fg(&ThemeColor::Error, &error.split('\n').collect::<Vec<_>>().join("\n    "))));
    }
    line
}

fn render_call(args: &Value, theme: &Arc<Theme>, context: &ToolRenderContext) -> ComponentRef {
    start_tool_timer(context);
    // The code includes the `// @options:` line, so options show as part of the script.
    let code = str_ref(args.get("code"));
    let title = theme.fg(&ThemeColor::ToolTitle, &theme.bold("codemode"));
    let component = reuse_container(context);
    component.clear();
    let Some(code) = code else {
        component.add_child(Arc::new(Text::new(
            format!("{title} {}", theme.fg(&ThemeColor::Error, "[invalid arg]")),
            0,
            0,
            None,
        )));
        return component;
    };
    component.add_child(Arc::new(Text::new(title, 0, 0, None)));
    if !code.is_empty() {
        let highlighter = with_render_state::<CodemodeRenderState, _>(&context.state, |state| {
            state.script_highlighter.clone()
        });
        let highlighted = highlighter
            .highlight(
                &replace_tabs(js_trim_end(&code.replace('\r', ""))),
                Some("javascript"),
            )
            .join("\n");
        if context.expanded {
            component.add_child(Arc::new(Text::new(highlighted, 0, 0, None)));
        } else {
            let hint_theme = theme.clone();
            component.add_child(Arc::new(VisualLinePreview::new(&VisualLinePreviewOptions {
                text: highlighted,
                max_visual_lines: CODE_PREVIEW_LINES,
                keep: TruncateToVisualLinesKeep::Start,
                format_hint: Arc::new(move |hidden| expand_hint(&hint_theme, hidden, "lines")),
            })));
        }
    }
    component
}

fn render_result(
    result: &AgentToolResult,
    options: ToolRenderResultOptions,
    theme: &Arc<Theme>,
    context: &ToolRenderContext,
) -> ComponentRef {
    let timer_text = tool_timer_text(context, options.is_partial);
    let component = reuse_container(context);
    component.clear();
    let details: Option<CodemodeToolDetails> =
        result.details.as_ref().and_then(|details| serde_json::from_value(details.clone()).ok());
    let calls = details.as_ref().map(|d| d.calls.clone()).unwrap_or_default();
    if !calls.is_empty() {
        let shown: &[CodemodeNestedCall] =
            if options.expanded { &calls } else { &calls[calls.len().saturating_sub(CALL_PREVIEW_COUNT)..] };
        let mut lines: Vec<String> = shown.iter().map(|call| format_call(call, theme, options.expanded)).collect();
        if shown.len() < calls.len() {
            lines.insert(
                0,
                format!(
                    "{} {}{}",
                    theme.fg(&ThemeColor::Muted, &format!("... ({} earlier calls,", calls.len() - shown.len())),
                    key_hint("app.tools.expand".to_string(), "to expand"),
                    theme.fg(&ThemeColor::Muted, ")")
                ),
            );
        }
        // Collapsed rows hide earlier calls, so the total covers every call.
        let priced: Vec<f64> = calls.iter().filter_map(|call| call.cost).filter(|c| *c != 0.0 && !c.is_nan()).collect();
        if priced.len() > 1 {
            let total: f64 = priced.iter().sum();
            lines.push(theme.fg(&ThemeColor::Muted, &format!("Model calls: {}", format_cost(total))));
        }
        component.add_child(Arc::new(Spacer::new(1)));
        component.add_child(Arc::new(Text::new(lines.join("\n"), 0, 0, None)));
    }

    // Drop the "Script completed\nWall time ...\nOutput:\n" header. Rejected input (invalid options)
    // has no header.
    let has_header = matches!(result.content.first(), Some(UserContentBlock::Text(t)) if SCRIPT_HEADER.is_match(&t.text));
    let output = if options.is_partial {
        String::new()
    } else {
        let content: &[UserContentBlock] = if has_header { &result.content[1..] } else { &result.content };
        js_trim(&get_content_text_output(Some(content), context.show_images)).to_string()
    };
    if !output.is_empty() {
        let color = if context.is_error { ThemeColor::Error } else { ThemeColor::ToolOutput };
        let styled = replace_tabs(&output).split('\n').map(|line| theme.fg(&color, line)).collect::<Vec<_>>().join("\n");
        component.add_child(Arc::new(Spacer::new(1)));
        if options.expanded {
            component.add_child(Arc::new(Text::new(styled, 0, 0, None)));
        } else {
            // Limit wrapped lines, not logical ones: script output is often one long JSON line.
            let hint_theme = theme.clone();
            component.add_child(Arc::new(VisualLinePreview::new(&VisualLinePreviewOptions {
                text: styled,
                max_visual_lines: OUTPUT_PREVIEW_LINES,
                keep: TruncateToVisualLinesKeep::Start,
                format_hint: Arc::new(move |hidden| expand_hint(&hint_theme, hidden, "lines")),
            })));
            // The collapsed preview hides the truncation notice at the end, so name the file here.
            if let Some(path) = details.as_ref().and_then(|d| d.full_output_path.as_deref()).filter(|p| !p.is_empty()) {
                component.add_child(Arc::new(Text::new(
                    theme.fg(&ThemeColor::Muted, &format!("Full output: {path}")),
                    0,
                    0,
                    None,
                )));
            }
        }
    }
    if let Some(timer_text) = timer_text.filter(|t| !t.is_empty()) {
        component.add_child(Arc::new(Spacer::new(1)));
        component.add_child(Arc::new(Text::new(theme.fg(&ThemeColor::Muted, &timer_text), 0, 0, None)));
    }
    component
}

/// The codemode tool's `renderCall` and `renderResult` (`codemodeRenderers`).
pub fn codemode_renderers() -> ToolRenderers {
    ToolRenderers {
        render_shell: None,
        render_call: Some(Arc::new(render_call)),
        render_result: Some(Arc::new(render_result)),
    }
}

pub static CODEMODE_RENDERERS: LazyLock<ToolRenderers> = LazyLock::new(codemode_renderers);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_costs_like_js() {
        assert_eq!(format_cost(0.000012936), "$0.000013");
        assert_eq!(format_cost(0.02), "$0.02");
        assert_eq!(format_cost(0.0012), "$0.0012");
        assert_eq!(format_cost(0.00000012), "$1.2e-7");
        assert_eq!(format_duration(Some(5.0)), "5ms");
        assert_eq!(format_duration(Some(5100.0)), "5.1s");
    }
}
