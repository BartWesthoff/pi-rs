//! Offline tests for the Mistral conversations API (ports of `mistral-*.test.ts`).

mod mock_http;

use std::sync::Arc;

use mock_http::{MockServer, Response};
use parking_lot::Mutex;
use pi_ai::api::mistral_conversations;
use pi_ai::providers::all::get_builtin_model;
use pi_ai::*;
use serde_json::{Value, json};

fn sse(events: &[Value]) -> String {
    let body: Vec<String> = events.iter().map(|e| format!("data: {e}")).collect();
    format!("{}\r\n\r\ndata: [DONE]\r\n\r\n", body.join("\r\n\r\n"))
}

fn terminal(reason: &str) -> Value {
    json!({ "id": "mistral-response-id", "model": "mistral-large-latest", "choices": [{ "index": 0, "finish_reason": reason, "delta": {} }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 } })
}

fn model_at(base_url: &str, id: &str) -> Model {
    let mut model = get_builtin_model("mistral", id).unwrap();
    model.base_url = base_url.to_string();
    model
}

fn hello() -> TranscriptContext {
    normalize_context(&Context { system_prompt: None, tools: None, messages: vec![Message::User(UserMessage::new("hello", 1))] })
}

fn key(key: &str) -> StreamOptions {
    StreamOptions { api_key: Some(key.into()), ..Default::default() }
}

#[tokio::test]
async fn serializes_sdk_style_payloads_to_the_wire_format() {
    let server = MockServer::sequence(vec![Response::sse(&sse(&[terminal("stop")])).with_header("x-request-id", "request-1")]).await;
    let model = model_at(&server.url, "mistral-large-latest");
    let context = normalize_context(&Context {
        system_prompt: Some("Be precise".into()),
        messages: vec![Message::User(UserMessage::new(
            vec![UserContentBlock::text("describe"), UserContentBlock::Image(ImageContent { data: "aGVsbG8=".into(), mime_type: "image/png".into() })],
            1,
        ))],
        tools: Some(vec![Tool::new("lookup", "Look something up", json!({ "type": "object", "properties": { "query": { "type": "string" } }, "required": ["query"] }))]),
    });
    let captured = Arc::new(Mutex::new(None::<Value>));
    let sink = captured.clone();
    let response = Arc::new(Mutex::new(None::<ProviderResponse>));
    let response_sink = response.clone();
    let mut headers = ProviderHeaders::new();
    headers.insert("x-custom".into(), Some("value".into()));
    let options = StreamOptions {
        api_key: Some("secret".into()),
        headers: Some(headers),
        max_tokens: Some(123),
        session_id: Some("session-1".into()),
        on_payload: Some(Arc::new(move |payload: Value, _: Model| {
            *sink.lock() = Some(payload.clone());
            let mut next = payload.as_object().cloned().unwrap();
            next.insert("topP".into(), json!(0.9));
            next.insert("randomSeed".into(), json!(42));
            next.insert(
                "responseFormat".into(),
                json!({ "type": "json_schema", "jsonSchema": { "name": "result", "schemaDefinition": { "type": "object", "properties": { "maxTokens": { "type": "number" } } } } }),
            );
            next.insert("presencePenalty".into(), json!(0.1));
            next.insert("frequencyPenalty".into(), json!(0.2));
            next.insert("parallelToolCalls".into(), json!(true));
            next.insert("safePrompt".into(), json!(true));
            Box::pin(async move { Some(Value::Object(next)) })
        })),
        on_response: Some(Arc::new(move |r: ProviderResponse, _: Model| {
            *response_sink.lock() = Some(r);
            Box::pin(async {})
        })),
        ..Default::default()
    }
    .with_extra("promptMode", "reasoning")
    .with_extra("reasoningEffort", "high")
    .with_extra("toolChoice", json!({ "type": "function", "function": { "name": "lookup" } }));

    let message = mistral_conversations::stream(&model, &context, options).result().await;
    assert_eq!(message.stop_reason, StopReason::Stop, "{:?}", message.error_message);
    let request = server.request(0);
    assert_eq!(request.path, "/v1/chat/completions");
    assert_eq!(request.header("authorization"), Some("Bearer secret"));
    assert_eq!(request.header("accept"), Some("text/event-stream"));
    assert_eq!(request.header("x-affinity"), Some("session-1"));
    assert_eq!(request.header("x-custom"), Some("value"));
    assert_eq!(request.header("user-agent"), Some(format_user_agent(None).as_str()));
    let payload = captured.lock().clone().unwrap();
    assert_eq!(payload["maxTokens"], 123);
    assert_eq!(payload["promptMode"], "reasoning");
    assert_eq!(payload["promptCacheKey"], "session-1");
    let response = response.lock().clone().unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.headers.get("x-request-id").map(String::as_str), Some("request-1"));

    let wire = request.json();
    assert_eq!(wire["max_tokens"], 123);
    assert_eq!(wire["prompt_mode"], "reasoning");
    assert_eq!(wire["reasoning_effort"], "high");
    assert_eq!(wire["tool_choice"], json!({ "type": "function", "function": { "name": "lookup" } }));
    assert_eq!(wire["prompt_cache_key"], "session-1");
    assert_eq!(wire["top_p"], 0.9);
    assert_eq!(wire["random_seed"], 42);
    assert_eq!(wire["presence_penalty"], 0.1);
    assert_eq!(wire["frequency_penalty"], 0.2);
    assert_eq!(wire["parallel_tool_calls"], true);
    assert_eq!(wire["safe_prompt"], true);
    assert_eq!(
        wire["response_format"],
        json!({ "type": "json_schema", "json_schema": { "name": "result", "schema": { "type": "object", "properties": { "maxTokens": { "type": "number" } } } } })
    );
    for key in ["maxTokens", "promptMode", "promptCacheKey"] {
        assert!(wire.get(key).is_none(), "{key}");
    }
    assert_eq!(
        wire["messages"],
        json!([
            { "role": "system", "content": "Be precise" },
            { "role": "user", "content": [{ "type": "text", "text": "describe" }, { "type": "image_url", "image_url": "data:image/png;base64,aGVsbG8=" }] }
        ])
    );
}

