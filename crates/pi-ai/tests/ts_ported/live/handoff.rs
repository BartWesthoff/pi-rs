//! Live ports of cross-provider-handoff.test.ts, tool-call-id-normalization.test.ts and
//! openai-responses-reasoning-replay-e2e.test.ts.

use pi_ai::compat::{complete, complete_simple};
use pi_ai::env_api_keys::get_env_api_key;
use pi_ai::providers::all::get_builtin_model;
use serde_json::{Value, json};

use super::*;
use crate::support::ctx;

fn level(name: &str) -> ThinkingLevel {
    serde_json::from_value(json!(name)).unwrap()
}

fn value(message: &AssistantMessage) -> Value {
    serde_json::to_value(message).unwrap()
}

fn messages_context(system: &str, messages: Vec<Value>, tools: Value) -> Context {
    ctx(json!({"systemPrompt": system, "messages": messages, "tools": tools}))
}

// ---------------------------------------------------------------------------
// cross-provider-handoff.test.ts
// ---------------------------------------------------------------------------

struct Pair {
    provider: &'static str,
    model: &'static str,
    label: &'static str,
    api_override: Option<&'static str>,
    upstream_api_key_env: Option<&'static str>,
}

const fn pair(provider: &'static str, model: &'static str, label: &'static str) -> Pair {
    Pair { provider, model, label, api_override: None, upstream_api_key_env: None }
}

/// Some pairs name models the catalog has since dropped; they are skipped ("Model not found"), as in TS.
const PAIRS: &[Pair] = &[
    pair("anthropic", "claude-sonnet-4-5", "anthropic-claude-sonnet-4-5"),
    pair("google", "gemini-3-flash-preview", "google-gemini-3-flash-preview"),
    Pair { api_override: Some("openai-completions"), ..pair("openai", "gpt-4o-mini", "openai-completions-gpt-4o-mini") },
    pair("openai", "gpt-5-mini", "openai-responses-gpt-5-mini"),
    pair("azure", "gpt-4o-mini", "azure-gpt-4o-mini"),
    pair("openai-codex", "gpt-6.1-sol", "openai-codex-gpt-6.1-sol"),
    pair("github-copilot", "claude-sonnet-4.5", "copilot-claude-sonnet-4.5"),
    pair("github-copilot", "gpt-5.1-codex", "copilot-gpt-5.1-codex"),
    pair("github-copilot", "gemini-3-flash-preview", "copilot-gemini-3-flash-preview"),
    pair("github-copilot", "grok-code-fast-1", "copilot-grok-code-fast-1"),
    pair("amazon-bedrock", "global.anthropic.claude-sonnet-4-5-20250929-v1:0", "bedrock-claude-sonnet-4-5"),
    pair("xai", "grok-4.3", "xai-grok-4.3"),
    pair("cerebras", "zai-glm-4.7", "cerebras-zai-glm-4.7"),
    pair("cloudflare-workers-ai", "@cf/moonshotai/kimi-k2.6", "cloudflare-kimi-k2.6"),
    pair("cloudflare-ai-gateway", "workers-ai/@cf/moonshotai/kimi-k2.6", "cloudflare-gateway-kimi-k2.6"),
    Pair { upstream_api_key_env: Some("ANTHROPIC_API_KEY"), ..pair("cloudflare-ai-gateway", "claude-sonnet-4-5", "cloudflare-gateway-claude-sonnet-4-5") },
    Pair { upstream_api_key_env: Some("OPENAI_API_KEY"), ..pair("cloudflare-ai-gateway", "gpt-5.1", "cloudflare-gateway-gpt-5.1") },
    pair("groq", "openai/gpt-oss-120b", "groq-gpt-oss-120b"),
    pair("huggingface", "moonshotai/Kimi-K2.5", "huggingface-kimi-k2.5"),
    pair("together", "moonshotai/Kimi-K3", "together-kimi-k3"),
    pair("baseten", "zai-org/GLM-5.2", "baseten-glm-5.2"),
    pair("kimi-coding", "kimi-for-coding", "kimi-for-coding"),
    pair("meta", "muse-spark-1.3", "meta-muse-spark-1.3"),
    pair("mistral", "devstral-medium-latest", "mistral-devstral-medium"),
    pair("minimax", "MiniMax-M2.7", "minimax-m2.7"),
    pair("minimax-cn", "MiniMax-M2.7", "minimax-m2.7"),
    pair("opencode", "big-pickle", "zen-big-pickle"),
    pair("opencode", "claude-sonnet-4-5", "zen-claude-sonnet-4-5"),
    pair("opencode", "gemini-3-flash", "zen-gemini-3-flash"),
    pair("opencode", "glm-4.7-free", "zen-glm-4.7-free"),
    pair("opencode", "gpt-5.2-codex", "zen-gpt-5.2-codex"),
    pair("opencode", "minimax-m2.1-free", "zen-minimax-m2.1-free"),
    pair("opencode-go", "kimi-k2.5", "go-kimi-k2.5"),
    pair("opencode-go", "minimax-m2.5", "go-minimax-m2.5"),
    pair("xiaomi", "mimo-v2.6-pro", "xiaomi-mimo-v2.6-pro"),
    pair("xiaomi-token-plan-cn", "mimo-v2.6-pro", "xiaomi-token-plan-cn-mimo-v2.6-pro"),
    pair("xiaomi-token-plan-ams", "mimo-v2.6-pro", "xiaomi-token-plan-ams-mimo-v2.6-pro"),
    pair("xiaomi-token-plan-sgp", "mimo-v2.6-pro", "xiaomi-token-plan-sgp-mimo-v2.6-pro"),
    pair("qwen-token-plan", "qwen3.7-max", "qwen-token-plan-qwen3.7-max"),
    pair("qwen-token-plan-cn", "qwen3.7-max", "qwen-token-plan-cn-qwen3.7-max"),
    pair("qwen-token-plan-individual", "qwen3.8-max", "qwen-token-plan-individual-qwen3.8-max"),
    pair("qwen-token-plan-individual", "deepseek-v4-flash-0731", "qwen-token-plan-individual-deepseek-v4-flash-0731"),
    pair("qwen-token-plan-individual", "glm-5.2", "qwen-token-plan-individual-glm-5.2"),
];

