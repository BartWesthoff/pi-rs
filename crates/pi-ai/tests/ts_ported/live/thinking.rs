//! Live ports of the reasoning E2E files: google-thinking-disable.test.ts, the E2E block of
//! anthropic-thinking-disable.test.ts, anthropic-opus-4-8-smoke.test.ts,
//! anthropic-thinking-binding-e2e.test.ts, interleaved-thinking.test.ts, xhigh.test.ts,
//! radius-thinking-replay-e2e.test.ts and xiaomi-token-plan-ams-anthropic-empty-signature-smoke.test.ts.

use pi_ai::compat::{complete_simple, stream, stream_simple};
use regex::Regex;
use serde_json::{Value, json};

use super::*;
use crate::support::ctx;

fn level(name: &str) -> ThinkingLevel {
    serde_json::from_value(json!(name)).unwrap()
}

// ---------------------------------------------------------------------------
// google-thinking-disable.test.ts / anthropic-thinking-disable.test.ts (E2E)
// ---------------------------------------------------------------------------

fn pong_context() -> Context {
    ctx(json!({"systemPrompt": "You are a precise assistant. Follow the requested output format exactly.",
        "messages": [{"role": "user", "content": "Before replying, carefully solve 36863 * 5279 internally. Then reply with the word pong repeated exactly 40 times, separated by single spaces. Do not add any other text.", "timestamp": now()}]}))
}

fn count_pongs(text: &str) -> usize {
    Regex::new(r"(?i)\bpong\b").unwrap().find_iter(text).count()
}

/// `expectThinkingDisabledE2E`: no thinking events or blocks, and the pong reply comes through.
async fn expect_thinking_disabled(model: &Model, options: StreamOptions, min_pongs: usize, max_output: Option<u64>) {
    let (events, response) = stream_events(stream_simple(model, &pong_context(), options)).await;
    assert_eq!(response.stop_reason, StopReason::Stop, "{:?}", response.error_message);
    let (count, chars) = thinking_event_stats(&events);
    assert_eq!((count, chars), (0, 0));
    assert!(!has_thinking(&response));
    let text = text_of(&response);
    assert!(count_pongs(text.trim()) >= min_pongs, "{text}");
    if let Some(max) = max_output {
        assert!(response.usage.output < max, "{}", response.usage.output);
    }
}

fn opts(max_tokens: u64, temperature: Option<f64>) -> StreamOptions {
    StreamOptions { max_tokens: Some(max_tokens), temperature, ..Default::default() }
}


/// The TS Vertex options: `GOOGLE_CLOUD_API_KEY`, else project + location (ADC).
fn vertex_options() -> Option<StreamOptions> {
    if let Some(key) = env("GOOGLE_CLOUD_API_KEY") {
        return Some(StreamOptions { api_key: Some(key), ..Default::default() });
    }
    let project = env("GOOGLE_CLOUD_PROJECT").or_else(|| env("GCLOUD_PROJECT"))?;
    let location = env("GOOGLE_CLOUD_LOCATION")?;
    let mut options = StreamOptions::default();
    options.extra.insert("project".into(), json!(project));
    options.extra.insert("location".into(), json!(location));
    Some(options)
}

