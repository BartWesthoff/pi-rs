//! Port of `packages/coding-agent/src/core/compaction/compaction.ts`.
//!
//! Context compaction for long sessions.
//!
//! Pure functions for compaction logic. The session manager handles I/O,
//! and after compaction the session is reloaded.

use crate::core::compaction::utils::{
    FileOperations, SUMMARIZATION_SYSTEM_PROMPT, compute_file_lists, create_file_ops,
    extract_file_ops_from_message, format_file_operations, js_len, serialize_conversation,
};
use crate::core::messages::convert_to_llm;
use crate::core::session_manager::{
    CompactionEntry, ProjectedSessionEntry, SessionEntry, SessionProjection, build_session_projection,
    session_entry_to_context_messages,
};
use crate::core::usage_totals::combine_usage;
use indexmap::IndexMap;
use pi_agent::types::AgentMessage;
use pi_agent::types::StreamFn;
use pi_agent::types::ThinkingLevel;
use pi_ai::types::AssistantMessage;
use pi_ai::types::Model;
use pi_ai::types::SimpleStreamOptions;
use pi_ai::types::TranscriptContext;
use pi_ai::types::Usage;
use pi_ai::types::{
    AssistantContent, CacheRetention, Context, Message, StopReason, SystemContent, UserContent,
    UserContentBlock, UserMessage,
};
use pi_ai::utils::retry::RetryCallbacks;
use pi_ai::utils::retry::RetryPolicy;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::{Arc, LazyLock};
use tokio_util::sync::CancellationToken;

// ============================================================================
// File Operation Tracking
// ============================================================================

/// Details stored in CompactionEntry.details for file tracking
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionDetails {
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
}

fn string_array(value: Option<&Value>) -> Option<Vec<String>> {
    match value {
        Some(Value::Array(items)) => Some(
            items
                .iter()
                .map(|item| match item {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect(),
        ),
        _ => None,
    }
}

/// Extract file operations from messages and previous compaction entries.
fn extract_file_operations(
    messages: &[AgentMessage],
    prev_compaction: Option<&CompactionEntry>,
) -> FileOperations {
    let mut file_ops = create_file_ops();

    // Collect from previous compaction's details (if pi-generated)
    if let Some(prev_compaction) = prev_compaction
        && !prev_compaction.from_hook.unwrap_or(false)
        && let Some(details) = prev_compaction.details.as_ref().filter(|d| !d.is_null())
    {
        // fromHook field kept for session file compatibility
        if let Some(read_files) = string_array(details.get("readFiles")) {
            file_ops.read.extend(read_files);
        }
        if let Some(modified_files) = string_array(details.get("modifiedFiles")) {
            file_ops.edited.extend(modified_files);
        }
    }

    // Extract from tool calls in messages
    for msg in messages {
        extract_file_ops_from_message(msg, &mut file_ops);
    }

    file_ops
}

// ============================================================================
// Message Extraction
// ============================================================================

/// Extract AgentMessage from an entry if it produces one.
/// Returns undefined for entries that don't contribute to LLM context.
fn get_messages_from_projected_entry_for_compaction(entry: &ProjectedSessionEntry) -> Vec<AgentMessage> {
    if matches!(entry.source_entry, SessionEntry::Compaction(_)) {
        return Vec::new();
    }
    // System messages are prompt state, not conversation; the compaction entry carries their replay.
    entry
        .messages
        .iter()
        .filter(|message| message.role() != "system")
        .cloned()
        .collect()
}

/// Result from compact() - SessionManager adds uuid/parentUuid when saving
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionResult {
    pub summary: String,
    pub first_kept_entry_id: String,
    pub tokens_before: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_tokens_after: Option<u64>,
    /// Usage from the LLM call(s) that generated this summary, if available
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Extension-specific data (e.g., ArtifactIndex, version markers for structured compaction)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

// ============================================================================
// Types
// ============================================================================

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSettings {
    pub enabled: bool,
    pub reserve_tokens: u64,
    pub keep_recent_tokens: u64,
}

impl Default for CompactionSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            reserve_tokens: 16384,
            keep_recent_tokens: 20000,
        }
    }
}

pub static DEFAULT_COMPACTION_SETTINGS: LazyLock<CompactionSettings> =
    LazyLock::new(CompactionSettings::default);

// ============================================================================
// Token calculation
// ============================================================================

/// Calculate total context tokens from usage.
/// Uses the native totalTokens field when available, falls back to computing from components.
pub fn calculate_context_tokens(usage: &Usage) -> u64 {
    if usage.total_tokens != 0 {
        return usage.total_tokens;
    }
    usage.input + usage.output + usage.cache_read + usage.cache_write
}

/// Get usage from an assistant message if available.
/// Skips aborted, error, and all-zero usage messages as they don't have valid usage data.
fn get_assistant_usage(msg: &AgentMessage) -> Option<&Usage> {
    let AgentMessage::Assistant(assistant) = msg else {
        return None;
    };
    if assistant.stop_reason != StopReason::Aborted
        && assistant.stop_reason != StopReason::Error
        && calculate_context_tokens(&assistant.usage) > 0
    {
        return Some(&assistant.usage);
    }
    None
}