fn double_tool() -> Value {
    json!([{"name": "double_number", "description": "Doubles a number and returns the result",
        "parameters": {"type": "object", "properties": {"value": {"type": "number", "description": "A number to double"}}, "required": ["value"]}}])
}

/// `getApiKey`: OAuth storage first, then the environment.
async fn handoff_api_key(provider: &str) -> Option<String> {
    match resolve_api_key(provider).await {
        Some(key) => Some(key),
        None => get_env_api_key(provider, None),
    }
}

/// `hasApiKey` (synchronous, environment only).
fn has_api_key(pair: &Pair) -> bool {
    match pair.provider {
        "azure" => env("AZURE_OPENAI_API_KEY").is_some() && (env("AZURE_OPENAI_BASE_URL").is_some() || env("AZURE_OPENAI_RESOURCE_NAME").is_some()),
        "cloudflare-workers-ai" => env("CLOUDFLARE_API_KEY").is_some() && env("CLOUDFLARE_ACCOUNT_ID").is_some(),
        "cloudflare-ai-gateway" => {
            env("CLOUDFLARE_API_KEY").is_some()
                && env("CLOUDFLARE_ACCOUNT_ID").is_some()
                && env("CLOUDFLARE_GATEWAY_ID").is_some()
                && pair.upstream_api_key_env.is_none_or(|name| env(name).is_some())
        }
        provider => get_env_api_key(provider, None).is_some(),
    }
}

fn headers(pair: &Pair) -> Option<ProviderHeaders> {
    let key = env(pair.upstream_api_key_env?)?;
    let mut headers = ProviderHeaders::new();
    headers.insert("Authorization".into(), Some(format!("Bearer {key}")));
    Some(headers)
}

fn pair_model(pair: &Pair) -> Option<Model> {
    let mut model = get_builtin_model(pair.provider, pair.model)?;
    if let Some(api) = pair.api_override {
        model.api = api.into();
    }
    Some(model)
}

fn handoff_options(model: &Model, pair: &Pair, key: &str, payload: OnPayload) -> StreamOptions {
    StreamOptions {
        api_key: Some(key.to_string()),
        reasoning: model.reasoning.then(|| level("high")),
        headers: headers(pair),
        on_payload: Some(payload),
        ..Default::default()
    }
}