#[tokio::test]
async fn serializes_assistant_thinking_tool_calls_and_results_for_replay() {
    let server = MockServer::sequence(vec![Response::sse(&sse(&[terminal("stop")]))]).await;
    let model = model_at(&server.url, "mistral-large-latest");
    let context = normalize_context(&Context {
        system_prompt: None,
        tools: None,
        messages: vec![
            Message::Assistant(AssistantMessage {
                api: "mistral-conversations".into(),
                provider: "mistral".into(),
                model: model.id.clone(),
                content: vec![
                    AssistantContent::Thinking(ThinkingContent::new("reason")),
                    AssistantContent::Text(TextContent::new("answer")),
                    AssistantContent::ToolCall(ToolCall {
                        id: "abc123456".into(),
                        name: "lookup".into(),
                        arguments: json!({ "query": "pi" }).as_object().cloned().unwrap(),
                        ..Default::default()
                    }),
                ].into(),
                stop_reason: StopReason::ToolUse,
                timestamp: 1,
                ..Default::default()
            }),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: "abc123456".into(),
                tool_name: "lookup".into(),
                content: vec![UserContentBlock::text("found"), UserContentBlock::Image(ImageContent { data: "aGVsbG8=".into(), mime_type: "image/png".into() })],
                timestamp: 2,
                ..Default::default()
            }),
        ],
    });
    let message = mistral_conversations::stream(&model, &context, key("test")).result().await;
    assert_eq!(message.stop_reason, StopReason::Stop, "{:?}", message.error_message);
    assert_eq!(
        server.request(0).json()["messages"],
        json!([
            {
                "role": "assistant", "prefix": false,
                "content": [{ "type": "thinking", "thinking": [{ "type": "text", "text": "reason" }] }, { "type": "text", "text": "answer" }],
                "tool_calls": [{ "id": "abc123456", "type": "function", "function": { "name": "lookup", "arguments": "{\"query\":\"pi\"}" }, "index": 0 }]
            },
            {
                "role": "tool", "tool_call_id": "abc123456", "name": "lookup",
                "content": [{ "type": "text", "text": "found" }, { "type": "image_url", "image_url": "data:image/png;base64,aGVsbG8=" }]
            }
        ])
    );
}

