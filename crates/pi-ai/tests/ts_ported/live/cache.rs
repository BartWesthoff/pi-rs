//! Live ports of the cache/compat probe E2E files: openai-codex-cache-affinity-e2e.test.ts,
//! openai-responses-cache-affinity-e2e.test.ts, openrouter-cache-write-repro.test.ts,
//! anthropic-long-cache-retention-e2e.test.ts, anthropic-eager-tool-input-e2e.test.ts and zen.test.ts.

use pi_ai::compat::{complete, complete_simple};
use pi_ai::providers::all::{get_builtin_models, get_builtin_providers};
use serde_json::{Value, json};

use super::*;
use crate::support::ctx;

const SESSION_ID: &str = "0195d6e4-4cf9-7f44-a2d8-f8f7f49ee9d3";

fn reply_context(text: &str) -> Context {
    ctx(json!({"systemPrompt": "You are a helpful assistant. Reply exactly as requested.",
        "messages": [{"role": "user", "content": format!("Reply with exactly: {text}"), "timestamp": now()}]}))
}

#[tokio::test]
#[ignore = "live: needs provider credentials"]
async fn openai_codex_cache_affinity_e2e() {
    run_case(
        "openai-codex-cache-affinity-e2e.test.ts",
        "handles SSE requests with aligned cache-affinity identifiers",
        &[Gate::OAuth("openai-codex")],
        0,
        |keys| {
            Box::pin(async move {
                let options = StreamOptions { api_key: keys[0].clone(), session_id: Some(SESSION_ID.into()), transport: Some(Transport::Sse), ..Default::default() };
                let response = complete(&catalog_model("openai-codex", "gpt-6.1-sol"), &reply_context("cache affinity e2e success"), options).await;
                assert_ne!(response.stop_reason, StopReason::Error, "{:?}", response.error_message);
                assert_eq!(response.error_message, None);
                assert!(text_of(&response).contains("cache affinity e2e success"));
            })
        },
    )
    .await;
}

#[tokio::test]
#[ignore = "live: needs provider credentials"]
async fn openai_responses_cache_affinity_e2e() {
    run_case(
        "openai-responses-cache-affinity-e2e.test.ts",
        "handles direct OpenAI Responses requests with aligned cache-affinity identifiers",
        &[Gate::Env(&["OPENAI_API_KEY"])],
        2,
        |_| {
            Box::pin(async move {
                let options = StreamOptions { api_key: env("OPENAI_API_KEY"), session_id: Some(SESSION_ID.into()), ..Default::default() };
                let response = complete(&catalog_model("openai", "gpt-5.4"), &reply_context("openai cache affinity e2e success"), options).await;
                assert_ne!(response.stop_reason, StopReason::Error, "{:?}", response.error_message);
                assert_eq!(response.error_message, None);
                assert!(text_of(&response).contains("openai cache affinity e2e success"));
            })
        },
    )
    .await;
}

// ---------------------------------------------------------------------------
// openrouter-cache-write-repro.test.ts
// ---------------------------------------------------------------------------

/// Mark the last user text part with an ephemeral cache breakpoint.
fn mark_last_user_text(payload: &mut Value) {
    let Some(messages) = payload["messages"].as_array_mut() else { return };
    for message in messages.iter_mut().rev() {
        if message["role"] != "user" {
            continue;
        }
        if let Some(text) = message["content"].as_str().map(str::to_string) {
            message["content"] = json!([{"type": "text", "text": text, "cache_control": {"type": "ephemeral"}}]);
            break;
        }
        if let Some(parts) = message["content"].as_array_mut()
            && let Some(part) = parts.iter_mut().rev().find(|p| p["type"] == "text")
        {
            part["cache_control"] = json!({"type": "ephemeral"});
        }
        break;
    }
}