/// `dumpFailurePayload`.
fn dump_failure(label: &str, error: &str, payload: Option<Value>, messages: &[Value]) {
    let path = std::env::temp_dir().join(format!("pi-handoff-{label}-{}.json", now()));
    let body = json!({"label": label, "error": error, "payload": payload, "messages": messages});
    let _ = std::fs::write(&path, serde_json::to_string_pretty(&body).unwrap_or_default());
    println!("Wrote failure payload to {}", path.display());
}

/// `generateContext`: user, assistant tool call, tool result, final reply.
async fn generate_context(pair: &Pair, key: &str) -> Option<Vec<Value>> {
    let Some(model) = pair_model(pair) else {
        println!("  Model not found: {}/{}", pair.provider, pair.model);
        return None;
    };
    let user = json!({"role": "user", "content": "Please double the number 21 using the double_number tool.", "timestamp": now()});
    let (hook, payload) = payload_recorder();
    let context = messages_context("You are a helpful assistant. Use the provided tool to complete the task.", vec![user.clone()], double_tool());
    let first = complete_simple(&model, &context, handoff_options(&model, pair, key, hook.clone())).await;
    if first.stop_reason == StopReason::Error {
        let error = first.error_message.clone().unwrap_or_else(|| "Unknown error".into());
        println!("  Initial request error: {error}");
        dump_failure(&format!("{}-initial", pair.label), &error, payload.lock().clone(), &[user]);
        return None;
    }
    let Some(call) = first.tool_calls().next().cloned() else {
        println!("  No tool call in response (stopReason: {:?})", first.stop_reason);
        return Some(vec![user, value(&first)]);
    };
    println!("  Tool call ID: {}", call.id);
    let result = json!({"role": "toolResult", "toolCallId": call.id, "toolName": call.name, "content": [{"type": "text", "text": "42"}], "isError": false, "timestamp": now()});
    let messages = vec![user, value(&first), result];
    let context = messages_context("You are a helpful assistant.", messages.clone(), double_tool());
    let last = complete_simple(&model, &context, handoff_options(&model, pair, key, hook)).await;
    if last.stop_reason == StopReason::Error {
        let error = last.error_message.clone().unwrap_or_else(|| "Unknown error".into());
        println!("  Final request error: {error}");
        dump_failure(&format!("{}-final", pair.label), &error, payload.lock().clone(), &messages);
        return None;
    }
    let mut messages = messages;
    messages.push(value(&last));
    Some(messages)
}

#[tokio::test]
#[ignore = "live: needs provider credentials"]
async fn cross_provider_handoff_e2e() {
    const FILE: &str = "cross-provider-handoff.test.ts";
    if !PAIRS.iter().any(has_api_key) {
        println!("[{FILE}] skip (no provider credentials)");
        return;
    }
    println!("\n=== Generating Fixtures ===\n");
    // Keyed by label like the TS record (minimax and minimax-cn share one), with every pair that
    // produced a fixture kept as a target.
    let mut contexts: indexmap::IndexMap<&str, Vec<Value>> = indexmap::IndexMap::new();
    let mut available: Vec<&Pair> = Vec::new();
    for pair in PAIRS {
        let Some(key) = handoff_api_key(pair.provider).await.filter(|_| has_api_key(pair)) else {
            println!("[{}] Skipping - no auth for {}", pair.label, pair.provider);
            continue;
        };
        println!("[{}] Generating fixture...", pair.label);
        match generate_context(pair, &key).await {
            Some(messages) if messages.len() >= 4 => {
                println!("[{}] Generated {} messages", pair.label, messages.len());
                contexts.insert(pair.label, messages);
                available.push(pair);
            }
            _ => println!("[{}] Failed to generate fixture, skipping", pair.label),
        }
    }
    println!("\n=== {}/{} contexts available ===\n", contexts.len(), PAIRS.len());
    // "should have at least 2 fixtures to test handoffs"
    assert!(contexts.len() >= 2, "need at least 2 fixtures, got {}", contexts.len());

    let mut failures = Vec::new();
    let mut total = 0;
    for target in &available {
        let Some(key) = handoff_api_key(target.provider).await.filter(|_| has_api_key(target)) else { continue };
        let mut messages: Vec<Value> = contexts.iter().filter(|(label, _)| **label != target.label).flat_map(|(_, m)| m.clone()).collect();
        if messages.is_empty() {
            continue;
        }
        let other = messages.len();
        messages.push(json!({"role": "user", "content": "Great, thanks for all that help! Now just say 'Hello, handoff successful!' to confirm you received everything.", "timestamp": now()}));
        let Some(model) = pair_model(target) else { continue };
        println!("[Target: {}] Testing with {other} messages from other providers...", target.label);
        let (hook, payload) = payload_recorder();
        let context = messages_context("You are a helpful assistant.", messages.clone(), double_tool());
        let response = complete_simple(&model, &context, handoff_options(&model, target, &key, hook)).await;
        total += 1;
        if response.stop_reason == StopReason::Error {
            let error = response.error_message.clone().unwrap_or_else(|| "Unknown error".into());
            println!("[Target: {}] FAILED: {error}", target.label);
            dump_failure(target.label, &error, payload.lock().clone(), &messages);
            failures.push(format!("{}: {error}", target.label));
        } else {
            let preview: String = text_of(&response).chars().take(100).collect::<String>().replace('\n', " ");
            println!("[Target: {}] SUCCESS: {preview}...", target.label);
        }
    }
    println!("\n=== Results Summary ===\n\nPassed: {}/{total}", total - failures.len());
    assert!(failures.is_empty(), "Failures:\n{}", failures.join("\n"));
}

