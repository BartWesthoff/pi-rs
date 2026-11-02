//! Context overflow detection (port of `utils/overflow.ts`).

use once_cell::sync::Lazy;
use regex::Regex;

use crate::types::{AssistantMessage, StopReason};

// JavaScript regex semantics: `\d`/`\w` are ASCII-only and `.` excludes all line terminators,
// so they are spelled out (the `regex` crate's classes are Unicode-aware).
const OVERFLOW_PATTERN_SOURCES: &[&str] = &[
    r"(?i)prompt (?:is )?too long",
    r"(?i)prompt exceeds max length",
    r"(?i)request_too_large",
    r"(?i)input is too long for requested model",
    r"(?i)exceeds the context window",
    r"(?i)exceeds (?:the )?(?:model'?s )?maximum context length(?: of [0-9,]+ tokens?|\s*\([0-9,]+\))",
    r"(?i)context window exceeded",
    r"(?i)input token count[^\n\r\u{2028}\u{2029}]*exceeds the maximum",
    r"(?i)maximum prompt length is [0-9]+",
    r"(?i)reduce the length of the messages",
    r"(?i)maximum context length is [0-9]+ tokens",
    r"(?i)exceeds (?:the )?maximum allowed input length of [0-9,]+ tokens?",
    r"(?i)input \([0-9]+ tokens\) is longer than the model'?s context length \([0-9]+ tokens\)",
    r"(?i)exceeds the limit of [0-9]+",
    r"(?i)exceeds the available context size",
    r"(?i)greater than the context length",
    r"(?i)context window exceeds limit",
    r"(?i)exceeded model token limit",
    r"(?i)too large for model with [0-9]+ maximum context length",
    r"(?i)prompt has [0-9,]+ tokens?, but the configured context size is [0-9,]+ tokens?",
    r"(?i)model_context_window_exceeded",
    r"(?i)prompt too long; exceeded (?:max )?context length",
    r"(?i)range of input length should be",
    r"(?i)context[_ ]length[_ ]exceeded",
    r"(?i)too many tokens",
    r"(?i)token limit exceeded",
    r"(?i)^(?:[A-Za-z0-9_ -]+ API error \(413\):|413(?:\s|:|$))",
];

static OVERFLOW_PATTERNS: Lazy<Vec<Regex>> =
    Lazy::new(|| OVERFLOW_PATTERN_SOURCES.iter().map(|p| Regex::new(p).expect("valid overflow pattern")).collect());

static CEREBRAS_BODYLESS_OVERFLOW_PATTERN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^4(?:00|13)\s*(?:status code)?\s*\(no body\)").expect("valid pattern"));

static NON_OVERFLOW_PATTERNS: Lazy<Vec<Regex>> = Lazy::new(|| {
    [r"(?i)^(Throttling error|Service unavailable):", r"(?i)rate limit", r"(?i)too many requests"]
        .iter()
        .map(|p| Regex::new(p).expect("valid pattern"))
        .collect()
});

/// Check if an assistant message represents a context overflow error.
pub fn is_context_overflow(message: &AssistantMessage, context_window: Option<u64>) -> bool {
    if message.stop_reason == StopReason::Error
        && let Some(error_message) = message.error_message.as_deref().filter(|m| !m.is_empty()) {
            let is_non_overflow = NON_OVERFLOW_PATTERNS.iter().any(|p| p.is_match(error_message));
            if !is_non_overflow {
                if OVERFLOW_PATTERNS.iter().any(|p| p.is_match(error_message)) {
                    return true;
                }
                if message.provider == "cerebras" && CEREBRAS_BODYLESS_OVERFLOW_PATTERN.is_match(error_message) {
                    return true;
                }
            }
        }

    if let Some(context_window) = context_window.filter(|w| *w > 0) {
        let input_tokens = message.usage.input + message.usage.cache_read;
        if message.stop_reason == StopReason::Stop && input_tokens > context_window {
            return true;
        }
        if message.stop_reason == StopReason::Length
            && message.usage.output == 0
            && input_tokens as f64 >= context_window as f64 * 0.99
        {
            return true;
        }
    }
    false
}

/// Check whether a length stop ended below the intended output limit.
pub fn is_recoverable_length(message: &AssistantMessage, desired_max_output: u64) -> bool {
    message.stop_reason == StopReason::Length && desired_max_output > 0 && message.usage.output < desired_max_output
}

/// The overflow patterns (for testing).
pub fn get_overflow_patterns() -> Vec<Regex> {
    OVERFLOW_PATTERNS.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(text: &str) -> AssistantMessage {
        AssistantMessage { stop_reason: StopReason::Error, error_message: Some(text.into()), ..Default::default() }
    }

    #[test]
    fn javascript_character_classes() {
        assert!(is_context_overflow(&error("github-copilot API error (413): {}"), None));
        // JS `\w` is ASCII: a non-ASCII provider name does not match the 413 prefix.
        assert!(!is_context_overflow(&error("prövider API error (413): {}"), None));
        // JS `\d` is ASCII.
        assert!(!is_context_overflow(&error("maximum prompt length is \u{0663}"), None));
        assert!(is_context_overflow(&error("maximum prompt length is 3"), None));
        // JS `.` stops at \r.
        assert!(!is_context_overflow(&error("input token count\rexceeds the maximum"), None));
        assert!(is_context_overflow(&error("input token count (5) exceeds the maximum"), None));
    }
}