#[tokio::test]
#[ignore = "live: needs provider credentials"]
async fn thinking_disable_e2e() {
    const FILE: &str = "google-thinking-disable.test.ts";
    run_case(FILE, "Anthropic: disables thinking for budget-based reasoning models", &[Gate::Env(&["ANTHROPIC_API_KEY"])], 2, |_| {
        Box::pin(async { expect_thinking_disabled(&catalog_model("anthropic", "claude-sonnet-4-5"), opts(320, Some(0.0)), 35, None).await })
    })
    .await;
    run_case(FILE, "Anthropic: disables thinking for adaptive reasoning models", &[Gate::Env(&["ANTHROPIC_API_KEY"])], 2, |_| {
        Box::pin(async { expect_thinking_disabled(&catalog_model("anthropic", "claude-sonnet-4-6"), opts(320, Some(0.0)), 35, None).await })
    })
    .await;
    run_case(FILE, "Google: disables thinking for Gemini 2.5", &[Gate::Env(&["GEMINI_API_KEY"])], 2, |_| {
        Box::pin(async { expect_thinking_disabled(&catalog_model("google", "gemini-2.5-flash"), opts(160, Some(0.0)), 35, None).await })
    })
    .await;
    run_case(FILE, "Google: disables thinking for Gemini 3.x", &[Gate::Env(&["GEMINI_API_KEY"])], 2, |_| {
        Box::pin(async { expect_thinking_disabled(&catalog_model("google", "gemini-3-flash-preview"), opts(160, Some(0.0)), 35, None).await })
    })
    .await;
    run_case(FILE, "Google: does not error when thinking is off for Gemini 3.1 Pro", &[Gate::Env(&["GEMINI_API_KEY"])], 2, |_| {
        Box::pin(async { expect_thinking_disabled(&catalog_model("google", "gemini-3.1-pro-preview"), opts(512, Some(0.0)), 20, None).await })
    })
    .await;
    for (name, id) in [("Vertex: disables thinking for Gemini 2.5", "gemini-2.5-flash"), ("Vertex: disables thinking for Gemini 3.x", "gemini-3-flash-preview")] {
        let Some(vertex) = vertex_options() else {
            println!("[{FILE}] skip {name} (Vertex not configured)");
            continue;
        };
        let model = catalog_model("google-vertex", id);
        let options = StreamOptions { max_tokens: Some(160), temperature: Some(0.0), ..vertex };
        if let Err(error) = attempt(2, || {
            let model = model.clone();
            let options = options.clone();
            Box::pin(async move { expect_thinking_disabled(&model, options, 35, None).await })
        })
        .await
        {
            panic!("[{FILE}] {name}: {error}");
        }
    }
    run_case(FILE, "OpenAI: disables thinking for Responses reasoning models", &[Gate::Env(&["OPENAI_API_KEY"])], 2, |_| {
        Box::pin(async { expect_thinking_disabled(&catalog_model("openai", "gpt-5.4-mini"), opts(160, None), 35, None).await })
    })
    .await;
    run_case(FILE, "OpenRouter: disables thinking for Qwen 3.5 reasoning models", &[Gate::Env(&["OPENROUTER_API_KEY"])], 2, |_| {
        Box::pin(async { expect_thinking_disabled(&catalog_model("openrouter", "qwen/qwen3.5-plus-02-15"), opts(160, Some(0.0)), 35, Some(100)).await })
    })
    .await;
}

#[tokio::test]
#[ignore = "live: needs provider credentials"]
async fn anthropic_thinking_disable_e2e() {
    run_case("anthropic-thinking-disable.test.ts", "disables thinking for Claude reasoning models", &[Gate::Env(&["ANTHROPIC_API_KEY"])], 2, |_| {
        Box::pin(async { expect_thinking_disabled(&catalog_model("anthropic", "claude-sonnet-4-5"), opts(160, Some(0.0)), 35, None).await })
    })
    .await;
}

// ---------------------------------------------------------------------------
// anthropic-opus-4-8-smoke.test.ts
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "live: needs provider credentials"]
async fn anthropic_opus_4_8_smoke() {
    run_case("anthropic-opus-4-8-smoke.test.ts", "streams Claude Opus 4.8 with reasoning enabled", &[Gate::Env(&["ANTHROPIC_API_KEY"])], 2, |_| {
        Box::pin(async {
            let model = catalog_model("anthropic", "claude-opus-4-8");
            let context = ctx(json!({"systemPrompt": "You are a precise assistant. Follow the user's instructions exactly.",
                "messages": [{"role": "user", "content": "Compute 48291 * 7317 and 90844 - 17729, add the results, and determine whether the sum is divisible by 11. Reply with exactly this format and nothing else: sum=<sum>; divisibleBy11=<yes|no>", "timestamp": now()}]}));
            let (hook, payload) = payload_recorder();
            let options = StreamOptions { reasoning: Some(level("high")), max_tokens: Some(1024), on_payload: Some(hook), ..Default::default() };
            let (events, response) = stream_events(stream_simple(&model, &context, options)).await;
            assert_eq!(response.stop_reason, StopReason::Stop, "{:?}", response.error_message);
            let payload = payload.lock().clone().expect("payload");
            assert_eq!(payload["thinking"], json!({"type": "adaptive"}));
            assert_eq!(payload["output_config"], json!({"effort": "high"}));
            assert!(thinking_event_stats(&events).0 > 0);
            let signature = response
                .content
                .iter()
                .find_map(|b| match b {
                    AssistantContent::Thinking(t) => Some(t.thinking_signature.clone().unwrap_or_default()),
                    _ => None,
                })
                .expect("thinking block");
            assert!(!signature.is_empty());
            assert_eq!(text_of(&response).trim(), "sum=353418362; divisibleBy11=yes");
        })
    })
    .await;
}