// ---------------------------------------------------------------------------
// tool-call-id-normalization.test.ts
// ---------------------------------------------------------------------------

fn echo_tool() -> Value {
    json!([{"name": "echo", "description": "Echoes the message back", "parameters": {"type": "object",
        "properties": {"message": {"type": "string", "description": "Message to echo back"}}, "required": ["message"]}}])
}

async fn live_handoff(target_provider: &str, target_model: &str, copilot: Option<String>, target_key: Option<String>, text: &str) {
    let copilot_model = catalog_model("github-copilot", "gpt-6-luna");
    let user = json!({"role": "user", "content": format!("Use the echo tool to echo '{text}'"), "timestamp": now()});
    let first = complete_simple(
        &copilot_model,
        &messages_context("You are a helpful assistant. Use the echo tool when asked.", vec![user.clone()], echo_tool()),
        StreamOptions { api_key: copilot, ..Default::default() },
    )
    .await;
    assert_eq!(first.stop_reason, StopReason::ToolUse, "Copilot error: {:?}", first.error_message);
    let call = first.tool_calls().next().expect("tool call").clone();
    println!("Tool call ID from github-copilot: {}...", call.id.chars().take(80).collect::<String>());
    let result = json!({"role": "toolResult", "toolCallId": call.id, "toolName": "echo", "content": [{"type": "text", "text": text}], "isError": false, "timestamp": now()});
    let messages = vec![user, value(&first), result, json!({"role": "user", "content": "Say hi", "timestamp": now()})];
    let response = complete_simple(
        &catalog_model(target_provider, target_model),
        &messages_context("You are a helpful assistant.", messages, echo_tool()),
        StreamOptions { api_key: target_key, ..Default::default() },
    )
    .await;
    assert_ne!(response.stop_reason, StopReason::Error, "{target_provider} error: {:?}", response.error_message);
    assert_eq!(response.error_message, None);
}

const FAILING_TOOL_CALL_ID: &str = "call_pAYbIr76hXIjncD9UE4eGfnS|t5nnb2qYMFWGSsr13fhCd1CaCu3t3qONEPuOudu4HSVEtA8YJSL6FAZUxvoOoD792VIJWl91g87EdqsCWp9krVsdBysQoDaf9lMCLb8BS4EYi4gQd5kBQBYLlgD71PYwvf+TbMD9J9/5OMD42oxSRj8H+vRf78/l2Xla33LWz4nOgsddBlbvabICRs8GHt5C9PK5keFtzyi3lsyVKNlfduK3iphsZqs4MLv4zyGJnvZo/+QzShyk5xnMSQX/f98+aEoNflEApCdEOXipipgeiNWnpFSHbcwmMkZoJhURNu+JEz3xCh1mrXeYoN5o+trLL3IXJacSsLYXDrYTipZZbJFRPAucgbnjYBC+/ZzJOfkwCs+Gkw7EoZR7ZQgJ8ma+9586n4tT4cI8DEhBSZsWMjrCt8dxKg==";