#[tokio::test]
async fn parses_thinking_text_fragmented_tool_calls_and_cached_usage() {
    let event = |delta: Value, reason: Value| json!({ "id": "response-1", "model": "mistral-large-latest", "choices": [{ "index": 0, "finish_reason": reason, "delta": delta }] });
    let mut last = event(json!({ "tool_calls": [{ "index": 0, "function": { "name": "", "arguments": "\"pi\"}" } }] }), json!("tool_calls"));
    last["usage"] = json!({ "prompt_tokens": 10, "completion_tokens": 4, "total_tokens": 14, "prompt_tokens_details": { "cached_tokens": 3 } });
    let events = vec![
        event(json!({ "content": [{ "type": "thinking", "thinking": [{ "type": "text", "text": "reason" }] }] }), Value::Null),
        event(json!({ "content": [{ "type": "text", "text": "answer" }] }), Value::Null),
        event(json!({ "tool_calls": [{ "id": "abc123456", "index": 0, "function": { "name": "lookup", "arguments": "{\"query\":" } }] }), Value::Null),
        last,
    ];
    let server = MockServer::sequence(vec![Response::sse(&sse(&events))]).await;
    let model = model_at(&server.url, "mistral-large-latest");
    let message = mistral_conversations::stream(&model, &hello(), key("test")).result().await;
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("tool_calls"));
    assert_eq!(message.response_id.as_deref(), Some("response-1"));
    assert_eq!(
        serde_json::to_value(&message.content).unwrap(),
        json!([
            { "type": "thinking", "thinking": "reason" },
            { "type": "text", "text": "answer" },
            { "type": "toolCall", "id": "abc123456", "name": "lookup", "arguments": { "query": "pi" } }
        ])
    );
    assert_eq!((message.usage.input, message.usage.output, message.usage.cache_read, message.usage.cache_write, message.usage.total_tokens), (7, 4, 3, 0, 14));
}

#[tokio::test]
async fn ignores_empty_content_deltas() {
    let thinking = |text: &str| json!({ "type": "thinking", "thinking": [{ "type": "text", "text": text }] });
    let call = |args: &str, first: bool| {
        let mut call = json!({ "index": 0, "function": { "name": if first { "read" } else { "" }, "arguments": args } });
        if first {
            call["id"] = json!("abc123456");
        }
        call
    };
    let deltas = vec![
        json!({ "content": "" }),
        json!({ "content": [thinking("first part,")] }),
        json!({ "content": "" }),
        json!({ "content": [{ "type": "text", "text": "" }] }),
        json!({ "content": [thinking(" second part."), { "type": "text", "text": "Reading." }] }),
        json!({ "content": "", "tool_calls": [call("", true)] }),
        json!({ "content": "", "tool_calls": [call("{\"path\":", false)] }),
        json!({ "content": "", "tool_calls": [call("\"a.txt\"}", false)] }),
        json!({ "content": "" }),
    ];
    let count = deltas.len();
    let events: Vec<Value> = deltas
        .into_iter()
        .enumerate()
        .map(|(i, delta)| json!({ "id": "response-1", "choices": [{ "index": 0, "finish_reason": if i == count - 1 { json!("tool_calls") } else { Value::Null }, "delta": delta }] }))
        .collect();
    let server = MockServer::sequence(vec![Response::sse(&sse(&events))]).await;
    let model = model_at(&server.url, "zai-glm-5-3");
    let message = mistral_conversations::stream(&model, &hello(), key("test")).result().await;
    assert_eq!(
        serde_json::to_value(&message.content).unwrap(),
        json!([
            { "type": "thinking", "thinking": "first part, second part." },
            { "type": "text", "text": "Reading." },
            { "type": "toolCall", "id": "abc123456", "name": "read", "arguments": { "path": "a.txt" } }
        ])
    );
}