// ---------------------------------------------------------------------------
// anthropic-thinking-binding-e2e.test.ts
// ---------------------------------------------------------------------------

async fn binding_request(context: Value, effort: &str) -> AssistantMessage {
    let model = catalog_model("anthropic", "claude-fable-5-1");
    let mut options = StreamOptions {
        api_key: env("ANTHROPIC_API_KEY"),
        cache_retention: Some(CacheRetention::None),
        max_tokens: Some(1536),
        on_payload: Some(payload_rewriter(|payload| {
            if payload["thinking"].get("block_binding").is_some() {
                payload["thinking"]["block_binding"]["prefix_mismatch_behavior"] = json!("error");
            }
        })),
        ..Default::default()
    };
    options.extra.insert("thinkingEnabled".into(), json!(true));
    options.extra.insert("thinkingDisplay".into(), json!("summarized"));
    options.extra.insert("effort".into(), json!(effort));
    let transcript = pi_ai::utils::transcript::normalize_context(&ctx(context));
    pi_ai::api::anthropic_messages::stream(&model, &transcript, options).result().await
}

#[tokio::test]
#[ignore = "live: needs provider credentials"]
async fn anthropic_thinking_binding_e2e() {
    run_case(
        "anthropic-thinking-binding-e2e.test.ts",
        "replays managed effort markers required by signed Fable thinking",
        &[Gate::Env(&["ANTHROPIC_API_KEY"])],
        0,
        |_| {
            Box::pin(async {
                let first_user = json!({"role": "user", "content": "Compute 982451653 multiplied by 961748941. Return only the integer.", "timestamp": 1});
                let first = binding_request(json!({"messages": [first_user]}), "low").await;
                assert_eq!(first.stop_reason, StopReason::Stop, "{:?}", first.error_message);
                assert!(first.content.iter().any(|b| matches!(b, AssistantContent::Thinking(t) if t.thinking_signature.as_deref().is_some_and(|s| !s.is_empty()))));
                assert_eq!(first.provider_thinking_level.as_deref(), Some("low"));
                let second_user = json!({"role": "user", "content": "Reply with exactly: ok", "timestamp": 2});
                let first_value = serde_json::to_value(&first).unwrap();
                let exact = binding_request(json!({"messages": [first_user, first_value, second_user]}), "high").await;
                assert_eq!(exact.stop_reason, StopReason::Stop, "{:?}", exact.error_message);
                let mut unmanaged = first_value.clone();
                unmanaged.as_object_mut().unwrap().remove("providerThinkingLevel");
                let missing = binding_request(json!({"messages": [first_user, unmanaged, second_user]}), "high").await;
                assert_eq!(missing.stop_reason, StopReason::Error);
                assert!(missing.error_message.unwrap_or_default().contains("Invalid `signature`"));
            })
        },
    )
    .await;
}

// ---------------------------------------------------------------------------
// interleaved-thinking.test.ts
// ---------------------------------------------------------------------------

fn calculator_tool() -> Value {
    json!({"name": "calculator", "description": "Perform basic arithmetic operations", "parameters": {"type": "object", "properties": {
        "a": {"type": "number", "description": "First number"},
        "b": {"type": "number", "description": "Second number"},
        "operation": {"type": "string", "enum": ["add", "subtract", "multiply", "divide"], "description": "The operation to perform."}
    }, "required": ["a", "b", "operation"]}})
}

fn evaluate_calculator(call: &ToolCall) -> f64 {
    let a = call.arguments["a"].as_f64().expect("a");
    let b = call.arguments["b"].as_f64().expect("b");
    match call.arguments["operation"].as_str().expect("operation") {
        "add" => a + b,
        "subtract" => a - b,
        "multiply" => a * b,
        "divide" => a / b,
        other => panic!("Invalid calculator arguments: {other}"),
    }
}

