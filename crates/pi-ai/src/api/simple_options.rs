//! Shared option handling for `stream_simple` (port of `api/simple-options.ts`).

use crate::models::clamp_thinking_level;
use crate::types::{
    Model, ModelThinkingLevel, SamplingParams, StreamOptions, ThinkingBudgets, ThinkingLevel, TranscriptContext,
};
use crate::utils::estimate::estimate_context_tokens;

const CONTEXT_SAFETY_TOKENS: i64 = 4096;
const MIN_MAX_TOKENS: i64 = 1;

/// Clamp the output budget so prompt + output fit the context window.
pub fn clamp_max_tokens_to_context(model: &Model, context: &TranscriptContext, max_tokens: u64) -> u64 {
    if model.context_window == 0 {
        return max_tokens.max(MIN_MAX_TOKENS as u64);
    }
    let available =
        model.context_window as i64 - estimate_context_tokens(&context.messages).tokens as i64 - CONTEXT_SAFETY_TOKENS;
    (max_tokens as i64).min(MIN_MAX_TOKENS.max(available)).max(0) as u64
}

/// Merge model, thinking-level and request sampling parameters.
pub fn resolve_sampling_params(
    model: &Model,
    thinking_level: ModelThinkingLevel,
    request_params: Option<&SamplingParams>,
) -> Option<SamplingParams> {
    let effective = clamp_thinking_level(model, thinking_level);
    let thinking_params = model.sampling_params_by_thinking_level.as_ref().and_then(|m| m.get(&effective));
    if model.sampling_params.is_none() && thinking_params.is_none() && request_params.is_none() {
        return None;
    }
    let mut merged = SamplingParams::new();
    for source in [model.sampling_params.as_ref(), thinking_params, request_params].into_iter().flatten() {
        for (k, v) in source {
            merged.insert(k.clone(), v.clone());
        }
    }
    Some(merged)
}

/// Base stream options derived from simple options (`buildBaseOptions`).
pub fn build_base_options(
    model: &Model,
    context: &TranscriptContext,
    options: &StreamOptions,
    api_key: Option<&str>,
) -> StreamOptions {
    let reasoning = options.reasoning.map(ModelThinkingLevel::from).unwrap_or(ModelThinkingLevel::Off);
    let sampling_params = resolve_sampling_params(model, reasoning, options.sampling_params.as_ref());
    StreamOptions {
        temperature: options.temperature,
        sampling_params,
        max_tokens: Some(clamp_max_tokens_to_context(model, context, options.max_tokens.unwrap_or(model.max_tokens))),
        signal: options.signal.clone(),
        telemetry: options.telemetry.clone(),
        api_key: api_key.filter(|k| !k.is_empty()).map(str::to_string).or_else(|| options.api_key.clone()),
        http_transport: options.http_transport.clone(),
        ws_transport: options.ws_transport.clone(),
        transport: options.transport,
        cache_retention: options.cache_retention,
        session_id: options.session_id.clone(),
        headers: options.headers.clone(),
        identity: options.identity.clone(),
        on_payload: options.on_payload.clone(),
        on_response: options.on_response.clone(),
        on_provider_stream_event: options.on_provider_stream_event.clone(),
        timeout_ms: options.timeout_ms,
        websocket_connect_timeout_ms: options.websocket_connect_timeout_ms,
        max_retries: options.max_retries,
        max_retry_delay_ms: options.max_retry_delay_ms,
        metadata: options.metadata.clone(),
        env: options.env.clone(),
        ..Default::default()
    }
}

/// Tokens always left for the answer when a thinking budget shares the response ceiling.
pub const MIN_ANSWER_TOKENS: u64 = 1024;

pub const DEFAULT_THINKING_BUDGETS: ThinkingBudgets =
    ThinkingBudgets { minimal: Some(1024), low: Some(2048), medium: Some(8192), high: Some(16384) };

/// Map xhigh/max to high for token-budget providers.
pub fn clamp_reasoning(effort: Option<ThinkingLevel>) -> Option<ThinkingLevel> {
    match effort {
        Some(ThinkingLevel::Xhigh | ThinkingLevel::Max) => Some(ThinkingLevel::High),
        other => other,
    }
}

pub fn thinking_budget_for_level(level: ThinkingLevel, custom: Option<&ThinkingBudgets>) -> u64 {
    let pick = |f: fn(&ThinkingBudgets) -> Option<u64>| {
        custom.and_then(f).or_else(|| f(&DEFAULT_THINKING_BUDGETS)).unwrap_or(0)
    };
    match clamp_reasoning(Some(level)).unwrap_or(ThinkingLevel::High) {
        ThinkingLevel::Minimal => pick(|b| b.minimal),
        ThinkingLevel::Low => pick(|b| b.low),
        ThinkingLevel::Medium => pick(|b| b.medium),
        _ => pick(|b| b.high),
    }
}

/// Cap a thinking budget so at least `MIN_ANSWER_TOKENS` remain under a shared ceiling.
pub fn clamp_thinking_budget_to_answer_room(thinking_budget: u64, ceiling: u64) -> u64 {
    thinking_budget.min(ceiling.saturating_sub(MIN_ANSWER_TOKENS))
}

/// Fit a thinking budget inside the response ceiling.
pub fn adjust_max_tokens_for_thinking(
    base_max_tokens: Option<u64>,
    model_max_tokens: u64,
    reasoning_level: ThinkingLevel,
    custom_budgets: Option<&ThinkingBudgets>,
) -> (u64, u64) {
    let mut thinking_budget = thinking_budget_for_level(reasoning_level, custom_budgets);
    let max_tokens = match base_max_tokens {
        None => model_max_tokens,
        Some(base) => (base + thinking_budget).min(model_max_tokens),
    };
    if max_tokens <= thinking_budget {
        thinking_budget = clamp_thinking_budget_to_answer_room(thinking_budget, max_tokens);
    }
    (max_tokens, thinking_budget)
}