#[tokio::test]
async fn forwards_parsed_events_and_handles_bytewise_utf8() {
    let events = vec![
        json!({ "id": "response-1", "provider_metadata": { "request": "test" }, "choices": [{ "index": 0, "finish_reason": null, "delta": { "content": "hello" } }] }),
        json!({ "id": "response-1", "choices": [{ "index": 0, "finish_reason": "stop", "delta": {} }], "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 } }),
    ];
    let server = MockServer::sequence(vec![Response::sse(&sse(&events))]).await;
    let model = model_at(&server.url, "mistral-large-latest");
    let received = Arc::new(Mutex::new(Vec::<Value>::new()));
    let sink = received.clone();
    let options = StreamOptions {
        api_key: Some("test".into()),
        on_provider_stream_event: Some(Arc::new(move |event: Value, _: Model| { sink.lock().push(event); Box::pin(async {}) })),
        ..Default::default()
    };
    let result = mistral_conversations::stream(&model, &hello(), options).result().await;
    assert_eq!(*received.lock(), events);
    assert_eq!(result.stop_reason, StopReason::Stop);
    assert_eq!(result.text(), "hello");

    let body = format!(
        "data: {}\r\n\r\ndata: [DONE]\r\n\r\n",
        json!({ "id": "response-bytewise", "choices": [{ "index": 0, "finish_reason": "stop", "delta": { "content": "héllo 🌍" } }], "usage": { "prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3 } })
    );
    let chunks: Vec<Vec<u8>> = body.as_bytes().iter().map(|b| vec![*b]).collect();
    let server = MockServer::sequence(vec![Response::chunked(200, "text/event-stream", chunks)]).await;
    let model = model_at(&server.url, "mistral-large-latest");
    let message = mistral_conversations::stream(&model, &hello(), key("test")).result().await;
    assert_eq!(message.stop_reason, StopReason::Stop, "{:?}", message.error_message);
    assert_eq!(message.text(), "héllo 🌍");
}

#[tokio::test]
async fn header_overrides_and_affinity_suppression() {
    let server = MockServer::sequence(vec![Response::sse(&sse(&[terminal("stop")]))]).await;
    let mut model = model_at(&server.url, "mistral-large-latest");
    model.headers = Some([("Authorization".to_string(), "Bearer model-key".to_string()), ("X-Affinity".to_string(), "model-affinity".to_string())].into_iter().collect());
    let mut headers = ProviderHeaders::new();
    headers.insert("authorization".into(), None);
    headers.insert("x-affinity".into(), None);
    headers.insert("User-Agent".into(), Some("custom-agent".into()));
    let options = StreamOptions { api_key: Some("request-key".into()), session_id: Some("automatic-affinity".into()), headers: Some(headers), ..Default::default() };
    mistral_conversations::stream(&model, &hello(), options).result().await;
    let request = server.request(0);
    assert!(request.header("authorization").is_none());
    assert!(request.header("x-affinity").is_none());
    assert_eq!(request.header("user-agent"), Some("custom-agent"));
}

#[tokio::test]
async fn aborts_while_waiting_for_an_sse_chunk() {
    let server = MockServer::sequence(vec![Response::chunked(200, "text/event-stream", vec![b": ping\n".to_vec(), b"\n".to_vec()]).with_chunk_delay(2000)]).await;
    let model = model_at(&server.url, "mistral-large-latest");
    let signal = tokio_util::sync::CancellationToken::new();
    let options = StreamOptions { api_key: Some("test".into()), signal: Some(signal.clone()), ..Default::default() };
    let stream = mistral_conversations::stream(&model, &hello(), options);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    signal.cancel();
    let message = stream.result().await;
    assert_eq!(message.stop_reason, StopReason::Aborted);
}

#[tokio::test]
async fn header_timeout_applies_only_to_response_headers() {
    let server = MockServer::sequence(vec![Response::sse(&sse(&[terminal("stop")])).with_delay(500)]).await;
    let model = model_at(&server.url, "mistral-large-latest");
    let options = StreamOptions { api_key: Some("test".into()), timeout_ms: Some(5), ..Default::default() };
    let message = mistral_conversations::stream(&model, &hello(), options).result().await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.error_message.as_deref(), Some("Mistral response headers timed out after 5ms"));

    let thinking = json!({ "choices": [{ "index": 0, "delta": { "content": [{ "type": "thinking", "thinking": [{ "type": "text", "text": "x" }] }] } }] });
    let mut chunks: Vec<Vec<u8>> = (0..5).map(|_| format!("data: {thinking}\n\n").into_bytes()).collect();
    chunks.push(format!("data: {}\n\ndata: [DONE]\n\n", terminal("stop")).into_bytes());
    let server = MockServer::sequence(vec![Response::chunked(200, "text/event-stream", chunks).with_chunk_delay(15)]).await;
    let model = model_at(&server.url, "mistral-large-latest");
    let options = StreamOptions { api_key: Some("test".into()), timeout_ms: Some(20), ..Default::default() };
    let message = mistral_conversations::stream(&model, &hello(), options).result().await;
    assert_eq!(message.stop_reason, StopReason::Stop, "{:?}", message.error_message);
    assert_eq!(serde_json::to_value(&message.content).unwrap(), json!([{ "type": "thinking", "thinking": "xxxxx" }]));
}

#[tokio::test]
async fn preserves_http_status_and_bodies_in_errors() {
    let server = MockServer::sequence(vec![Response::new(403, "application/json", "{\"message\":\"blocked by gateway\"}")]).await;
    let model = model_at(&server.url, "mistral-large-latest");
    let message = mistral_conversations::stream(&model, &hello(), key("test")).result().await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.error_message.as_deref(), Some(r#"Mistral API error (403): {"message":"blocked by gateway"}"#));
}

#[tokio::test]
async fn raw_stop_reasons() {
    for (reason, stop, error, retryable) in [
        ("stop", StopReason::Stop, None, false),
        ("error", StopReason::Error, Some("Provider stopped with: error (server error)"), true),
        ("unmapped_error", StopReason::Error, Some("Provider stopped with: unmapped_error"), false),
    ] {
        let server = MockServer::sequence(vec![Response::sse(&format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({ "id": "x", "choices": [{ "index": 0, "finish_reason": reason, "delta": {} }], "usage": { "prompt_tokens": 1, "completion_tokens": 0, "total_tokens": 1 } })
        ))])
        .await;
        let model = model_at(&server.url, "devstral-medium-latest");
        let message = mistral_conversations::stream(&model, &hello(), key("test")).result().await;
        assert_eq!(message.stop_reason, stop);
        assert_eq!(message.raw_stop_reason.as_deref(), Some(reason));
        assert_eq!(message.error_message.as_deref(), error);
        if stop == StopReason::Error {
            assert_eq!(is_retryable_assistant_error(&message), retryable, "{reason}");
        }
    }
}

// ---------------------------------------------------------------------------
// Reasoning mode selection (mistral-reasoning-mode.test.ts)
// ---------------------------------------------------------------------------

fn level_map(entries: &[(ModelThinkingLevel, Option<&str>)]) -> ThinkingLevelMap {
    entries.iter().map(|(k, v)| (*k, v.map(str::to_string))).collect()
}

fn none_high() -> ThinkingLevelMap {
    use ModelThinkingLevel as L;
    level_map(&[(L::Off, Some("none")), (L::Minimal, None), (L::Low, None), (L::Medium, None), (L::High, Some("high")), (L::Xhigh, None), (L::Max, None)])
}

fn reasoning_model(id: &str, reasoning: bool, map: Option<ThinkingLevelMap>) -> Model {
    Model {
        id: id.into(),
        name: id.into(),
        api: "mistral-conversations".into(),
        provider: "mistral".into(),
        base_url: "http://127.0.0.1:9".into(),
        reasoning,
        thinking_level_map: map,
        input: vec![ModelInputModality::Text],
        context_window: 128000,
        max_tokens: 16384,
        ..Default::default()
    }
}

async fn capture(model: &Model, mut options: StreamOptions) -> Value {
    let captured = Arc::new(Mutex::new(None::<Value>));
    let sink = captured.clone();
    options.api_key = Some("fake-key".into());
    options.on_payload = Some(Arc::new(move |payload: Value, _: Model| {
        *sink.lock() = Some(payload);
        Box::pin(async { None })
    }));
    mistral_conversations::stream_simple(model, &hello(), options).result().await;
    captured.lock().clone().expect("payload captured")
}

fn reasoning(level: ThinkingLevel) -> StreamOptions {
    StreamOptions { reasoning: Some(level), ..Default::default() }
}

#[tokio::test]
async fn reasoning_mode_selection() {
    use ModelThinkingLevel as L;
    let payload = capture(&reasoning_model("magistral-medium-latest", true, None), reasoning(ThinkingLevel::Medium)).await;
    assert_eq!(payload["promptMode"], "reasoning");
    assert!(payload.get("reasoningEffort").is_none());
    let payload = capture(&reasoning_model("magistral-medium-latest", true, None), StreamOptions::default()).await;
    assert!(payload.get("promptMode").is_none() && payload.get("reasoningEffort").is_none());

    let mut glm_5_2 = none_high();
    glm_5_2.insert(L::Max, Some("max".into()));
    for (id, map) in [("mistral-small-2603", none_high()), ("mistral-medium-latest", none_high()), ("zai-glm-5-2", glm_5_2.clone())] {
        let model = reasoning_model(id, true, Some(map));
        let payload = capture(&model, reasoning(ThinkingLevel::High)).await;
        assert_eq!(payload["reasoningEffort"], "high", "{id}");
        assert!(payload.get("promptMode").is_none());
        assert_eq!(capture(&model, reasoning(ThinkingLevel::Low)).await["reasoningEffort"], "high", "{id}");
        let payload = capture(&model, StreamOptions::default()).await;
        assert_eq!(payload["reasoningEffort"], "none", "{id}");
        assert!(payload.get("promptMode").is_none());
    }
    assert_eq!(capture(&reasoning_model("zai-glm-5-2", true, Some(glm_5_2)), reasoning(ThinkingLevel::Max)).await["reasoningEffort"], "max");

    let mut glm_5_3 = none_high();
    glm_5_3.insert(L::Off, None);
    glm_5_3.insert(L::Low, Some("low".into()));
    glm_5_3.insert(L::Max, Some("max".into()));
    let model = reasoning_model("zai-glm-5-3", true, Some(glm_5_3));
    for (level, expected) in [(ThinkingLevel::Low, "low"), (ThinkingLevel::High, "high"), (ThinkingLevel::Max, "max"), (ThinkingLevel::Medium, "high")] {
        let payload = capture(&model, reasoning(level)).await;
        assert_eq!(payload["reasoningEffort"], expected);
        assert!(payload.get("promptMode").is_none());
    }

    let payload = capture(&reasoning_model("mistral-medium-2505", false, None), reasoning(ThinkingLevel::Medium)).await;
    assert!(payload.get("promptMode").is_none() && payload.get("reasoningEffort").is_none());

    let options = StreamOptions { session_id: Some("session-123".into()), ..Default::default() };
    assert_eq!(capture(&reasoning_model("mistral-large-latest", false, None), options).await["promptCacheKey"], "session-123");
    let options = StreamOptions { session_id: Some("session-123".into()), cache_retention: Some(CacheRetention::None), ..Default::default() };
    assert!(capture(&reasoning_model("mistral-large-latest", false, None), options).await.get("promptCacheKey").is_none());
}

#[tokio::test]
async fn strict_tool_schemas() {
    let model = reasoning_model("devstral-medium-latest", false, None);
    let mut tool = Tool::new(
        "inspect_schema",
        "Inspect the schema",
        json!({ "type": "object", "properties": { "nested": { "type": "object", "properties": { "value": { "type": "string" } }, "required": ["value"] } }, "required": ["nested"] }),
    );
    tool.constrained_sampling = Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::JsonSchema { strict: JsonSchemaStrictness::Require }));
    let context = normalize_context(&Context { system_prompt: None, tools: Some(vec![tool]), messages: vec![Message::User(UserMessage::new("Hi", 1))] });
    let captured = Arc::new(Mutex::new(None::<Value>));
    let sink = captured.clone();
    let options = StreamOptions {
        api_key: Some("fake-key".into()),
        on_payload: Some(Arc::new(move |payload: Value, _: Model| {
            *sink.lock() = Some(payload);
            Box::pin(async { None })
        })),
        ..Default::default()
    };
    let response = mistral_conversations::stream(&model, &context, options).result().await;
    let payload = captured.lock().clone().unwrap();
    assert_eq!(payload["tools"].as_array().unwrap().len(), 1);
    assert_eq!(payload["tools"][0]["function"]["strict"], true);
    assert!(payload["tools"][0]["function"]["parameters"]["properties"]["nested"].is_object());
    assert_eq!(response.stop_reason, StopReason::Error);
}