async fn second_tool_call_with_interleaved_thinking(model: Model) {
    let system = "You are a helpful assistant that must use tools for arithmetic. Always think before every tool call, not just the first one. Do not answer with plain text when a tool call is required.";
    let ask = "Use calculator to calculate 328 * 29. You must call the calculator tool exactly once. Provide the final answer based on the best guess given the tool result, even if it seems unreliable. Start by thinking about the steps you will take to solve the problem.";
    let mut context = ctx(json!({"systemPrompt": system, "messages": [{"role": "user", "content": ask, "timestamp": now()}], "tools": [calculator_tool()]}));
    let options = StreamOptions { reasoning: Some(level("high")), ..Default::default() };
    let first = complete_simple(&model, &context, options.clone()).await;
    assert_eq!(first.stop_reason, StopReason::ToolUse, "{:?}", first.error_message);
    assert!(has_thinking(&first));
    let call = first.tool_calls().next().expect("tool call").clone();
    context.messages.push(Message::Assistant(first));
    let answer = evaluate_calculator(&call);
    context.messages.push(
        serde_json::from_value(json!({"role": "toolResult", "toolCallId": call.id, "toolName": call.name,
            "content": [{"type": "text", "text": format!("The answer is {answer} or {}.", answer * 2.0)}], "isError": false, "timestamp": now()}))
        .unwrap(),
    );
    let second = complete_simple(&model, &context, options).await;
    assert_eq!(second.stop_reason, StopReason::Stop, "{:?}", second.error_message);
    assert!(has_thinking(&second));
    assert!(second.content.iter().any(|b| matches!(b, AssistantContent::Text(_))));
}

#[tokio::test]
#[ignore = "live: needs provider credentials"]
async fn interleaved_thinking_e2e() {
    const FILE: &str = "interleaved-thinking.test.ts";
    for (gate, provider, id, label) in [
        (Gate::Bedrock, "amazon-bedrock", "global.anthropic.claude-opus-4-5-20251101-v1:0", "Bedrock Claude Opus 4.5"),
        (Gate::Bedrock, "amazon-bedrock", "global.anthropic.claude-opus-4-6-v1", "Bedrock Claude Opus 4.6"),
        (Gate::EnvApiKey("anthropic"), "anthropic", "claude-opus-4-5", "Anthropic Claude Opus 4.5"),
        (Gate::EnvApiKey("anthropic"), "anthropic", "claude-opus-4-6", "Anthropic Claude Opus 4.6"),
    ] {
        if open_gate(&gate).await.is_none() {
            println!("[{FILE}] skip {label} ({gate:?} not configured)");
            continue;
        }
        let model = catalog_model(provider, id);
        if let Err(error) = attempt(3, || Box::pin(second_tool_call_with_interleaved_thinking(model.clone()))).await {
            panic!("[{FILE}] {label}: {error}");
        }
        println!("[{FILE}] ok   {label}");
    }
}

// ---------------------------------------------------------------------------
// xhigh.test.ts
// ---------------------------------------------------------------------------

fn sum_context() -> Context {
    let a = now() % 100;
    let b = (now() / 7) % 100;
    ctx(json!({"messages": [{"role": "user", "content": format!("What is {a} + {b}? Think step by step."), "timestamp": now()}]}))
}

fn xhigh_options() -> StreamOptions {
    let mut options = StreamOptions::default();
    options.extra.insert("reasoningEffort".into(), json!("xhigh"));
    options
}

#[tokio::test]
#[ignore = "live: needs provider credentials"]
async fn xhigh_reasoning_e2e() {
    const FILE: &str = "xhigh.test.ts";
    let gate = [Gate::Env(&["OPENAI_API_KEY"])];
    run_case(FILE, "gpt 5.5: should work with openai-responses", &gate, 0, |_| {
        Box::pin(async {
            let (events, response) = stream_events(stream(&catalog_model("openai", "gpt-5.5"), &sum_context(), xhigh_options())).await;
            assert_eq!(response.stop_reason, StopReason::Stop, "Error: {:?}", response.error_message);
            assert!(response.content.iter().any(|b| matches!(b, AssistantContent::Text(_))));
            let saw_thinking = events.iter().any(|e| matches!(e, AssistantMessageEvent::ThinkingStart { .. } | AssistantMessageEvent::ThinkingDelta { .. }));
            assert!(saw_thinking || has_thinking(&response));
        })
    })
    .await;
    run_case(FILE, "gpt-5-mini: should error with openai-responses when using xhigh", &gate, 0, |_| {
        Box::pin(async {
            let response = stream(&catalog_model("openai", "gpt-5-mini"), &sum_context(), xhigh_options()).result().await;
            assert_eq!(response.stop_reason, StopReason::Error);
            assert!(response.error_message.unwrap_or_default().contains("xhigh"));
        })
    })
    .await;
    run_case(FILE, "gpt-5-mini: should error with openai-completions when using xhigh", &gate, 0, |_| {
        Box::pin(async {
            let mut model = catalog_model("openai", "gpt-5-mini");
            model.api = "openai-completions".into();
            model.compat = None;
            let response = stream(&model, &sum_context(), xhigh_options()).result().await;
            assert_eq!(response.stop_reason, StopReason::Error);
            assert!(response.error_message.unwrap_or_default().contains("xhigh"));
        })
    })
    .await;
}