#[tokio::test]
#[ignore = "live: needs provider credentials"]
async fn openrouter_cache_write_repro_e2e() {
    run_case(
        "openrouter-cache-write-repro.test.ts",
        "regression: preserves cache_write_tokens on openai-completions stream path",
        &[Gate::Env(&["OPENROUTER_API_KEY"])],
        2,
        |_| {
            Box::pin(async move {
                let model = catalog_model("openrouter", "google/gemini-2.5-flash");
                let filler = "Prompt-caching probe content. Keep this exact text stable across requests so the provider can reuse prefix tokens and report cache read and cache write usage.";
                let system = format!("You are a concise assistant.\nCache nonce: {}-{}\n\n{}", now(), rand_suffix(), vec![filler; 80].join("\n\n"));
                let context = ctx(json!({"systemPrompt": system, "messages": [{"role": "user", "content": "Reply with exactly: OK", "timestamp": now()}]}));
                let options = StreamOptions {
                    api_key: env("OPENROUTER_API_KEY"),
                    max_tokens: Some(32),
                    temperature: Some(0.0),
                    on_payload: Some(payload_rewriter(mark_last_user_text)),
                    ..Default::default()
                };
                let first = complete_simple(&model, &context, options.clone()).await;
                assert_eq!(first.stop_reason, StopReason::Stop, "{:?}", first.error_message);
                let second = complete_simple(&model, &context, options).await;
                assert_eq!(second.stop_reason, StopReason::Stop, "{:?}", second.error_message);
                assert!(first.usage.cache_write > 0 || second.usage.cache_write > 0);
            })
        },
    )
    .await;
}

fn rand_suffix() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_i64(now());
    hasher.finish()
}

// ---------------------------------------------------------------------------
// anthropic-long-cache-retention-e2e.test.ts / anthropic-eager-tool-input-e2e.test.ts
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct ProbeCase {
    name: String,
    provider: String,
    model: Model,
}

/// Every `anthropic-messages` model of every builtin provider.
fn anthropic_messages_cases() -> Vec<ProbeCase> {
    get_builtin_providers()
        .into_iter()
        .flat_map(|provider| {
            get_builtin_models(&provider)
                .into_iter()
                .filter(|m| m.api == "anthropic-messages")
                .map(move |model| ProbeCase { name: format!("{provider}/{}", model.id), provider: provider.clone(), model })
        })
        .collect()
}

fn probe_priority(model: &Model) -> f64 {
    let id = model.id.to_lowercase();
    let mut priority = model.cost.input + model.cost.output;
    if id.contains("haiku") && (id.contains("4-5") || id.contains("4.5")) {
        priority -= 1000.0;
    } else if id.contains("sonnet") && (id.contains("4-") || id.contains("4.")) {
        priority -= 750.0;
    } else if id.contains("claude") && (id.contains("4-") || id.contains("4.")) {
        priority -= 500.0;
    }
    priority
}

/// The cheapest current Claude-like model per provider (TS `selectOneCasePerProvider`).
fn one_case_per_provider(cases: Vec<ProbeCase>) -> Vec<ProbeCase> {
    let mut by_provider: indexmap::IndexMap<String, Vec<ProbeCase>> = indexmap::IndexMap::new();
    for case in cases {
        by_provider.entry(case.provider.clone()).or_default().push(case);
    }
    by_provider
        .into_values()
        .map(|mut cases| {
            cases.sort_by(|a, b| {
                probe_priority(&a.model).partial_cmp(&probe_priority(&b.model)).unwrap().then_with(|| a.model.id.cmp(&b.model.id))
            });
            cases.remove(0)
        })
        .collect()
}

/// `getE2EApiKey`: Copilot from `~/.pi/agent/auth.json`, everything else from the environment.
async fn probe_api_key(provider: &str) -> Option<String> {
    if provider == "github-copilot" {
        return resolve_api_key("github-copilot").await;
    }
    pi_ai::env_api_keys::get_env_api_key(provider, None)
}

async fn run_probes(file: &str, cases: Vec<ProbeCase>, suffix: &str, probe: fn(Model, String) -> CaseFuture) {
    let mut failures = Vec::new();
    for case in cases {
        let Some(key) = probe_api_key(&case.provider).await else {
            println!("[{file}] skip {} {suffix} (no api key)", case.name);
            continue;
        };
        match attempt(2, || probe(case.model.clone(), key.clone())).await {
            Ok(()) => println!("[{file}] ok   {} {suffix}", case.name),
            Err(error) => failures.push(format!("{} {suffix}: {error}", case.name)),
        }
    }
    assert!(failures.is_empty(), "{file}:\n{}", failures.join("\n"));
}

#[test]
fn anthropic_messages_probe_cases_cover_every_model() {
    let cases = anthropic_messages_cases();
    let mut names: Vec<String> = cases.iter().map(|c| c.name.clone()).collect();
    let mut expected: Vec<String> = get_builtin_providers()
        .into_iter()
        .flat_map(|p| get_builtin_models(&p).into_iter().filter(|m| m.api == "anthropic-messages").map(move |m| format!("{p}/{}", m.id)))
        .collect();
    names.sort();
    expected.sort();
    assert_eq!(names, expected);
    let probes = one_case_per_provider(cases);
    let providers: std::collections::HashSet<&str> = probes.iter().map(|c| c.provider.as_str()).collect();
    assert_eq!(providers.len(), probes.len(), "one probe per provider");
    let anthropic = probes.iter().find(|c| c.provider == "anthropic").expect("anthropic probe");
    assert!(anthropic.model.id.contains("haiku"), "{}", anthropic.model.id);
}

