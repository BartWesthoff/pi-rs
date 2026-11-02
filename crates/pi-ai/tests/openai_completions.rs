//! Offline tests for the OpenAI Chat Completions API against a local mock server.

mod common;

use common::*;
use pi_ai::api::openai_completions;
use pi_ai::providers::all::get_builtin_model;
use pi_ai::*;
use serde_json::json;

fn model_at(base_url: &str, provider: &str, id: &str) -> Model {
    let mut model = get_builtin_model(provider, id).unwrap_or_else(|| panic!("{provider}/{id}"));
    model.base_url = base_url.to_string();
    model
}

fn custom_model(base_url: &str) -> Model {
    Model {
        id: "local-model".into(),
        name: "Local".into(),
        api: "openai-completions".into(),
        provider: "local".into(),
        base_url: base_url.into(),
        reasoning: true,
        input: vec![ModelInputModality::Text],
        context_window: 32000,
        max_tokens: 4096,
        ..Default::default()
    }
}

fn context() -> TranscriptContext {
    normalize_context(&Context {
        system_prompt: Some("Be brief.".into()),
        messages: vec![Message::User(UserMessage::new("hi", 1))],
        tools: Some(vec![Tool::new("echo", "Echo", json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}))]),
    })
}

fn chunk(delta: serde_json::Value, finish: Option<&str>) -> serde_json::Value {
    json!({"id": "chatcmpl-1", "model": "local-model", "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
}

#[tokio::test]
async fn streams_reasoning_text_and_tool_calls() {
    let body = openai_sse_body(&[
        chunk(json!({"role": "assistant", "reasoning_content": "think "}), None),
        chunk(json!({"reasoning_content": "more"}), None),
        chunk(json!({"content": "Hello"}), None),
        chunk(json!({"tool_calls": [{"index": 0, "id": "call_1", "type": "function", "function": {"name": "echo", "arguments": "{\"te"}}]}), None),
        chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": "xt\": \"hi\"}"}}]}), None),
        chunk(json!({}), Some("tool_calls")),
        json!({"id": "chatcmpl-1", "choices": [], "usage": {"prompt_tokens": 20, "completion_tokens": 7, "prompt_tokens_details": {"cached_tokens": 5}, "completion_tokens_details": {"reasoning_tokens": 3}}}),
    ]);
    let server = serve(vec![MockResponse::sse(body)]).await;
    let model = custom_model(&server.base_url);
    let options = StreamOptions { api_key: Some("sk-local".into()), reasoning: Some(ThinkingLevel::High), ..Default::default() };
    let (events, result) = openai_completions::stream_simple(&model, &context(), options).collect_all().await;
    assert_eq!(result.stop_reason, StopReason::ToolUse, "{:?}", result.error_message);
    assert_eq!(result.response_id.as_deref(), Some("chatcmpl-1"));
    let thinking = result.content[0].as_thinking().unwrap();
    assert_eq!(thinking.thinking, "think more");
    assert_eq!(thinking.thinking_signature.as_deref(), Some("reasoning_content"));
    assert_eq!(result.content[1].as_text().unwrap().text, "Hello");
    let call = result.tool_calls().next().unwrap();
    assert_eq!(call.id, "call_1");
    assert_eq!(serde_json::Value::Object(call.arguments.clone()), json!({"text": "hi"}));
    assert_eq!(result.usage.input, 15);
    assert_eq!(result.usage.cache_read, 5);
    assert_eq!(result.usage.output, 7);
    assert_eq!(result.usage.reasoning, Some(3));
    let types: Vec<&str> = events.iter().map(|e| e.type_name()).collect();
    assert_eq!(types.first(), Some(&"start"));
    assert_eq!(types.last(), Some(&"done"));
    assert!(types.contains(&"toolcall_end"));

    let request = server.requests.lock()[0].clone();
    assert_eq!(request.path, "/chat/completions");
    assert_eq!(request.header("authorization"), Some("Bearer sk-local"));
    let body = request.json();
    assert_eq!(body["model"], "local-model");
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"], json!({"include_usage": true}));
    assert_eq!(body["store"], false);
    assert_eq!(body["reasoning_effort"], "high");
    assert_eq!(body["messages"][0], json!({"role": "developer", "content": "Be brief."}));
    assert_eq!(body["messages"][1], json!({"role": "user", "content": "hi"}));
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["function"]["name"], "echo");
    assert!(body["tools"][0]["function"].get("strict").is_none());
    assert!(body["max_completion_tokens"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn converts_history_with_tool_results_and_images() {
    let server = serve(vec![MockResponse::sse(openai_sse_body(&[chunk(json!({"content": "ok"}), Some("stop"))]))]).await;
    let mut model = custom_model(&server.base_url);
    model.input = vec![ModelInputModality::Text, ModelInputModality::Image];
    let assistant = AssistantMessage {
        content: vec![
            AssistantContent::Text(TextContent::new("calling")),
            AssistantContent::ToolCall(ToolCall {
                id: "call|item".into(),
                name: "echo".into(),
                arguments: json!({"text": "x"}).as_object().unwrap().clone(),
                ..Default::default()
            }),
        ].into(),
        api: "openai-responses".into(),
        provider: "openai".into(),
        model: "gpt".into(),
        stop_reason: StopReason::ToolUse,
        timestamp: 2,
        ..Default::default()
    };
    let tool_result = ToolResultMessage {
        tool_call_id: "call|item".into(),
        tool_name: "echo".into(),
        content: vec![UserContentBlock::Image(ImageContent { data: "AAAA".into(), mime_type: "image/png".into() })],
        timestamp: 3,
        ..Default::default()
    };
    let ctx = normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage::new("hi", 1)), assistant.into(), tool_result.into()],
        tools: None,
    });
    let result = openai_completions::stream(&model, &ctx, StreamOptions { api_key: Some("k".into()), ..Default::default() }).result().await;
    assert_eq!(result.stop_reason, StopReason::Stop, "{:?}", result.error_message);
    let body = server.requests.lock()[0].json();
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages[1]["content"], "calling");
    assert_eq!(messages[1]["tool_calls"][0]["id"], "call_item");
    assert_eq!(messages[1]["tool_calls"][0]["function"]["arguments"], "{\"text\":\"x\"}");
    assert_eq!(messages[2], json!({"role": "tool", "content": "(see attached image)", "tool_call_id": "call_item"}));
    assert_eq!(messages[3]["role"], "user");
    assert_eq!(messages[3]["content"][1]["image_url"]["url"], "data:image/png;base64,AAAA");
    assert_eq!(body["tools"], json!([]));
}