// ---------------------------------------------------------------------------
// radius-thinking-replay-e2e.test.ts
// ---------------------------------------------------------------------------

const RADIUS_KEY: Gate = Gate::FirstOf(&[Gate::Env(&["RADIUS_API_KEY"]), Gate::OAuth("radius")]);
const ANTHROPIC_KEY: Gate = Gate::FirstOf(&[Gate::EnvApiKey("anthropic"), Gate::OAuth("anthropic")]);

fn radius_user() -> Value {
    json!({"role": "user", "content": "First, think carefully: list every prime between 100 and 300 in your head and sum them. Then call the lookup tool with key equal to that sum. Do not write the primes in your visible reply.", "timestamp": 1})
}

async fn radius_request(model: &Model, key: &str, messages: Vec<Value>) -> AssistantMessage {
    let context = ctx(json!({"systemPrompt": "You are a careful assistant.", "messages": messages,
        "tools": [{"name": "lookup", "description": "Look up a value by key.", "parameters": {"type": "object", "properties": {"key": {"type": "string"}}, "required": ["key"]}}]}));
    let options = StreamOptions {
        api_key: Some(key.to_string()),
        reasoning: Some(level("high")),
        max_tokens: Some(16000),
        cache_retention: Some(CacheRetention::None),
        ..Default::default()
    };
    complete_simple(model, &context, options).await
}

fn input_tokens(message: &AssistantMessage) -> u64 {
    message.usage.input + message.usage.cache_read + message.usage.cache_write
}

async fn thinking_turn(model: &Model, key: &str) -> (AssistantMessage, Value) {
    let reply = radius_request(model, key, vec![radius_user()]).await;
    assert_eq!(reply.stop_reason, StopReason::ToolUse, "{:?}", reply.error_message);
    assert!(reply.content.iter().any(|b| matches!(b, AssistantContent::Thinking(t) if t.thinking_signature.as_deref().is_some_and(|s| !s.is_empty()))));
    let call = reply.tool_calls().next().expect("reply has no tool call").clone();
    let result = json!({"role": "toolResult", "toolCallId": call.id, "toolName": call.name, "content": [{"type": "text", "text": "value: 42"}], "isError": false, "timestamp": 2});
    (reply, result)
}

async fn replayed_thinking_tokens(model: &Model, key: &str, reply: &Value, result: &Value) -> i64 {
    let mut without = reply.clone();
    without["content"] = json!(reply["content"].as_array().unwrap().iter().filter(|b| b["type"] != "thinking").cloned().collect::<Vec<_>>());
    let (kept, stripped) = tokio::join!(
        radius_request(model, key, vec![radius_user(), reply.clone(), result.clone()]),
        radius_request(model, key, vec![radius_user(), without, result.clone()]),
    );
    assert_ne!(kept.stop_reason, StopReason::Error, "{:?}", kept.error_message);
    assert_ne!(stripped.stop_reason, StopReason::Error, "{:?}", stripped.error_message);
    input_tokens(&kept) as i64 - input_tokens(&stripped) as i64
}