/// The exact pipe-separated id from issue #1022.
fn prefilled_messages() -> Vec<Value> {
    let t = now();
    vec![
        json!({"role": "user", "content": "Use the echo tool to echo 'hello'", "timestamp": t - 2000}),
        json!({"role": "assistant", "content": [{"type": "toolCall", "id": FAILING_TOOL_CALL_ID, "name": "echo", "arguments": {"message": "hello"}}],
            "api": "openai-responses", "provider": "github-copilot", "model": "gpt-5.2-codex",
            "usage": {"input": 100, "output": 50, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 150, "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}},
            "stopReason": "toolUse", "timestamp": t - 1500}),
        json!({"role": "toolResult", "toolCallId": FAILING_TOOL_CALL_ID, "toolName": "echo", "content": [{"type": "text", "text": "hello"}], "isError": false, "timestamp": t - 1000}),
        json!({"role": "user", "content": "Say hi", "timestamp": t}),
    ]
}

async fn prefilled(provider: &str, id: &str, key: Option<String>, forbidden: &[&str]) {
    let response = complete_simple(
        &catalog_model(provider, id),
        &messages_context("You are a helpful assistant.", prefilled_messages(), echo_tool()),
        StreamOptions { api_key: key, ..Default::default() },
    )
    .await;
    assert_ne!(response.stop_reason, StopReason::Error, "{provider} error: {:?}", response.error_message);
    if let Some(error) = response.error_message {
        for word in forbidden {
            assert!(!error.contains(word), "{error}");
        }
    }
}

#[tokio::test]
#[ignore = "live: needs provider credentials"]
async fn tool_call_id_normalization_e2e() {
    const FILE: &str = "tool-call-id-normalization.test.ts";
    run_case(FILE, "github-copilot -> openrouter should normalize pipe-separated IDs", &[Gate::OAuth("github-copilot"), Gate::EnvApiKey("openrouter")], 0, |keys| {
        Box::pin(async move { live_handoff("openrouter", "openai/gpt-5.5", keys[0].clone(), keys[1].clone(), "hello world").await })
    })
    .await;
    run_case(FILE, "github-copilot -> openai-codex should normalize pipe-separated IDs", &[Gate::OAuth("github-copilot"), Gate::OAuth("openai-codex")], 0, |keys| {
        Box::pin(async move { live_handoff("openai-codex", "gpt-6.1-sol", keys[0].clone(), keys[1].clone(), "test message").await })
    })
    .await;
    run_case(FILE, "openrouter should handle prefilled context with long pipe-separated IDs", &[Gate::EnvApiKey("openrouter")], 0, |keys| {
        Box::pin(async move { prefilled("openrouter", "openai/gpt-5.5", keys[0].clone(), &["call_id", "too long"]).await })
    })
    .await;
    run_case(FILE, "openai-codex should handle prefilled context with long pipe-separated IDs", &[Gate::OAuth("openai-codex")], 0, |keys| {
        Box::pin(async move { prefilled("openai-codex", "gpt-6.1-sol", keys[0].clone(), &["id", "additional characters"]).await })
    })
    .await;
}

// ---------------------------------------------------------------------------
// openai-responses-reasoning-replay-e2e.test.ts
// ---------------------------------------------------------------------------

fn effort(key: Option<String>, value: &str) -> StreamOptions {
    let mut options = StreamOptions { api_key: key, ..Default::default() };
    options.extra.insert("reasoningEffort".into(), json!(value));
    options
}

fn double_ask() -> Value {
    json!({"role": "user", "content": "Use the double_number tool to double 21.", "timestamp": now()})
}

async fn answer_42(model: &Model, history: Vec<Value>, options: StreamOptions) {
    let (hook, payload) = payload_recorder();
    let context = messages_context("You are a helpful assistant. Answer concisely.", history, double_tool());
    let response = complete(model, &context, StreamOptions { on_payload: Some(hook), ..options }).await;
    if let Some(payload) = payload.lock().clone() {
        let input = payload["input"].as_array().cloned().unwrap_or_default();
        println!("Payload sent to API:\n- function_calls: {}\n- reasoning items: {}",
            input.iter().filter(|i| i["type"] == "function_call").count(), input.iter().filter(|i| i["type"] == "reasoning").count());
    }
    assert_ne!(response.stop_reason, StopReason::Error, "Error: {:?}", response.error_message);
    assert!(response.error_message.as_deref().unwrap_or_default().is_empty());
    assert!(!response.content.is_empty());
    assert!(text_of(&response).contains("42"), "{}", text_of(&response));
}

fn tool_round(first: &AssistantMessage, user: &Value) -> Vec<Value> {
    let call = first.tool_calls().next().expect("Missing tool call - model did not use the tool").clone();
    vec![
        user.clone(),
        value(first),
        json!({"role": "toolResult", "toolCallId": call.id, "toolName": call.name, "content": [{"type": "text", "text": "42"}], "isError": false, "timestamp": now()}),
        json!({"role": "user", "content": "What was the result? Answer with just the number.", "timestamp": now()}),
    ]
}

#[tokio::test]
#[ignore = "live: needs provider credentials"]
async fn openai_responses_reasoning_replay_e2e() {
    const FILE: &str = "openai-responses-reasoning-replay-e2e.test.ts";
    let gates = [Gate::EnvApiKey("openai"), Gate::Env(&["OPENAI_API_KEY", "ANTHROPIC_API_KEY"])];
    run_case(FILE, "skips reasoning-only history after an aborted turn", &gates, 2, |keys| {
        Box::pin(async move {
            let model = catalog_model("openai", "gpt-5-mini");
            let user = double_ask();
            let first = complete(&model, &messages_context("You are a helpful assistant. Use the tool.", vec![user.clone()], double_tool()), effort(keys[0].clone(), "high")).await;
            let thinking = first
                .content
                .iter()
                .find(|b| matches!(b, AssistantContent::Thinking(t) if t.thinking_signature.is_some()))
                .cloned()
                .expect("Missing thinking signature from OpenAI Responses");
            let mut corrupted = first.clone();
            corrupted.content = vec![thinking].into();
            corrupted.stop_reason = StopReason::Aborted;
            let history = vec![user, value(&corrupted), json!({"role": "user", "content": "Say hello to confirm you can continue.", "timestamp": now()})];
            let response = complete(&model, &messages_context("You are a helpful assistant.", history, double_tool()), effort(keys[0].clone(), "high")).await;
            assert_ne!(response.stop_reason, StopReason::Error, "Error: {:?}", response.error_message);
            assert!(response.error_message.as_deref().unwrap_or_default().is_empty());
            assert!(!response.content.is_empty());
        })
    })
    .await;
    run_case(FILE, "handles same-provider different-model handoff with tool calls", &gates, 2, |keys| {
        Box::pin(async move {
            let user = double_ask();
            let first = complete(
                &catalog_model("openai", "gpt-5-mini"),
                &messages_context("You are a helpful assistant. Always use the tool when asked.", vec![user.clone()], double_tool()),
                effort(keys[0].clone(), "high"),
            )
            .await;
            answer_42(&catalog_model("openai", "gpt-5.5"), tool_round(&first, &user), effort(keys[0].clone(), "high")).await;
        })
    })
    .await;
    run_case(FILE, "handles cross-provider handoff from Anthropic to OpenAI Codex", &gates, 2, |keys| {
        Box::pin(async move {
            let user = double_ask();
            let mut options = StreamOptions { api_key: get_env_api_key("anthropic", None), ..Default::default() };
            options.extra.insert("thinkingEnabled".into(), json!(true));
            options.extra.insert("thinkingBudgetTokens".into(), json!(5000));
            let first = complete(
                &catalog_model("anthropic", "claude-sonnet-4-5"),
                &messages_context("You are a helpful assistant. Always use the tool when asked.", vec![user.clone()], double_tool()),
                options,
            )
            .await;
            answer_42(&catalog_model("openai", "gpt-5.5"), tool_round(&first, &user), effort(keys[0].clone(), "high")).await;
        })
    })
    .await;
}