#[tokio::test]
async fn errors_and_missing_finish_reason() {
    let server = serve(vec![MockResponse::json(429, json!({"error": {"message": "Rate limit reached", "type": "rate_limit"}}).to_string())]).await;
    let model = custom_model(&server.base_url);
    let result = openai_completions::stream(&model, &context(), StreamOptions { api_key: Some("k".into()), ..Default::default() }).result().await;
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(result.error_message.as_deref(), Some(r#"429: {"message":"Rate limit reached","type":"rate_limit"}"#));
    assert!(is_retryable_assistant_error(&result));

    let server = serve(vec![MockResponse::sse(openai_sse_body(&[chunk(json!({"content": "partial"}), None)]))]).await;
    let model = custom_model(&server.base_url);
    let result = openai_completions::stream(&model, &context(), StreamOptions { api_key: Some("k".into()), ..Default::default() }).result().await;
    assert_eq!(result.error_message.as_deref(), Some("Stream ended without finish_reason"));

    let server = serve(vec![MockResponse::sse(
        "data: {\"error\":{\"message\":\"Provider returned error\",\"metadata\":{\"raw\":\"upstream boom\"}}}\n\n".to_string(),
    )])
    .await;
    let model = custom_model(&server.base_url);
    let result = openai_completions::stream(&model, &context(), StreamOptions { api_key: Some("k".into()), ..Default::default() }).result().await;
    assert_eq!(result.error_message.as_deref(), Some("Provider returned error\nupstream boom"));
}

#[tokio::test]
async fn provider_compat_detection_shapes_requests() {
    let server = serve(vec![
        MockResponse::sse(openai_sse_body(&[chunk(json!({"content": "ok"}), Some("stop"))])),
        MockResponse::sse(openai_sse_body(&[chunk(json!({"content": "ok"}), Some("stop"))])),
    ])
    .await;
    let deepseek = model_at(&server.base_url, "deepseek", get_builtin_models("deepseek")[0].id.as_str());
    openai_completions::stream_simple(&deepseek, &context(), StreamOptions { api_key: Some("k".into()), reasoning: Some(ThinkingLevel::High), ..Default::default() })
        .result()
        .await;
    let body = server.requests.lock()[0].json();
    if deepseek.reasoning {
        assert_eq!(body["thinking"], json!({"type": "enabled"}));
    }
    assert!(body.get("max_tokens").is_some());
    assert!(body.get("store").is_none());

    let mut openrouter = model_at(&server.base_url, "openrouter", "anthropic/claude-haiku-4.5");
    openrouter.base_url = server.base_url.clone();
    openai_completions::stream(&openrouter, &context(), StreamOptions { api_key: Some("k".into()), session_id: Some("sess".into()), ..Default::default() })
        .result()
        .await;
    let request = server.requests.lock()[1].clone();
    assert_eq!(request.header("x-session-id"), Some("sess"));
    let body = request.json();
    assert_eq!(body["messages"][0]["content"][0]["cache_control"], json!({"type": "ephemeral"}));
    assert_eq!(body["tools"][0]["cache_control"], json!({"type": "ephemeral"}));
}

use pi_ai::providers::all::get_builtin_models;

// ---------------------------------------------------------------------------
// Local llama.cpp server (`llama-server --reasoning-format deepseek`) streams, as served to
// the `openai-completions` providers of a local models.json.
// ---------------------------------------------------------------------------

mod mock_http;

use std::time::{Duration, Instant};

use serde_json::Value;

/// A models.json entry for a local llama.cpp model: no `compat`, sampling params, reasoning.
fn llama_cpp_model(base_url: &str) -> Model {
    let mut sampling = serde_json::Map::new();
    sampling.insert("temperature".into(), json!(0.6));
    sampling.insert("top_p".into(), json!(0.95));
    Model {
        id: "lfm3-qad".into(),
        name: "LFM3".into(),
        api: "openai-completions".into(),
        provider: "lfm3-llama".into(),
        base_url: format!("{base_url}/v1"),
        reasoning: true,
        input: vec![ModelInputModality::Text],
        context_window: 262_144,
        max_tokens: 4096,
        sampling_params: Some(sampling),
        ..Default::default()
    }
}

/// One `chat.completion.chunk` exactly as llama.cpp's server frames it.
fn llama_chunk(delta: Value, finish: Option<&str>) -> Value {
    json!({
        "choices": [{"finish_reason": finish, "index": 0, "delta": delta}],
        "created": 1_793_527_409, "id": "chatcmpl-llama", "model": "lfm3-qad",
        "system_fingerprint": "b12024-ddf48f85c", "object": "chat.completion.chunk"
    })
}

/// llama.cpp's trailing usage chunk: empty `choices`, `usage` and its own `timings`.
fn llama_usage_chunk(prompt: u64, completion: u64) -> Value {
    json!({
        "choices": [], "created": 1_793_527_409, "id": "chatcmpl-llama", "model": "lfm3-qad",
        "system_fingerprint": "b12024-ddf48f85c", "object": "chat.completion.chunk",
        "usage": {"completion_tokens": completion, "prompt_tokens": prompt, "total_tokens": prompt + completion,
                  "prompt_tokens_details": {"cached_tokens": 3}},
        "timings": {"cache_n": 3, "prompt_n": prompt - 3, "prompt_ms": 211.5, "predicted_n": completion, "predicted_ms": 82.6}
    })
}

/// A turn with reasoning, answer text (multi-byte characters included) and two parallel tool
/// calls whose arguments arrive in pieces.
fn llama_tool_turn() -> Vec<Value> {
    vec![
        llama_chunk(json!({"role": "assistant", "content": null}), None),
        llama_chunk(json!({"reasoning_content": "Read both "}), None),
        llama_chunk(json!({"reasoning_content": "files ✓ — café"}), None),
        llama_chunk(json!({"content": "Reading 📄 now."}), None),
        llama_chunk(
            json!({"tool_calls": [{"index": 0, "id": "call_a", "type": "function", "function": {"name": "read", "arguments": ""}}]}),
            None,
        ),
        llama_chunk(
            json!({"tool_calls": [{"index": 0, "function": {"arguments": "{\"path\":"}}]}),
            None,
        ),
        llama_chunk(
            json!({"tool_calls": [{"index": 0, "function": {"arguments": "\"héllo.txt\"}"}}]}),
            None,
        ),
        llama_chunk(
            json!({"tool_calls": [{"index": 1, "id": "call_b", "type": "function", "function": {"name": "bash", "arguments": "{\"command\":"}}]}),
            None,
        ),
        llama_chunk(
            json!({"tool_calls": [{"index": 1, "function": {"arguments": "\"ls -la\"}"}}]}),
            None,
        ),
        llama_chunk(json!({}), Some("tool_calls")),
        llama_usage_chunk(120, 40),
    ]
}

fn llama_context() -> TranscriptContext {
    normalize_context(&Context {
        system_prompt: Some("Be brief.".into()),
        messages: vec![Message::User(UserMessage::new("go", 1))],
        tools: Some(vec![
            Tool::new(
                "read",
                "Read",
                json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
            ),
            Tool::new(
                "bash",
                "Bash",
                json!({"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]}),
            ),
        ]),
    })
}

fn local_options() -> StreamOptions {
    StreamOptions {
        api_key: Some("local-key".into()),
        ..Default::default()
    }
}

/// Event types with their text payloads, without the timestamped partial messages.
fn event_trace(events: &[AssistantMessageEvent]) -> Vec<String> {
    events
        .iter()
        .map(|event| match event {
            AssistantMessageEvent::TextDelta {
                content_index,
                delta,
                ..
            }
            | AssistantMessageEvent::ThinkingDelta {
                content_index,
                delta,
                ..
            }
            | AssistantMessageEvent::ToolCallDelta {
                content_index,
                delta,
                ..
            } => {
                format!("{} {content_index} {delta}", event.type_name())
            }
            AssistantMessageEvent::ToolCallEnd {
                content_index,
                tool_call,
                ..
            } => {
                format!(
                    "toolcall_end {content_index} {} {} {:?}",
                    tool_call.id, tool_call.name, tool_call.arguments
                )
            }
            other => other.type_name().to_string(),
        })
        .collect()
}

/// Split `body` into chunks of `size` bytes, ignoring UTF-8 character boundaries.
fn split_bytes(body: &str, size: usize) -> Vec<Vec<u8>> {
    body.as_bytes().chunks(size).map(<[u8]>::to_vec).collect()
}

#[tokio::test]
async fn llama_cpp_stream_parses_identically_at_any_byte_split() {
    let body = openai_sse_body(&llama_tool_turn());
    let whole = mock_http::MockServer::sequence(vec![mock_http::Response::sse(&body)]).await;
    let (reference_events, reference) = openai_completions::stream(
        &llama_cpp_model(&whole.url),
        &llama_context(),
        local_options(),
    )
    .collect_all()
    .await;
    assert_eq!(
        reference.stop_reason,
        StopReason::ToolUse,
        "{:?}",
        reference.error_message
    );
    let thinking = reference.content[0]
        .as_thinking()
        .expect("thinking block first");
    assert_eq!(thinking.thinking, "Read both files ✓ — café");
    assert_eq!(
        thinking.thinking_signature.as_deref(),
        Some("reasoning_content")
    );
    assert_eq!(
        reference.content[1].as_text().expect("text block").text,
        "Reading 📄 now."
    );
    let calls: Vec<&ToolCall> = reference.tool_calls().collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        (calls[0].id.as_str(), calls[0].name.as_str()),
        ("call_a", "read")
    );
    assert_eq!(
        Value::Object(calls[0].arguments.clone()),
        json!({"path": "héllo.txt"})
    );
    assert_eq!(
        (calls[1].id.as_str(), calls[1].name.as_str()),
        ("call_b", "bash")
    );
    assert_eq!(
        Value::Object(calls[1].arguments.clone()),
        json!({"command": "ls -la"})
    );
    assert_eq!(
        (
            reference.usage.input,
            reference.usage.cache_read,
            reference.usage.output
        ),
        (117, 3, 40)
    );
    assert_eq!(reference.response_id.as_deref(), Some("chatcmpl-llama"));
    assert_eq!(
        reference.response_model, None,
        "chunk model equals the requested id"
    );
    let reference_trace = event_trace(&reference_events);

    for size in [1, 2, 3, 5, 7, 11, 64, 333] {
        let server = mock_http::MockServer::sequence(vec![mock_http::Response::chunked(
            200,
            "text/event-stream",
            split_bytes(&body, size),
        )])
        .await;
        let (events, message) = openai_completions::stream(
            &llama_cpp_model(&server.url),
            &llama_context(),
            local_options(),
        )
        .collect_all()
        .await;
        assert_eq!(message.content, reference.content, "split at {size} bytes");
        assert_eq!(message.usage, reference.usage, "split at {size} bytes");
        assert_eq!(
            message.stop_reason,
            StopReason::ToolUse,
            "split at {size} bytes: {:?}",
            message.error_message
        );
        assert_eq!(
            event_trace(&events),
            reference_trace,
            "split at {size} bytes"
        );
    }
}

#[tokio::test]
async fn llama_cpp_request_shape_for_a_local_model_entry() {
    let server =
        mock_http::MockServer::sequence(vec![mock_http::Response::sse(&openai_sse_body(&[
            llama_chunk(json!({"content": "ok"}), None),
            llama_chunk(json!({}), Some("stop")),
        ]))])
        .await;
    let options = StreamOptions {
        reasoning: Some(ThinkingLevel::Medium),
        max_tokens: Some(4096),
        ..local_options()
    };
    let result =
        openai_completions::stream_simple(&llama_cpp_model(&server.url), &llama_context(), options)
            .result()
            .await;
    assert_eq!(
        result.stop_reason,
        StopReason::Stop,
        "{:?}",
        result.error_message
    );
    let request = server.request(0);
    assert_eq!(request.path, "/v1/chat/completions");
    assert_eq!(request.header("authorization"), Some("Bearer local-key"));
    assert_eq!(request.header("accept"), Some("application/json"));
    // Same body as TS pi 1.4.0 sends for this models.json entry (key order aside).
    assert_eq!(
        request.json(),
        json!({
            "model": "lfm3-qad",
            "messages": [{"role": "developer", "content": "Be brief."}, {"role": "user", "content": "go"}],
            "stream": true,
            "stream_options": {"include_usage": true},
            "store": false,
            "max_completion_tokens": 4096,
            "tools": [
                {"type": "function", "function": {"name": "read", "description": "Read", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}}},
                {"type": "function", "function": {"name": "bash", "description": "Bash", "parameters": {"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]}}}
            ],
            "reasoning_effort": "medium",
            "temperature": 0.6,
            "top_p": 0.95
        })
    );
}

#[tokio::test]
async fn stream_outlives_the_request_timeout_while_chunks_keep_arriving() {
    // The SDK timeout covers waiting for the response, not reading the body: a long local
    // generation that keeps streaming must not be cut off when the total time passes it.
    let chunks = vec![
        llama_chunk(json!({"reasoning_content": "slow "}), None),
        llama_chunk(json!({"content": "one "}), None),
        llama_chunk(json!({"content": "two "}), None),
        llama_chunk(json!({"content": "three"}), None),
        llama_chunk(json!({}), Some("stop")),
    ];
    let body: Vec<Vec<u8>> = openai_sse_body(&chunks)
        .split_inclusive("\n\n")
        .map(|frame| frame.as_bytes().to_vec())
        .collect();
    let server = mock_http::MockServer::sequence(vec![
        mock_http::Response::chunked(200, "text/event-stream", body).with_chunk_delay(150),
    ])
    .await;
    let options = StreamOptions {
        timeout_ms: Some(400),
        ..local_options()
    };
    let started = Instant::now();
    let result =
        openai_completions::stream(&llama_cpp_model(&server.url), &llama_context(), options)
            .result()
            .await;
    assert!(
        started.elapsed() > Duration::from_millis(600),
        "the stream ran past the timeout: {:?}",
        started.elapsed()
    );
    assert_eq!(
        result.stop_reason,
        StopReason::Stop,
        "{:?}",
        result.error_message
    );
    assert_eq!(result.text(), "one two three");
}

#[tokio::test]
async fn request_timeout_covers_waiting_for_the_response() {
    let server = mock_http::MockServer::sequence(vec![
        mock_http::Response::sse(&openai_sse_body(&[llama_chunk(json!({}), Some("stop"))]))
            .with_delay(1_000),
    ])
    .await;
    let options = StreamOptions {
        timeout_ms: Some(200),
        ..local_options()
    };
    let started = Instant::now();
    let result =
        openai_completions::stream(&llama_cpp_model(&server.url), &llama_context(), options)
            .result()
            .await;
    assert!(
        started.elapsed() < Duration::from_millis(900),
        "timed out before the response: {:?}",
        started.elapsed()
    );
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(result.error_message.as_deref(), Some("Request timed out."));
}

#[tokio::test]
async fn server_disconnect_mid_stream_fails_like_undici_terminated() {
    // undici rejects the body read with `TypeError: terminated`; the message is what the agent
    // session's retry classification matches, so pi retries a dropped local server like TS.
    let body = openai_sse_body(&[
        llama_chunk(json!({"reasoning_content": "Hmm."}), None),
        llama_chunk(json!({"content": "partial"}), None),
    ]);
    let frames: Vec<Vec<u8>> = body
        .split_inclusive("\n\n")
        .take(2)
        .map(|frame| frame.as_bytes().to_vec())
        .collect();
    let server = mock_http::MockServer::sequence(vec![
        mock_http::Response::chunked(200, "text/event-stream", frames).truncated(),
    ])
    .await;
    let (events, result) = openai_completions::stream(
        &llama_cpp_model(&server.url),
        &llama_context(),
        local_options(),
    )
    .collect_all()
    .await;
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(result.error_message.as_deref(), Some("terminated"));
    assert!(is_retryable_assistant_error(&result));
    // The partial content stays on the error message.
    assert_eq!(
        result.content[0].as_thinking().expect("thinking").thinking,
        "Hmm."
    );
    assert_eq!(result.content[1].as_text().expect("text").text, "partial");
    assert_eq!(
        events.last().map(AssistantMessageEvent::type_name),
        Some("error")
    );
}

#[tokio::test]
async fn idle_body_timeout_mid_stream_fails_like_undici_terminated() {
    // No bytes for the idle timeout (the client's read timeout, undici's `bodyTimeout`) ends
    // the stream with the same retryable `terminated` as a dropped connection.
    let body = openai_sse_body(&[
        llama_chunk(json!({"content": "never"}), None),
        llama_chunk(json!({}), Some("stop")),
    ]);
    let frames: Vec<Vec<u8>> = body
        .split_inclusive("\n\n")
        .map(|frame| frame.as_bytes().to_vec())
        .collect();
    let server = mock_http::MockServer::sequence(vec![
        mock_http::Response::chunked(200, "text/event-stream", frames).with_chunk_delay(900),
    ])
    .await;
    let client = reqwest::Client::builder()
        .read_timeout(Duration::from_millis(300))
        .build()
        .unwrap();
    let options = StreamOptions {
        http_client: Some(client),
        ..local_options()
    };
    let (_, result) =
        openai_completions::stream(&llama_cpp_model(&server.url), &llama_context(), options)
            .collect_all()
            .await;
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(result.error_message.as_deref(), Some("terminated"));
    assert!(is_retryable_assistant_error(&result));
    // The response head arrived, the first body read stalled: no content yet.
    assert!(result.content.is_empty(), "{:?}", result.content);
}

#[tokio::test]
async fn user_abort_mid_stream_reports_aborted() {
    let body = openai_sse_body(&[
        llama_chunk(json!({"content": "first"}), None),
        llama_chunk(json!({"content": " second"}), None),
        llama_chunk(json!({}), Some("stop")),
    ]);
    let frames: Vec<Vec<u8>> = body
        .split_inclusive("\n\n")
        .map(|frame| frame.as_bytes().to_vec())
        .collect();
    let server = mock_http::MockServer::sequence(vec![
        mock_http::Response::chunked(200, "text/event-stream", frames).with_chunk_delay(300),
    ])
    .await;
    let signal = tokio_util::sync::CancellationToken::new();
    let options = StreamOptions {
        signal: Some(signal.clone()),
        ..local_options()
    };
    let mut stream =
        openai_completions::stream(&llama_cpp_model(&server.url), &llama_context(), options);
    let mut types = Vec::new();
    while let Some(event) = futures::StreamExt::next(&mut stream).await {
        if matches!(event, AssistantMessageEvent::TextDelta { .. }) {
            signal.cancel();
        }
        types.push(event.type_name());
        if let AssistantMessageEvent::Error { reason, error } = &event {
            assert_eq!(*reason, StopReason::Aborted);
            assert_eq!(error.stop_reason, StopReason::Aborted);
            assert_eq!(error.error_message.as_deref(), Some("Request was aborted"));
            assert_eq!(error.text(), "first");
        }
    }
    assert_eq!(
        types,
        ["start", "text_start", "text_delta", "text_end", "error"]
    );
}

/// Esc while a local server is still prefilling: the response head has not arrived yet.
#[tokio::test]
async fn user_abort_before_the_response_head_reports_aborted_without_retrying() {
    let body = openai_sse_body(&[llama_chunk(json!({"content": "late"}), None), llama_chunk(json!({}), Some("stop"))]);
    let server = mock_http::MockServer::sequence(vec![mock_http::Response::sse(&body).with_delay(5_000)]).await;
    let signal = tokio_util::sync::CancellationToken::new();
    let options = StreamOptions {
        signal: Some(signal.clone()),
        max_retries: Some(3),
        ..local_options()
    };
    let started = Instant::now();
    let canceller = {
        let signal = signal.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            signal.cancel();
        })
    };
    let (events, result) = openai_completions::stream(&llama_cpp_model(&server.url), &llama_context(), options)
        .collect_all()
        .await;
    canceller.await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(3), "abort did not interrupt the wait for the head: {:?}", started.elapsed());
    assert_eq!(result.stop_reason, StopReason::Aborted);
    assert_eq!(result.error_message.as_deref(), Some("Request aborted"));
    assert!(result.content.is_empty(), "{:?}", result.content);
    let types: Vec<&str> = events.iter().map(|e| e.type_name()).collect();
    // TS 1.4.0 emits no `start` either: the request never produced a response.
    assert_eq!(types, ["error"]);
    assert_eq!(server.requests().len(), 1, "an aborted request is not retried");
}

/// Esc while the model is streaming a tool call's arguments: the partial call stays in the aborted
/// message with its parse error, exactly like TS 1.4.0 on the same stream.
#[tokio::test]
async fn user_abort_mid_tool_call_arguments_reports_aborted() {
    let body = openai_sse_body(&[
        llama_chunk(json!({"role": "assistant", "content": null}), None),
        llama_chunk(
            json!({"tool_calls": [{"index": 0, "id": "call_a", "type": "function", "function": {"name": "bash", "arguments": "{\"command\":"}}]}),
            None,
        ),
        llama_chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": "\"rm -rf"}}]}), None),
        llama_chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": " build\"}"}}]}), None),
        llama_chunk(json!({}), Some("tool_calls")),
    ]);
    let frames: Vec<Vec<u8>> = body.split_inclusive("\n\n").map(|frame| frame.as_bytes().to_vec()).collect();
    let server = mock_http::MockServer::sequence(vec![
        mock_http::Response::chunked(200, "text/event-stream", frames).with_chunk_delay(300),
    ])
    .await;
    let signal = tokio_util::sync::CancellationToken::new();
    let options = StreamOptions { signal: Some(signal.clone()), ..local_options() };
    let mut stream = openai_completions::stream(&llama_cpp_model(&server.url), &llama_context(), options);
    let mut types = Vec::new();
    let mut final_message = None;
    while let Some(event) = futures::StreamExt::next(&mut stream).await {
        if matches!(event, AssistantMessageEvent::ToolCallDelta { .. }) {
            signal.cancel();
        }
        types.push(event.type_name());
        if let AssistantMessageEvent::Error { reason, error } = event {
            assert_eq!(reason, StopReason::Aborted);
            final_message = Some(error);
        }
    }
    assert_eq!(types, ["start", "toolcall_start", "toolcall_delta", "toolcall_end", "error"]);
    let message = final_message.expect("error event");
    assert_eq!(message.stop_reason, StopReason::Aborted);
    assert_eq!(message.error_message.as_deref(), Some("Request was aborted"));
    let calls: Vec<&ToolCall> = message
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantContent::ToolCall(call) => Some(call),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "bash");
    assert!(calls[0].arguments.is_empty(), "partial argument must not look complete: {:?}", calls[0].arguments);
    assert_eq!(
        calls[0].arguments_error.as_deref(),
        Some("Unexpected end of JSON input\n\nReceived arguments:\n{\"command\":")
    );
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn malformed_tool_call_json_finishes_with_an_arguments_error() {
    let raw = "{\"path\": \"hello.txt\", ";
    let server = mock_http::MockServer::sequence(vec![mock_http::Response::sse(&openai_sse_body(&[
        llama_chunk(json!({"tool_calls": [{"index": 0, "id": "call_bad", "type": "function", "function": {"name": "read", "arguments": raw}}]}), None),
        llama_chunk(json!({}), Some("tool_calls")),
    ]))])
    .await;
    let (events, result) = openai_completions::stream(
        &llama_cpp_model(&server.url),
        &llama_context(),
        local_options(),
    )
    .collect_all()
    .await;
    assert_eq!(
        result.stop_reason,
        StopReason::ToolUse,
        "{:?}",
        result.error_message
    );
    let call = result.tool_calls().next().expect("tool call");
    // Streaming repair shows the partial arguments; the final parse reports the raw text.
    assert!(call.arguments.is_empty(), "{:?}", call.arguments);
    let error = call.arguments_error.as_deref().expect("arguments error");
    assert!(
        error.ends_with(&format!("\n\nReceived arguments:\n{raw}")),
        "{error}"
    );
    let streamed = events.iter().find_map(|event| match event {
        AssistantMessageEvent::ToolCallDelta { partial, .. } => partial
            .tool_calls()
            .next()
            .map(|c| Value::Object(c.arguments.clone())),
        _ => None,
    });
    assert_eq!(streamed, Some(json!({"path": "hello.txt"})));
}

