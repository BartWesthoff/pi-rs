//! Prefix-cache stability for local `openai-completions` servers (llama.cpp, NInfer).
//!
//! Those servers reuse KV state only for a byte-identical prefix, so every request of a session
//! must extend the previous one: the request head and every earlier message are sent byte for
//! byte as before, and the `tools` declaration does not change. Re-setting the same active tools
//! in another order (as packages calling `setActiveTools` do) must not rewrite the head (#11495).

#[path = "../../pi-ai/tests/mock_http/mod.rs"]
mod mock_http;

use std::sync::Arc;

use mock_http::{MockServer, Request, Response};
use pi_agent::types::ThinkingLevel;
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::model_registry::ModelRegistry;
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use pi_coding_agent::core::models_store::InMemoryCodingAgentModelsStore;
use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::settings_manager::SettingsManager;
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::{Value, json};

const TURNS: usize = 5;

fn chunk(delta: Value, finish: Option<&str>) -> Value {
    json!({"id": "chatcmpl-local", "object": "chat.completion.chunk", "model": "local-model",
           "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
}

fn sse(chunks: &[Value]) -> Response {
    let mut body = String::new();
    for chunk in chunks {
        body.push_str(&format!("data: {chunk}\n\n"));
    }
    body.push_str("data: [DONE]\n\n");
    Response::sse(&body)
}

/// llama.cpp-shaped replies: even requests answer a user prompt with reasoning, text and a
/// `read` tool call; odd requests answer the tool result with text.
fn reply(index: usize) -> Response {
    let turn = index / 2;
    let usage = json!({"id": "chatcmpl-local", "choices": [],
                       "usage": {"prompt_tokens": 100 + index, "completion_tokens": 10, "total_tokens": 110 + index}});
    if index % 2 == 0 {
        sse(&[
            chunk(json!({"role": "assistant", "content": null}), None),
            chunk(json!({"reasoning_content": format!("Turn {turn}: read the notes ✓")}), None),
            chunk(json!({"content": "Reading."}), None),
            chunk(
                json!({"tool_calls": [{"index": 0, "id": format!("call_{turn}"), "type": "function",
                                       "function": {"name": "read", "arguments": "{\"path\":"}}]}),
                None,
            ),
            chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": "\"notes.txt\"}"}}]}), None),
            chunk(json!({}), Some("tool_calls")),
            usage,
        ])
    } else {
        sse(&[
            chunk(json!({"role": "assistant", "content": null}), None),
            chunk(json!({"content": format!("Done with turn {turn} — café.")}), None),
            chunk(json!({}), Some("stop")),
            usage,
        ])
    }
}

/// The raw `messages` and `tools` of a request body, exactly as sent.
#[derive(Deserialize)]
struct RawBody<'body> {
    #[serde(borrow)]
    messages: &'body RawValue,
    #[serde(borrow)]
    tools: Option<&'body RawValue>,
}

/// The request bytes up to (not including) the closing `]` of `messages`: everything a
/// prefix cache can reuse for the next request.
fn reusable_prefix(body: &str) -> &str {
    let raw: RawBody = serde_json::from_str(body).expect("request body is JSON");
    let messages = raw.messages.get();
    let start = body.find(messages).expect("messages appear verbatim in the body");
    let close = messages.rfind(']').expect("messages is an array");
    &body[..start + close]
}

fn tools(body: &str) -> String {
    let raw: RawBody = serde_json::from_str(body).expect("request body is JSON");
    raw.tools.map(|tools| tools.get().to_string()).unwrap_or_default()
}

#[tokio::test]
async fn local_completions_requests_extend_a_byte_stable_prefix_across_turns() {
    let server = MockServer::start(|_request: &Request, index| reply(index)).await;
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("project");
    let agent_dir = dir.path().join("agent");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(cwd.join("notes.txt"), "alpha\nbeta\n").unwrap();
    // Shaped like a llama.cpp entry of a local models.json; the key is a placeholder.
    let models_path = agent_dir.join("models.json");
    std::fs::write(
        &models_path,
        serde_json::to_string_pretty(&json!({
            "providers": {
                "local-llama": {
                    "baseUrl": format!("{}/v1/", server.url),
                    "api": "openai-completions",
                    "apiKey": "sk-local-placeholder",
                    "models": [{
                        "id": "local-model", "name": "Local model", "reasoning": true, "input": ["text"],
                        "contextWindow": 262144, "maxTokens": 4096,
                        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0}
                    }]
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let runtime = ModelRuntime::create(Some(&CreateModelRuntimeOptions {
        credentials: Some(AuthStorage::create(Some(&agent_dir.join("auth.json").to_string_lossy()))),
        models_path: Some(models_path.to_string_lossy().into_owned()),
        models_store: Some(Arc::new(InMemoryCodingAgentModelsStore::new())),
        allow_model_network: Some(false),
        ..Default::default()
    }))
    .await
    .unwrap();
    let model = ModelRegistry::new(runtime.clone()).find("local-llama", "local-model").expect("models.json model");
    let cwd_str = cwd.to_string_lossy().into_owned();
    let created = create_agent_session(Some(&CreateAgentSessionOptions {
        cwd: Some(cwd_str.clone()),
        agent_dir: Some(agent_dir.to_string_lossy().into_owned()),
        model_runtime: Some(runtime),
        model: Some(model),
        thinking_level: Some(ThinkingLevel::Medium),
        settings_manager: Some(SettingsManager::in_memory(None, None)),
        session_manager: Some(SessionManager::in_memory(Some(&cwd_str), None, None).unwrap()),
        ..Default::default()
    }))
    .await
    .unwrap();
    let session = created.session;

    for turn in 0..TURNS {
        if turn == 3 {
            // A package re-applying the same tool set in another order (pi-web-access builds
            // `[...others, ...names, loader]`) must not touch the prompt or the declarations.
            let reversed: Vec<String> = session.get_active_tool_names().into_iter().rev().collect();
            session.set_active_tools_by_name(&reversed);
        }
        session.prompt(&format!("Turn {turn}: summarize notes.txt"), None).await.unwrap();
        let last = session.messages().last().cloned().expect("assistant reply");
        let last = serde_json::to_value(&last).unwrap();
        assert_eq!(last["stopReason"], "stop", "turn {turn}: {last}");
    }

    let requests = server.requests();
    assert_eq!(requests.len(), TURNS * 2);
    assert!(requests.iter().all(|r| r.path == "/v1/chat/completions"), "trailing-slash baseUrl is joined once");
    let bodies: Vec<String> = requests.iter().map(Request::text).collect();
    let first_tools = tools(&bodies[0]);
    assert!(first_tools.contains("\"read\""), "{first_tools}");
    for (index, pair) in bodies.windows(2).enumerate() {
        let (previous, next) = (&pair[0], &pair[1]);
        let prefix = reusable_prefix(previous);
        assert!(
            next.starts_with(prefix),
            "request {} does not extend request {index} byte for byte\nprevious: {prefix}\nnext:     {next}",
            index + 1,
        );
        assert_eq!(tools(next), first_tools, "request {} changed the tools declaration", index + 1);
    }
    // Exactly one instruction message: the reorder did not patch the prompt.
    let last: Value = serde_json::from_str(&bodies[TURNS * 2 - 1]).unwrap();
    let instructions = last["messages"].as_array().unwrap().iter().filter(|m| m["role"] == "developer" || m["role"] == "system");
    assert_eq!(instructions.count(), 1);
    session.dispose();
}