#[tokio::test]
#[ignore = "live: needs provider credentials"]
async fn radius_thinking_replay_e2e() {
    const FILE: &str = "radius-thinking-replay-e2e.test.ts";
    run_case(FILE, "anthropic keeps it (control)", &[ANTHROPIC_KEY], 0, |keys| {
        Box::pin(async move {
            let key = keys[0].clone().unwrap();
            let model = catalog_model("anthropic", "claude-opus-5-5");
            let (reply, result) = thinking_turn(&model, &key).await;
            assert_eq!(reply.provider_thinking_level.as_deref(), Some("high"));
            let added = replayed_thinking_tokens(&model, &key, &serde_json::to_value(&reply).unwrap(), &result).await;
            assert!(added as f64 > 0.5 * reply.usage.output as f64, "{added}");
        })
    })
    .await;
    run_case(FILE, "radius keeps it", &[RADIUS_KEY], 0, |keys| {
        Box::pin(async move {
            let key = keys[0].clone().unwrap();
            let model = catalog_model("radius", "claude-opus-5-5");
            let (reply, result) = thinking_turn(&model, &key).await;
            // `expect.soft`: reported without failing the case.
            if reply.provider_thinking_level.as_deref() != Some("high") {
                println!("radius done event omits providerThinkingLevel");
            }
            let added = replayed_thinking_tokens(&model, &key, &serde_json::to_value(&reply).unwrap(), &result).await;
            assert!(added as f64 > 0.5 * reply.usage.output as f64, "radius dropped the replayed thinking: {added}");
        })
    })
    .await;
    run_case(FILE, "radius keeps it when the client supplies the level", &[RADIUS_KEY], 0, |keys| {
        Box::pin(async move {
            let key = keys[0].clone().unwrap();
            let model = catalog_model("radius", "claude-opus-5-5");
            let (reply, result) = thinking_turn(&model, &key).await;
            let mut supplied = serde_json::to_value(&reply).unwrap();
            supplied["providerThinkingLevel"] = json!("high");
            let added = replayed_thinking_tokens(&model, &key, &supplied, &result).await;
            assert!(added as f64 > 0.5 * reply.usage.output as f64, "{added}");
        })
    })
    .await;
}

// ---------------------------------------------------------------------------
// xiaomi-token-plan-ams-anthropic-empty-signature-smoke.test.ts
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "live: needs provider credentials"]
async fn xiaomi_token_plan_ams_empty_signature_smoke() {
    run_case(
        "xiaomi-token-plan-ams-anthropic-empty-signature-smoke.test.ts",
        "reproduces empty thinking signatures and preserves them for replay",
        &[Gate::EnvApiKey("xiaomi-token-plan-ams")],
        1,
        |keys| {
            Box::pin(async move {
                let model = crate::support::model(json!({"id": "mimo-v2.6-pro", "name": "MiMo-V2.5-Pro Anthropic smoke", "api": "anthropic-messages",
                    "provider": "xiaomi-token-plan-ams", "baseUrl": "https://token-plan-ams.xiaomimimo.com/anthropic", "reasoning": true, "input": ["text"],
                    "cost": {"input": 1, "output": 3, "cacheRead": 0.2, "cacheWrite": 0}, "contextWindow": 1048576, "maxTokens": 1024,
                    "compat": {"allowEmptySignature": true}}));
                let options = StreamOptions { api_key: keys[0].clone(), max_tokens: Some(512), reasoning: Some(level("high")), ..Default::default() };
                let first_user = json!({"role": "user", "content": "Think internally if you need to, then reply with exactly this text and nothing else: first-ok", "timestamp": now()});
                let system = "You are concise. Follow the requested output format exactly.";
                let first = complete_simple(&model, &ctx(json!({"systemPrompt": system, "messages": [first_user]})), options.clone()).await;
                assert_eq!(first.stop_reason, StopReason::Stop, "{:?}", first.error_message);
                let thinking: Vec<&ThinkingContent> = first
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        AssistantContent::Thinking(t) => Some(t),
                        _ => None,
                    })
                    .collect();
                assert!(!thinking.is_empty());
                assert!(thinking.iter().any(|t| t.thinking_signature.as_deref() == Some("")));
                let replay = ctx(json!({"systemPrompt": system, "messages": [first_user, serde_json::to_value(&first).unwrap(),
                    {"role": "user", "content": "Reply with exactly this text and nothing else: second-ok", "timestamp": now()}]}));
                let payload = crate::support::capture_payload_offline(|o| stream_simple(&model, &replay, o), options).await;
                let assistant = payload["messages"].as_array().unwrap().iter().find(|m| m["role"] == "assistant").expect("assistant").clone();
                let blocks = assistant["content"].as_array().expect("content blocks");
                let replayed: Vec<&Value> = blocks.iter().filter(|b| b["type"] == "thinking").collect();
                assert_eq!(replayed, [&json!({"type": "thinking", "thinking": thinking[0].thinking, "signature": ""})]);
                assert!(!blocks.iter().any(|b| b["type"] == "text" && b["text"] == json!(thinking[0].thinking)));
            })
        },
    )
    .await;
}