/// Find the last valid assistant message usage from session entries.
pub fn get_last_assistant_usage(entries: &[SessionEntry]) -> Option<Usage> {
    entries.iter().rev().find_map(|entry| match entry {
        SessionEntry::Message(m) => get_assistant_usage(&m.message).cloned(),
        _ => None,
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsageEstimate {
    pub tokens: u64,
    pub usage_tokens: u64,
    pub trailing_tokens: u64,
    #[serde(default)]
    pub last_usage_index: Option<i64>,
}

fn get_last_assistant_usage_info(messages: &[AgentMessage]) -> Option<(&Usage, usize)> {
    messages
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, message)| get_assistant_usage(message).map(|usage| (usage, index)))
}

/// Estimate context tokens from messages, using the last assistant usage when available.
/// If there are messages after the last usage, estimate their tokens with estimateTokens.
pub fn estimate_context_tokens(messages: &[AgentMessage]) -> ContextUsageEstimate {
    let Some((usage, index)) = get_last_assistant_usage_info(messages) else {
        let estimated: u64 = messages.iter().map(estimate_tokens).sum();
        return ContextUsageEstimate {
            tokens: estimated,
            usage_tokens: 0,
            trailing_tokens: estimated,
            last_usage_index: None,
        };
    };

    let usage_tokens = calculate_context_tokens(usage);
    let trailing_tokens: u64 = messages[index + 1..].iter().map(estimate_tokens).sum();

    ContextUsageEstimate {
        tokens: usage_tokens + trailing_tokens,
        usage_tokens,
        trailing_tokens,
        last_usage_index: Some(index as i64),
    }
}

/// Estimate projected context without trusting usage captured before a later edit or compaction.
pub fn estimate_projected_context_tokens(
    projection: &SessionProjection,
    branch_entries: &[SessionEntry],
) -> ContextUsageEstimate {
    let estimate = estimate_context_tokens(&projection.messages);
    if let Some(last_usage_index) = estimate.last_usage_index {
        let last_usage_index = last_usage_index as usize;
        let mut projected_message_index = 0usize;
        let mut usage_entry_id: Option<&str> = None;
        for entry in &projection.entries {
            let next_message_index = projected_message_index + entry.messages.len();
            if last_usage_index < next_message_index {
                usage_entry_id = Some(entry.source_entry.id());
                break;
            }
            projected_message_index = next_message_index;
        }

        let usage_entry_index: i64 = usage_entry_id
            .filter(|id| !id.is_empty())
            .and_then(|id| branch_entries.iter().position(|entry| entry.id() == id))
            .map(|i| i as i64)
            .unwrap_or(-1);
        let latest_invalidating_entry_index: i64 = branch_entries
            .iter()
            .rposition(|entry| {
                matches!(entry, SessionEntry::ContextEdit(_) | SessionEntry::Compaction(_))
            })
            .map(|i| i as i64)
            .unwrap_or(-1);
        if usage_entry_index > latest_invalidating_entry_index {
            return estimate;
        }
    }

    let system_messages: Vec<Message> = projection
        .messages
        .iter()
        .filter_map(|m| m.as_system().cloned().map(Message::System))
        .collect();
    let current_system = pi_ai::get_current_system_message(&system_messages);
    let mut tokens = current_system
        .map(|system| estimate_tokens(&AgentMessage::System(system)))
        .unwrap_or(0);
    for message in &projection.messages {
        if message.role() != "system" {
            tokens += estimate_tokens(message);
        }
    }
    ContextUsageEstimate {
        tokens,
        usage_tokens: 0,
        trailing_tokens: tokens,
        last_usage_index: None,
    }
}

/// Check if compaction should trigger based on context usage.
pub fn should_compact(context_tokens: u64, context_window: u64, settings: &CompactionSettings) -> bool {
    if !settings.enabled {
        return false;
    }
    (context_tokens as i128) > (context_window as i128) - (settings.reserve_tokens as i128)
}

// ============================================================================
// Cut point detection
// ============================================================================

const ESTIMATED_IMAGE_CHARS: usize = 4800;

fn estimate_user_content_chars(content: &UserContent) -> usize {
    match content {
        UserContent::Text(text) => js_len(text),
        UserContent::Blocks(blocks) => estimate_blocks_chars(blocks),
    }
}

fn estimate_blocks_chars(blocks: &[UserContentBlock]) -> usize {
    blocks
        .iter()
        .map(|block| match block {
            UserContentBlock::Text(t) => js_len(&t.text),
            UserContentBlock::Image(_) => ESTIMATED_IMAGE_CHARS,
        })
        .sum()
}

/// Content chars of a raw JSON content value (`string | block[]`).
fn estimate_value_content_chars(content: Option<&Value>) -> usize {
    match content {
        Some(Value::String(text)) => js_len(text),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .map(|block| match block.get("type").and_then(Value::as_str) {
                Some("text") => block.get("text").and_then(Value::as_str).map(js_len).unwrap_or(0),
                Some("image") => ESTIMATED_IMAGE_CHARS,
                _ => 0,
            })
            .sum(),
        _ => 0,
    }
}

fn value_str_len(value: Option<&Value>) -> usize {
    value.and_then(Value::as_str).map(js_len).unwrap_or(0)
}

fn ceil_div4(chars: usize) -> u64 {
    chars.div_ceil(4) as u64
}

/// Estimate token count for a message using chars/4 heuristic.
/// This is conservative (overestimates tokens).
pub fn estimate_tokens(message: &AgentMessage) -> u64 {
    match message {
        AgentMessage::System(system) => {
            let mut chars = match &system.content {
                SystemContent::Text(text) => js_len(text),
                SystemContent::Blocks(blocks) => blocks.iter().map(|b| js_len(b.text())).sum(),
            };
            if let Some(sections) = &system.sections {
                for section in sections.values().flatten() {
                    chars += js_len(section);
                }
            }
            if let Some(tools) = &system.tools_added {
                chars += serde_json::to_string(tools).map(|s| js_len(&s)).unwrap_or(0);
            }
            ceil_div4(chars)
        }
        AgentMessage::User(user) => ceil_div4(estimate_user_content_chars(&user.content)),
        AgentMessage::Assistant(assistant) => {
            let mut chars = 0usize;
            for block in &assistant.content {
                match block {
                    AssistantContent::Text(t) => chars += js_len(&t.text),
                    AssistantContent::Thinking(t) => chars += js_len(&t.thinking),
                    AssistantContent::ToolCall(call) => {
                        chars += js_len(&call.name)
                            + serde_json::to_string(&call.arguments).map(|s| js_len(&s)).unwrap_or(0);
                    }
                }
            }
            ceil_div4(chars)
        }
        AgentMessage::ToolResult(result) => ceil_div4(estimate_blocks_chars(&result.content)),
        AgentMessage::Custom(custom) => match custom.role.as_str() {
            "custom" => ceil_div4(estimate_value_content_chars(custom.get("content"))),
            "bashExecution" => {
                ceil_div4(value_str_len(custom.get("command")) + value_str_len(custom.get("output")))
            }
            "branchSummary" | "compactionSummary" => ceil_div4(value_str_len(custom.get("summary"))),
            _ => 0,
        },
    }
}

fn is_cut_point_message(message: &AgentMessage) -> bool {
    matches!(
        message.role(),
        "user" | "assistant" | "bashExecution" | "custom" | "branchSummary" | "compactionSummary"
    )
}

fn is_turn_start_message(message: &AgentMessage) -> bool {
    matches!(
        message.role(),
        "user" | "bashExecution" | "custom" | "branchSummary" | "compactionSummary"
    )
}

fn is_turn_start_entry(entry: &SessionEntry) -> bool {
    if matches!(entry, SessionEntry::Compaction(_)) {
        return false;
    }
    session_entry_to_context_messages(entry)
        .iter()
        .any(is_turn_start_message)
}

/// Find valid cut points: indices of context-visible user-like or assistant messages.
/// Never cut at tool results (they must follow their tool call).
/// When we cut at an assistant message with tool calls, its tool results follow it
/// and will be kept.
fn find_valid_cut_points(entries: &[SessionEntry], start_index: usize, end_index: usize) -> Vec<usize> {
    (start_index..end_index.min(entries.len()))
        .filter(|&i| {
            let entry = &entries[i];
            !matches!(entry, SessionEntry::Compaction(_))
                && session_entry_to_context_messages(entry)
                    .iter()
                    .any(is_cut_point_message)
        })
        .collect()
}

/// Find the context-visible user-role message that starts the turn containing the given entry index.
/// Returns -1 if no turn start found before the index.
pub fn find_turn_start_index(entries: &[SessionEntry], entry_index: i64, start_index: i64) -> i64 {
    let mut i = entry_index;
    while i >= start_index && i >= 0 {
        if let Some(entry) = entries.get(i as usize)
            && is_turn_start_entry(entry)
        {
            return i;
        }
        i -= 1;
    }
    -1
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutPointResult {
    /// Index of first entry to keep
    pub first_kept_entry_index: i64,
    /// Index of user message that starts the turn being split, or -1 if not splitting
    pub turn_start_index: i64,
    /// Whether this cut splits a turn (cut point is not a user message)
    pub is_split_turn: bool,
}

/// Find the cut point in session entries that keeps approximately `keepRecentTokens`.
///
/// Algorithm: Walk backwards from newest, accumulating estimated message sizes.
/// Stop when we've accumulated >= keepRecentTokens. Cut at that point.
///
/// Can cut at user OR assistant messages (never tool results). When cutting at an
/// assistant message with tool calls, its tool results come after and will be kept.
///
/// Returns CutPointResult with:
/// - firstKeptEntryIndex: the entry index to start keeping from
/// - turnStartIndex: if cutting mid-turn, the user message that started that turn
/// - isSplitTurn: whether we're cutting in the middle of a turn
///
/// Only considers entries between `startIndex` and `endIndex` (exclusive).
pub fn find_cut_point(
    entries: &[SessionEntry],
    start_index: i64,
    end_index: i64,
    keep_recent_tokens: u64,
) -> CutPointResult {
    let start = start_index.max(0) as usize;
    let end = end_index.max(0) as usize;
    let cut_points = find_valid_cut_points(entries, start, end);

    if cut_points.is_empty() {
        return CutPointResult {
            first_kept_entry_index: start_index,
            turn_start_index: -1,
            is_split_turn: false,
        };
    }

    // Walk backwards from newest, accumulating estimated message sizes
    let mut accumulated_tokens = 0u64;
    let mut cut_index = cut_points[0]; // Default: keep from first message (not header)

    for i in (start..end.min(entries.len())).rev() {
        let message_tokens: u64 = session_entry_to_context_messages(&entries[i])
            .iter()
            .map(estimate_tokens)
            .sum();
        if message_tokens == 0 {
            continue;
        }
        accumulated_tokens += message_tokens;

        // Check if we've exceeded the budget
        if accumulated_tokens >= keep_recent_tokens {
            // Prefer the closest valid cut point at or after this entry. If trailing
            // tool results exceed the budget by themselves, keep their preceding
            // assistant tool call instead of falling back to the first message.
            cut_index = cut_points
                .iter()
                .copied()
                .find(|&candidate| candidate >= i)
                .unwrap_or(*cut_points.last().expect("non-empty"));
            break;
        }
    }

    // Scan backwards from cutIndex to include adjacent metadata entries that do not affect context.
    while cut_index > start {
        let prev_entry = &entries[cut_index - 1];
        // Stop at compaction boundaries or context-visible entries.
        if matches!(prev_entry, SessionEntry::Compaction(_))
            || !session_entry_to_context_messages(prev_entry).is_empty()
        {
            break;
        }
        cut_index -= 1;
    }

    // Determine if this is a split turn
    let starts_turn = is_turn_start_entry(&entries[cut_index]);
    let turn_start_index = if starts_turn {
        -1
    } else {
        find_turn_start_index(entries, cut_index as i64, start_index)
    };

    CutPointResult {
        first_kept_entry_index: cut_index as i64,
        turn_start_index,
        is_split_turn: !starts_turn && turn_start_index != -1,
    }
}

// ============================================================================
// Summarization
// ============================================================================

const SUMMARIZATION_PROMPT: &str = r#"The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.

Use this EXACT format:

## Goal
[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned by user]
- [Or "(none)" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Current work]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [Ordered list of what should happen next]

## Critical Context
- [Any data, examples, or references needed to continue]
- [Or "(none)" if not applicable]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

const UPDATE_SUMMARIZATION_INSTRUCTIONS: &str = r#"Update the existing structured summary with new information. RULES:
- PRESERVE all existing information from the previous summary
- ADD new progress, decisions, and context from the new messages
- UPDATE the Progress section: move items from "In Progress" to "Done" when completed
- UPDATE "Next Steps" based on what was accomplished
- PRESERVE exact file paths, function names, and error messages
- If something is no longer relevant, you may remove it

Use this EXACT format:

## Goal
[Preserve existing goals, add new ones if the task expanded]

## Constraints & Preferences
- [Preserve existing, add new ones discovered]

## Progress
### Done
- [x] [Include previously done items AND newly completed items]

### In Progress
- [ ] [Current work - update based on progress]

### Blocked
- [Current blockers - remove if resolved]

## Key Decisions
- **[Decision]**: [Brief rationale] (preserve all previous, add new)

## Next Steps
1. [Update based on current state]

## Critical Context
- [Preserve important context, add new if needed]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

static UPDATE_SUMMARIZATION_PROMPT: LazyLock<String> = LazyLock::new(|| {
    format!(
        "The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.\n\n{UPDATE_SUMMARIZATION_INSTRUCTIONS}"
    )
});

/// Returns an error message when a summarization response cannot safely be persisted.
/// A length stop contains partial text and must not become a session checkpoint.
pub fn get_summarization_failure(response: &AssistantMessage, label: &str) -> Option<String> {
    match response.stop_reason {
        StopReason::Error => Some(format!(
            "{label} failed: {}",
            response
                .error_message
                .as_deref()
                .filter(|m| !m.is_empty())
                .unwrap_or("Unknown error")
        )),
        StopReason::Length => Some(format!(
            "{label} failed: generation hit the token cap and the summary is incomplete"
        )),
        _ => None,
    }
}

pub(crate) fn to_provider_headers(
    headers: Option<&IndexMap<String, String>>,
) -> Option<pi_ai::types::ProviderHeaders> {
    headers.map(|headers| {
        headers
            .iter()
            .map(|(k, v)| (k.clone(), Some(v.clone())))
            .collect()
    })
}

pub(crate) fn to_provider_env(
    env: Option<&IndexMap<String, String>>,
) -> Option<pi_ai::types::ProviderEnv> {
    env.map(|env| env.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
}

#[allow(clippy::too_many_arguments)]
fn create_summarization_options(
    model: &Model,
    api_key: Option<&str>,
    headers: Option<&IndexMap<String, String>>,
    env: Option<&IndexMap<String, String>>,
    signal: Option<&CancellationToken>,
    thinking_level: Option<ThinkingLevel>,
    session_id: Option<&str>,
) -> SimpleStreamOptions {
    let mut options = SimpleStreamOptions {
        signal: signal.cloned(),
        api_key: api_key.map(str::to_string),
        headers: to_provider_headers(headers),
        env: to_provider_env(env),
        session_id: session_id.map(str::to_string),
        ..Default::default()
    };
    if model.reasoning
        && let Some(level) = thinking_level.and_then(|level| level.thinking())
    {
        options.reasoning = Some(level);
    }
    options
}

/// Shared choke point for every compaction/branch-summary summarization call. Wraps the
/// single LLM call in {@link retryAssistantCall} so transient stream drops (e.g.
/// `terminated`, socket close) honor the configured retry policy instead of failing
/// the whole compaction on the first attempt. Deterministic errors and aborts return
/// immediately (see {@link retryAssistantCall}).
pub async fn complete_summarization(
    model: &Model,
    context: TranscriptContext,
    options: SimpleStreamOptions,
    stream_fn: Option<StreamFn>,
    retry: Option<&RetryPolicy>,
    callbacks: Option<&RetryCallbacks>,
) -> anyhow::Result<AssistantMessage> {
    // Avoid cache writes for one-off summaries. Reuse caller-supplied routing when available;
    // callers without a session ID, including branch summaries, receive a fresh routing ID.
    let mut request_options = options;
    request_options.cache_retention = Some(CacheRetention::None);
    if request_options.session_id.is_none() {
        request_options.session_id = Some(pi_ai::uuidv7());
    }
    let signal = request_options.signal.clone();
    // A thrown stream function error (TS rejection) ends the call without retrying.
    let thrown: Arc<parking_lot::Mutex<Option<anyhow::Error>>> = Arc::new(parking_lot::Mutex::new(None));
    let produce = || {
        let model = model.clone();
        let context = context.clone();
        let request_options = request_options.clone();
        let stream_fn = stream_fn.clone();
        let thrown = thrown.clone();
        async move {
            match stream_fn {
                Some(stream_fn) => match stream_fn(model.clone(), context, request_options).await {
                    Ok(stream) => stream.result().await,
                    Err(error) => {
                        *thrown.lock() = Some(error);
                        let mut message = AssistantMessage::empty_for(&model, chrono::Utc::now().timestamp_millis());
                        message.stop_reason = StopReason::Aborted;
                        message
                    }
                },
                None => {
                    let context = Context {
                        system_prompt: None,
                        messages: context.messages,
                        tools: None,
                    };
                    pi_ai::compat::complete_simple(&model, &context, request_options).await
                }
            }
        }
    };
    let response = pi_ai::retry_assistant_call(produce, retry, signal.as_ref(), callbacks).await;
    if let Some(error) = thrown.lock().take() {
        return Err(error);
    }
    Ok(response)
}

/// Generate a summary of the conversation using the LLM.
/// If previousSummary is provided, uses the update prompt to merge.
#[allow(clippy::too_many_arguments)]
pub async fn generate_summary(
    current_messages: &[AgentMessage],
    model: &Model,
    // Unused. Kept for existing callers; summaries use the model's output limit (#9512).
    reserve_tokens: Option<u64>,
    api_key: Option<&str>,
    headers: Option<&IndexMap<String, String>>,
    signal: Option<CancellationToken>,
    custom_instructions: Option<&str>,
    previous_summary: Option<&str>,
    thinking_level: Option<ThinkingLevel>,
    stream_fn: Option<StreamFn>,
    env: Option<&IndexMap<String, String>>,
    retry: Option<&RetryPolicy>,
    callbacks: Option<&RetryCallbacks>,
    session_id: Option<&str>,
) -> anyhow::Result<String> {
    Ok(generate_summary_with_usage(
        current_messages,
        model,
        reserve_tokens,
        api_key,
        headers,
        signal,
        custom_instructions,
        previous_summary,
        thinking_level,
        stream_fn,
        env,
        retry,
        callbacks,
        session_id,
    )
    .await?
    .text)
}

/// Build the provider context for a standalone summary request.
fn build_summarization_context(prompt_text: String) -> TranscriptContext {
    pi_ai::normalize_context_owned(Context {
        system_prompt: Some(SUMMARIZATION_SYSTEM_PROMPT.to_string()),
        messages: vec![Message::User(UserMessage::new(
            UserContent::Blocks(vec![UserContentBlock::text(prompt_text)]),
            chrono::Utc::now().timestamp_millis(),
        ))],
        tools: None,
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateSummaryWithUsageResult {
    pub text: String,
    pub usage: Usage,
}

/// Generate or update a conversation summary and return its provider usage.
#[allow(clippy::too_many_arguments)]
pub async fn generate_summary_with_usage(
    current_messages: &[AgentMessage],
    model: &Model,
    // Unused. Kept for existing callers; summaries use the model's output limit (#9512).
    _reserve_tokens: Option<u64>,
    api_key: Option<&str>,
    headers: Option<&IndexMap<String, String>>,
    signal: Option<CancellationToken>,
    custom_instructions: Option<&str>,
    previous_summary: Option<&str>,
    thinking_level: Option<ThinkingLevel>,
    stream_fn: Option<StreamFn>,
    env: Option<&IndexMap<String, String>>,
    retry: Option<&RetryPolicy>,
    callbacks: Option<&RetryCallbacks>,
    session_id: Option<&str>,
) -> anyhow::Result<GenerateSummaryWithUsageResult> {
    let previous_summary = previous_summary.filter(|s| !s.is_empty());
    // Use update prompt if we have a previous summary, otherwise initial prompt
    let mut base_prompt = if previous_summary.is_some() {
        UPDATE_SUMMARIZATION_PROMPT.clone()
    } else {
        SUMMARIZATION_PROMPT.to_string()
    };
    if let Some(custom_instructions) = custom_instructions.filter(|s| !s.is_empty()) {
        base_prompt = format!("{base_prompt}\n\nAdditional focus: {custom_instructions}");
    }

    // Serialize conversation to text so model doesn't try to continue it
    // Convert to LLM messages first (handles custom types like bashExecution, custom, etc.)
    let llm_messages = convert_to_llm(current_messages);
    let conversation_text = serialize_conversation(&llm_messages);

    // Build the prompt with conversation wrapped in tags
    let mut prompt_text = format!("<conversation>\n{conversation_text}\n</conversation>\n\n");
    if let Some(previous_summary) = previous_summary {
        prompt_text.push_str(&format!("<previous-summary>\n{previous_summary}\n</previous-summary>\n\n"));
    }
    prompt_text.push_str(&base_prompt);

    let completion_options = create_summarization_options(
        model,
        api_key,
        headers,
        env,
        signal.as_ref(),
        thinking_level,
        session_id,
    );

    let response = complete_summarization(
        model,
        build_summarization_context(prompt_text),
        completion_options,
        stream_fn,
        retry,
        callbacks,
    )
    .await?;

    if let Some(failure) = get_summarization_failure(&response, "Summarization") {
        anyhow::bail!(failure);
    }
    if response.tool_calls().next().is_some() {
        anyhow::bail!("Summarization attempted to call a tool");
    }

    let text_content = pi_ai::assistant_content_text(&response.content, "\n");

    Ok(GenerateSummaryWithUsageResult {
        text: text_content,
        usage: response.usage,
    })
}

// ============================================================================
// Compaction Preparation (for extensions)
// ============================================================================

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionPreparation {
    /// UUID of first entry to keep
    pub first_kept_entry_id: String,
    /// Messages that will be summarized and discarded
    pub messages_to_summarize: Vec<AgentMessage>,
    /// Messages that will be turned into turn prefix summary (if splitting)
    pub turn_prefix_messages: Vec<AgentMessage>,
    /// Whether this is a split turn (cut point in middle of turn)
    pub is_split_turn: bool,
    pub tokens_before: u64,
    /// Summary from previous compaction, for iterative update
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_summary: Option<String>,
    /// File operations extracted from messagesToSummarize
    pub file_ops: FileOperations,
    /// Compaction settions from settings.jsonl
    pub settings: CompactionSettings,
}

fn is_projected_turn_start(entry: &ProjectedSessionEntry) -> bool {
    if matches!(entry.source_entry, SessionEntry::Compaction(_)) {
        return false;
    }
    entry.messages.iter().any(is_turn_start_message)
}

fn find_projected_turn_start_index(entries: &[ProjectedSessionEntry], entry_index: usize, start_index: usize) -> i64 {
    let mut i = entry_index as i64;
    while i >= start_index as i64 {
        if is_projected_turn_start(&entries[i as usize]) {
            return i;
        }
        i -= 1;
    }
    -1
}

fn find_projected_cut_point(
    entries: &[ProjectedSessionEntry],
    start_index: usize,
    end_index: usize,
    keep_recent_tokens: u64,
) -> CutPointResult {
    let cut_points: Vec<usize> = (start_index..end_index)
        .filter(|&i| {
            let entry = &entries[i];
            !matches!(entry.source_entry, SessionEntry::Compaction(_))
                && entry.messages.iter().any(is_cut_point_message)
        })
        .collect();
    if cut_points.is_empty() {
        return CutPointResult {
            first_kept_entry_index: start_index as i64,
            turn_start_index: -1,
            is_split_turn: false,
        };
    }

    let mut accumulated_tokens = 0u64;
    let mut exceeded_budget = false;
    let mut cut_index = cut_points[0];
    for i in (start_index..end_index).rev() {
        let message_tokens: u64 = entries[i].messages.iter().map(estimate_tokens).sum();
        if message_tokens == 0 {
            continue;
        }
        accumulated_tokens += message_tokens;
        if accumulated_tokens >= keep_recent_tokens {
            exceeded_budget = true;
            cut_index = cut_points
                .iter()
                .copied()
                .find(|&candidate| candidate >= i)
                .unwrap_or(*cut_points.last().expect("non-empty"));
            break;
        }
    }

    // A recovery attempt and its omission edits are context-invisible after the last
    // visible input. Advance only for a closed suffix containing an omitted assistant
    // attempt; arbitrary metadata must not move the cut past unsent input.
    let suffix = &entries[(cut_index + 1).min(end_index)..end_index];
    let is_intrinsically_visible = |entry: &ProjectedSessionEntry| -> bool {
        !matches!(entry.source_entry, SessionEntry::ContextEdit(_))
            && !session_entry_to_context_messages(&entry.source_entry).is_empty()
    };
    let is_omitted =
        |entry: &ProjectedSessionEntry| -> bool { is_intrinsically_visible(entry) && entry.messages.is_empty() };
    let omitted_suffix_ids: HashSet<&str> = suffix
        .iter()
        .filter(|entry| is_omitted(entry))
        .map(|entry| entry.source_entry.id())
        .collect();
    let has_external_replacement = suffix.iter().any(|entry| match &entry.source_entry {
        SessionEntry::ContextEdit(edit) => {
            edit.replacement.is_some() && !omitted_suffix_ids.contains(edit.target_id.as_str())
        }
        _ => false,
    });
    let is_recovery_omission_suffix = exceeded_budget
        && !has_external_replacement
        && suffix.iter().any(|entry| {
            matches!(&entry.source_entry, SessionEntry::Message(m) if m.message.role() == "assistant")
                && is_omitted(entry)
        })
        && suffix.iter().all(|entry| {
            !matches!(entry.source_entry, SessionEntry::Compaction(_))
                && (!is_intrinsically_visible(entry) || is_omitted(entry))
        });
    if is_recovery_omission_suffix {
        cut_index += 1;
    }

    while cut_index > start_index {
        let previous = &entries[cut_index - 1];
        if matches!(previous.source_entry, SessionEntry::Compaction(_)) || !previous.messages.is_empty() {
            break;
        }
        cut_index -= 1;
    }
    // The recovery advance only happens with a non-empty suffix, so cut_index stays in range.
    let starts_turn = is_projected_turn_start(&entries[cut_index]);
    let turn_start_index = if starts_turn {
        -1
    } else {
        find_projected_turn_start_index(entries, cut_index, start_index)
    };
    CutPointResult {
        first_kept_entry_index: cut_index as i64,
        turn_start_index,
        is_split_turn: !starts_turn && turn_start_index != -1,
    }
}

pub fn prepare_compaction(
    path_entries: &[SessionEntry],
    settings: &CompactionSettings,
) -> Option<CompactionPreparation> {
    if matches!(path_entries.last(), Some(SessionEntry::Compaction(_))) {
        return None;
    }

    let projection = build_session_projection(path_entries, None, None);
    let projected_entries = &projection.entries;
    // The newest compaction is projected first. Older compaction entries can still
    // occur in its retained raw range, but their projected contribution is empty.
    let prev_compaction_index = projected_entries.iter().position(|entry| {
        matches!(entry.source_entry, SessionEntry::Compaction(_)) && !entry.messages.is_empty()
    });

    let mut previous_summary: Option<String> = None;
    let mut boundary_start = 0usize;
    let mut prev_compaction: Option<&CompactionEntry> = None;
    if let Some(index) = prev_compaction_index
        && let SessionEntry::Compaction(compaction) = &projected_entries[index].source_entry
    {
        previous_summary = Some(compaction.summary.clone());
        prev_compaction = Some(compaction);
        // The canonical projection has already selected the previous compaction's retained tail.
        boundary_start = index + 1;
    }
    let boundary_end = projected_entries.len();
    let tokens_before = estimate_projected_context_tokens(&projection, path_entries).tokens;
    let cut_point = find_projected_cut_point(
        projected_entries,
        boundary_start,
        boundary_end,
        settings.keep_recent_tokens,
    );

    let first_kept_index = cut_point.first_kept_entry_index as usize;
    let first_kept_entry = projected_entries.get(first_kept_index).map(|e| &e.source_entry)?;
    if first_kept_entry.id().is_empty() {
        return None;
    }
    let first_kept_entry_id = first_kept_entry.id().to_string();
    let history_end = if cut_point.is_split_turn {
        cut_point.turn_start_index as usize
    } else {
        first_kept_index
    };

    let messages_to_summarize: Vec<AgentMessage> = projected_entries
        [boundary_start.min(history_end)..history_end]
        .iter()
        .flat_map(get_messages_from_projected_entry_for_compaction)
        .collect();
    let turn_prefix_messages: Vec<AgentMessage> = if cut_point.is_split_turn {
        projected_entries[cut_point.turn_start_index as usize..first_kept_index]
            .iter()
            .flat_map(get_messages_from_projected_entry_for_compaction)
            .collect()
    } else {
        Vec::new()
    };

    if messages_to_summarize.is_empty() && turn_prefix_messages.is_empty() {
        return None;
    }

    // Extract file operations from edited model-visible messages and the previous compaction.
    let mut file_ops = extract_file_operations(&messages_to_summarize, prev_compaction);

    // Also extract file ops from turn prefix if splitting
    if cut_point.is_split_turn {
        for msg in &turn_prefix_messages {
            extract_file_ops_from_message(msg, &mut file_ops);
        }
    }

    Some(CompactionPreparation {
        first_kept_entry_id,
        messages_to_summarize,
        turn_prefix_messages,
        is_split_turn: cut_point.is_split_turn,
        tokens_before,
        previous_summary,
        file_ops,
        settings: settings.clone(),
    })
}

// ============================================================================
// Chat-prefix summarization (pi-rs divergence, LM-19)
// ============================================================================

/// The chat context a compaction summary request can replay (`compaction.reuseChatPrefix`).
///
/// A local server (llama.cpp, NInfer) keeps the state of the last chat request. A summary
/// request that sends the chat's own messages, system messages and tool declarations unchanged
/// and adds one instruction message looks to the server like the next chat turn: it prefills
/// only the instruction. TypeScript pi's standalone summary request shares no prefix with the
/// chat and must prefill the serialized conversation from scratch. Hybrid models (recurrent or
/// convolutional layers, as Bonsai and LFM) can only resume from the end of a cached request or
/// from a server checkpoint, not from an arbitrary earlier position, so the request replays the
/// whole context rather than only the part being summarized.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CompactionChatPrefix {
    /// The agent's model-visible context messages (system messages included), in order.
    pub messages: Vec<AgentMessage>,
}

/// The chat context of the branch `path_entries` ends in, as the agent sends it.
pub fn prepare_compaction_chat_prefix(path_entries: &[SessionEntry]) -> Option<CompactionChatPrefix> {
    let messages = build_session_projection(path_entries, None, None).messages;
    (!messages.is_empty()).then_some(CompactionChatPrefix { messages })
}

/// Leads the chat-prefix instruction: the request continues the chat, so the model must be told
/// explicitly to stop working on it. Carries the standalone request's system prompt.
const CHAT_PREFIX_PREAMBLE: &str = "Stop working on the conversation above. Do NOT continue it, do NOT respond to any questions in it and do NOT call any tools. Your only task now is the summary described below; ONLY output that summary.";

static CHAT_PREFIX_UPDATE_SUMMARIZATION_PROMPT: LazyLock<String> = LazyLock::new(|| {
    format!(
        "The messages above are a conversation to summarize. It starts with the existing summary of earlier history in <summary> tags; incorporate the messages after it into that summary.\n\n{UPDATE_SUMMARIZATION_INSTRUCTIONS}"
    )
});

/// The trailing instruction of a chat-prefix summary request.
pub fn chat_prefix_summarization_instruction(has_previous_summary: bool, custom_instructions: Option<&str>) -> String {
    let prompt = if has_previous_summary {
        CHAT_PREFIX_UPDATE_SUMMARIZATION_PROMPT.as_str()
    } else {
        SUMMARIZATION_PROMPT
    };
    let instruction = format!("{CHAT_PREFIX_PREAMBLE}\n\n{prompt}");
    match custom_instructions.filter(|s| !s.is_empty()) {
        Some(custom_instructions) => format!("{instruction}\n\nAdditional focus: {custom_instructions}"),
        None => instruction,
    }
}

/// Request parameters shared by every summary request of one compaction.
struct SummaryRequest<'a> {
    model: &'a Model,
    api_key: Option<&'a str>,
    headers: Option<&'a IndexMap<String, String>>,
    env: Option<&'a IndexMap<String, String>>,
    signal: Option<CancellationToken>,
    thinking_level: Option<ThinkingLevel>,
    stream_fn: Option<StreamFn>,
    retry: Option<&'a RetryPolicy>,
    callbacks: Option<&'a RetryCallbacks>,
    session_id: Option<&'a str>,
}

impl SummaryRequest<'_> {
    fn options(&self) -> SimpleStreamOptions {
        create_summarization_options(
            self.model,
            self.api_key,
            self.headers,
            self.env,
            self.signal.as_ref(),
            self.thinking_level,
            self.session_id,
        )
    }
}

/// Outcome of a chat-prefix summary request that cannot be used as the summary.
struct ChatPrefixRejected {
    /// Usage of a response that arrived but was unusable (tool call, no text).
    usage: Option<Usage>,
}

/// Summarize `prefix` (the chat context, converted for the LLM) followed by `instruction` as a
/// new user message.
async fn summarize_chat_prefix(
    request: &SummaryRequest<'_>,
    prefix: Vec<Message>,
    instruction: String,
) -> anyhow::Result<Result<GenerateSummaryWithUsageResult, ChatPrefixRejected>> {
    let mut messages = prefix;
    messages.push(Message::User(UserMessage::new(
        UserContent::Blocks(vec![UserContentBlock::text(instruction)]),
        chrono::Utc::now().timestamp_millis(),
    )));
    let response = complete_summarization(
        request.model,
        pi_ai::utils::transcript::transcript_from_messages(messages),
        request.options(),
        request.stream_fn.clone(),
        request.retry,
        request.callbacks,
    )
    .await?;
    if get_summarization_failure(&response, "Summarization").is_some() {
        return Ok(Err(ChatPrefixRejected { usage: None }));
    }
    let text = pi_ai::assistant_content_text(&response.content, "\n");
    if response.tool_calls().next().is_some() || text.trim().is_empty() {
        return Ok(Err(ChatPrefixRejected {
            usage: Some(response.usage),
        }));
    }
    Ok(Ok(GenerateSummaryWithUsageResult {
        text,
        usage: response.usage,
    }))
}

// ============================================================================
// Main compaction function
// ============================================================================

const TURN_PREFIX_SUMMARIZATION_PROMPT: &str = r#"The messages above are earlier context from an ongoing conversation. Later messages are stored separately and do not need to be reconstructed.

Create a concise checkpoint of the user's request and the progress shown above. This checkpoint will be placed before the later messages so the conversation can continue with the necessary context.

## Original Request
[What did the user ask for?]

## Progress So Far
- [Key decisions and work completed in these messages]

## Context Needed to Continue
- [Information from these messages needed to understand the later work]

Only summarize information explicitly present above. Do not infer or recreate later messages."#;

/// Generate summaries for compaction using prepared data.
/// Returns CompactionResult - SessionManager adds uuid/parentUuid when saving.
///
/// @param preparation - Pre-calculated preparation from prepareCompaction()
/// @param customInstructions - Optional custom focus for the summary
/// @param sessionId - Optional routing session ID forwarded without enabling prompt caching
#[allow(clippy::too_many_arguments)]
pub async fn compact(
    preparation: &CompactionPreparation,
    model: &Model,
    api_key: Option<&str>,
    headers: Option<&IndexMap<String, String>>,
    custom_instructions: Option<&str>,
    signal: Option<CancellationToken>,
    thinking_level: Option<ThinkingLevel>,
    stream_fn: Option<StreamFn>,
    env: Option<&IndexMap<String, String>>,
    retry: Option<&RetryPolicy>,
    callbacks: Option<&RetryCallbacks>,
    session_id: Option<&str>,
) -> anyhow::Result<CompactionResult> {
    let request = SummaryRequest {
        model,
        api_key,
        headers,
        env,
        signal,
        thinking_level,
        stream_fn,
        retry,
        callbacks,
        session_id,
    };
    let (summary, usage) = generate_standalone_summaries(preparation, &request, custom_instructions).await?;
    finish_compaction_result(preparation, summary, usage)
}

/// [`compact`], but the summary comes from one request that replays the chat context in
/// `chat_prefix` (converted for the LLM by `convert`, as the agent converts its context)
/// followed by the summarization instruction, so a server that cached the chat prefills only
/// the instruction. That summary covers the whole context, including the messages compaction
/// keeps, so a split turn needs no separate turn prefix summary. A chat-prefix request that
/// fails, returns no text or calls a tool falls back to [`compact`]'s requests, and the usage
/// of both is reported; an abort is not retried.
/// pi-rs divergence (LM-19, `compaction.reuseChatPrefix`).
#[allow(clippy::too_many_arguments)]
pub async fn compact_with_chat_prefix(
    preparation: &CompactionPreparation,
    chat_prefix: &CompactionChatPrefix,
    convert: &(dyn Fn(&[AgentMessage]) -> Vec<Message> + Send + Sync),
    model: &Model,
    api_key: Option<&str>,
    headers: Option<&IndexMap<String, String>>,
    custom_instructions: Option<&str>,
    signal: Option<CancellationToken>,
    thinking_level: Option<ThinkingLevel>,
    stream_fn: Option<StreamFn>,
    env: Option<&IndexMap<String, String>>,
    retry: Option<&RetryPolicy>,
    callbacks: Option<&RetryCallbacks>,
    session_id: Option<&str>,
) -> anyhow::Result<CompactionResult> {
    let request = SummaryRequest {
        model,
        api_key,
        headers,
        env,
        signal,
        thinking_level,
        stream_fn,
        retry,
        callbacks,
        session_id,
    };
    let has_previous_summary = preparation.previous_summary.as_deref().is_some_and(|s| !s.is_empty());
    let instruction = chat_prefix_summarization_instruction(has_previous_summary, custom_instructions);
    let attempt = summarize_chat_prefix(&request, convert(&chat_prefix.messages), instruction).await;
    let rejected_usage = match attempt {
        Ok(Ok(result)) => return finish_compaction_result(preparation, result.text, result.usage),
        Ok(Err(rejected)) => rejected.usage,
        Err(_) => None,
    };
    if request.signal.as_ref().is_some_and(CancellationToken::is_cancelled) {
        anyhow::bail!("Summarization aborted");
    }
    let (summary, usage) = generate_standalone_summaries(preparation, &request, custom_instructions).await?;
    let usage = match rejected_usage {
        Some(rejected) => combine_usage(&rejected, &usage),
        None => usage,
    };
    finish_compaction_result(preparation, summary, usage)
}

/// TypeScript pi's summary requests: the history summary and, for a split turn, the turn prefix
/// summary, merged into one summary text.
async fn generate_standalone_summaries(
    preparation: &CompactionPreparation,
    request: &SummaryRequest<'_>,
    custom_instructions: Option<&str>,
) -> anyhow::Result<(String, Usage)> {
    let CompactionPreparation {
        messages_to_summarize,
        turn_prefix_messages,
        is_split_turn,
        previous_summary,
        ..
    } = preparation;
    let history_summary = || {
        generate_summary_with_usage(
            messages_to_summarize,
            request.model,
            None,
            request.api_key,
            request.headers,
            request.signal.clone(),
            custom_instructions,
            previous_summary.as_deref(),
            request.thinking_level,
            request.stream_fn.clone(),
            request.env,
            request.retry,
            request.callbacks,
            request.session_id,
        )
    };

    // Generate summaries and merge into one
    if *is_split_turn && !turn_prefix_messages.is_empty() {
        let mut history_text = previous_summary
            .clone()
            .unwrap_or_else(|| "No prior history.".to_string());
        let mut history_usage: Option<Usage> = None;
        if !messages_to_summarize.is_empty() {
            let history_result = history_summary().await?;
            history_text = history_result.text;
            history_usage = Some(history_result.usage);
        }
        let turn_prefix_result = generate_turn_prefix_summary(turn_prefix_messages, request, custom_instructions).await?;
        // Merge into single summary
        let summary = format!(
            "{history_text}\n\n---\n\n**Turn Context (split turn):**\n\n{}",
            turn_prefix_result.text
        );
        let summary_usage = match history_usage {
            Some(history_usage) => combine_usage(&history_usage, &turn_prefix_result.usage),
            None => turn_prefix_result.usage,
        };
        Ok((summary, summary_usage))
    } else {
        // Just generate history summary
        let result = history_summary().await?;
        Ok((result.text, result.usage))
    }
}

/// Append the file lists to `summary` and build the result.
fn finish_compaction_result(
    preparation: &CompactionPreparation,
    mut summary: String,
    summary_usage: Usage,
) -> anyhow::Result<CompactionResult> {
    let CompactionPreparation {
        first_kept_entry_id,
        tokens_before,
        file_ops,
        ..
    } = preparation;

    // Compute file lists and append to summary
    let lists = compute_file_lists(file_ops);
    summary.push_str(&format_file_operations(&lists.read_files, &lists.modified_files));

    if first_kept_entry_id.is_empty() {
        anyhow::bail!("First kept entry has no UUID - session may need migration");
    }

    let details = CompactionDetails {
        read_files: lists.read_files,
        modified_files: lists.modified_files,
    };
    Ok(CompactionResult {
        summary,
        first_kept_entry_id: first_kept_entry_id.clone(),
        tokens_before: *tokens_before,
        estimated_tokens_after: None,
        usage: Some(summary_usage),
        details: Some(serde_json::to_value(details)?),
    })
}

/// Generate a summary for a turn prefix (when splitting a turn).
async fn generate_turn_prefix_summary(
    messages: &[AgentMessage],
    request: &SummaryRequest<'_>,
    custom_instructions: Option<&str>,
) -> anyhow::Result<GenerateSummaryWithUsageResult> {
    let llm_messages = convert_to_llm(messages);
    let conversation_text = serialize_conversation(&llm_messages);
    let mut prompt_text =
        format!("# Conversation\n{conversation_text}\n\n# Instructions\n{TURN_PREFIX_SUMMARIZATION_PROMPT}");
    if let Some(custom_instructions) = custom_instructions.filter(|s| !s.is_empty()) {
        prompt_text = format!("{prompt_text}\n\nAdditional focus: {custom_instructions}");
    }

    let response = complete_summarization(
        request.model,
        build_summarization_context(prompt_text),
        request.options(),
        request.stream_fn.clone(),
        request.retry,
        request.callbacks,
    )
    .await?;

    if let Some(failure) = get_summarization_failure(&response, "Turn prefix summarization") {
        anyhow::bail!(failure);
    }
    if response.tool_calls().next().is_some() {
        anyhow::bail!("Turn prefix summarization attempted to call a tool");
    }

    Ok(GenerateSummaryWithUsageResult {
        text: pi_ai::assistant_content_text(&response.content, "\n"),
        usage: response.usage,
    })
}