fn long_cache_probe(model: Model, key: String) -> CaseFuture {
    Box::pin(async move {
        let model = with_compat(&model, "supportsLongCacheRetention", json!(true));
        let mut options = StreamOptions { api_key: Some(key), cache_retention: Some(CacheRetention::Long), max_tokens: Some(128), ..Default::default() };
        options.extra.insert("thinkingEnabled".into(), json!(false));
        let context = ctx(json!({"systemPrompt": "You are a concise assistant.",
            "messages": [{"role": "user", "content": "Reply with exactly: long cache retention accepted", "timestamp": now()}]}));
        let response = complete(&model, &context, options).await;
        assert!(response.error_message.as_deref().unwrap_or_default().is_empty(), "{:?}", response.error_message);
        assert_ne!(response.stop_reason, StopReason::Error);
    })
}

#[tokio::test]
#[ignore = "live: needs provider credentials"]
async fn anthropic_long_cache_retention_e2e() {
    run_probes(
        "anthropic-long-cache-retention-e2e.test.ts",
        one_case_per_provider(anthropic_messages_cases()),
        "accepts long cache retention",
        long_cache_probe,
    )
    .await;
}

fn eager_tool_probe(model: Model, key: String) -> CaseFuture {
    Box::pin(async move {
        let mut options = StreamOptions { api_key: Some(key), max_tokens: Some(128), ..Default::default() };
        options.extra.insert("thinkingEnabled".into(), json!(false));
        let context = ctx(json!({"systemPrompt": "You are a concise assistant. Use tools when useful.",
            "messages": [{"role": "user", "content": "Call echo_value with value set to eager-input-streaming-compat.", "timestamp": now()}],
            "tools": [{"name": "echo_value", "description": "Echo a string value", "parameters": {"type": "object",
                "properties": {"value": {"type": "string", "description": "The value to echo"}}, "required": ["value"]}}]}));
        let response = complete(&model, &context, options).await;
        assert!(response.error_message.as_deref().unwrap_or_default().is_empty(), "{:?}", response.error_message);
        assert_ne!(response.stop_reason, StopReason::Error);
    })
}

fn forced_eager_probe(model: Model, key: String) -> CaseFuture {
    eager_tool_probe(with_compat(&model, "supportsEagerToolInputStreaming", json!(true)), key)
}

#[tokio::test]
#[ignore = "live: needs provider credentials"]
async fn anthropic_eager_tool_input_e2e() {
    const FILE: &str = "anthropic-eager-tool-input-e2e.test.ts";
    let cases = anthropic_messages_cases();
    run_probes(FILE, one_case_per_provider(cases.clone()), "accepts configured tool streaming", eager_tool_probe).await;
    let eager_capable = cases
        .into_iter()
        .filter(|c| serde_json::to_value(&c.model).unwrap()["compat"]["supportsEagerToolInputStreaming"] != json!(false))
        .collect();
    run_probes(FILE, one_case_per_provider(eager_capable), "accepts forced eager_input_streaming", forced_eager_probe).await;
}

// ---------------------------------------------------------------------------
// zen.test.ts
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "live: needs provider credentials"]
async fn opencode_models_smoke() {
    const FILE: &str = "zen.test.ts";
    if env("OPENCODE_API_KEY").is_none() {
        println!("[{FILE}] skip (OPENCODE_API_KEY not configured)");
        return;
    }
    let mut failures = Vec::new();
    for (provider, label) in [("opencode", "OpenCode Zen"), ("opencode-go", "OpenCode Go")] {
        for model in get_builtin_models(provider) {
            let context = ctx(json!({"messages": [{"role": "user", "content": "Say hello.", "timestamp": now()}]}));
            let response = complete(&model, &context, StreamOptions::default()).await;
            if response.stop_reason != StopReason::Stop {
                failures.push(format!("{label}: {} -> {:?} {:?}", model.id, response.stop_reason, response.error_message));
            } else {
                println!("[{FILE}] ok   {label}: {}", model.id);
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