#[tokio::test]
async fn llama_cpp_http_errors_surface_the_server_message() {
    let server = mock_http::MockServer::sequence(vec![mock_http::Response::json(
        400,
        json!({"error": {"code": 400, "message": "the request exceeds the available context size, try increasing it", "type": "exceed_context_size_error", "n_prompt_tokens": 9000, "n_ctx": 8192}}),
    )])
    .await;
    let model = llama_cpp_model(&server.url);
    let result = openai_completions::stream(&model, &llama_context(), local_options())
        .result()
        .await;
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(
        result.error_message.as_deref(),
        Some(
            r#"400: {"code":400,"message":"the request exceeds the available context size, try increasing it","type":"exceed_context_size_error","n_prompt_tokens":9000,"n_ctx":8192}"#
        )
    );
    assert!(is_context_overflow(&result, Some(model.context_window)));
    assert!(!is_retryable_assistant_error(&result));
}

/// Port of `streaming-tool-arguments.test.ts` (#9265) for Chat Completions: arguments streamed one
/// character at a time show growing partial arguments and finish with the exact parse.
#[tokio::test]
async fn streams_growing_partial_tool_arguments_one_character_at_a_time() {
    let content = "line \"one\"\nline two \\ done\n".repeat(20);
    let arguments = json!({"text": content}).to_string();
    let mut chunks =
        vec![chunk(json!({"tool_calls": [{"index": 0, "id": "call_1", "type": "function", "function": {"name": "echo", "arguments": ""}}]}), None)];
    for c in arguments.chars() {
        chunks.push(chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": c.to_string()}}]}), None));
    }
    chunks.push(chunk(json!({}), Some("tool_calls")));
    let server = serve(vec![MockResponse::sse(openai_sse_body(&chunks))]).await;
    let model = custom_model(&server.base_url);
    let options = StreamOptions { api_key: Some("sk-local".into()), ..Default::default() };
    let (events, result) = openai_completions::stream_simple(&model, &context(), options).collect_all().await;

    let mut shown = 0;
    let mut deltas = String::new();
    for event in &events {
        if let AssistantMessageEvent::ToolCallDelta { delta, partial, .. } = event {
            deltas.push_str(delta);
            let call = partial.tool_calls().next().expect("a tool call");
            let text = call.arguments.get("text").and_then(serde_json::Value::as_str).unwrap_or_default();
            assert!(content.starts_with(text), "partial arguments are a prefix of the final ones");
            assert!(text.len() >= shown, "partial arguments only grow");
            shown = text.len();
        }
    }
    assert_eq!(deltas, arguments);
    assert!(shown > content.len() / 2, "partial arguments kept up with the stream ({shown})");
    let call = result.tool_calls().next().unwrap();
    assert_eq!(serde_json::Value::Object(call.arguments.clone()), json!({"text": content}));
    assert_eq!(call.arguments_error, None);
}
