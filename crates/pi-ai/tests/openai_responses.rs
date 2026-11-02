//! Offline tests for the OpenAI Responses APIs (ports of `openai-responses-*.test.ts`,
//! `azure-openai-*.test.ts` and `xai-responses.test.ts`).

mod responses_mock;

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use pi_ai::api::openai_responses_shared::{
    ConvertResponsesMessagesOptions, OpenAIResponsesStreamOptions, convert_responses_messages,
    process_responses_stream,
};
use pi_ai::api::{azure_openai_responses, openai_responses};
use pi_ai::providers::all::get_builtin_model;
use pi_ai::utils::hash::short_hash;
use pi_ai::*;
use responses_mock::*;
use serde_json::{Value, json};

const OPENAI_PROVIDERS: [&str; 3] = ["openai", "openai-codex", "opencode"];

fn base_model() -> Model {
    serde_json::from_value(json!({
        "id": "gpt-5-mini", "name": "GPT-5 Mini", "api": "openai-responses", "provider": "openai",
        "baseUrl": "https://api.openai.com/v1", "reasoning": true, "input": ["text"],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 400000, "maxTokens": 128000
    }))
    .unwrap()
}

fn output_for(model: &Model) -> AssistantMessage {
    AssistantMessage {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        stop_reason: StopReason::Pending,
        timestamp: 1,
        ..Default::default()
    }
}

fn user(text: &str) -> Message {
    Message::User(UserMessage::new(text, 0))
}

fn ctx(
    system: Option<&str>,
    messages: Vec<Message>,
    tools: Option<Vec<Tool>>,
) -> TranscriptContext {
    normalize_context(&Context {
        system_prompt: system.map(str::to_string),
        messages,
        tools,
    })
}

async fn run_events(
    model: &Model,
    events: Vec<Value>,
    options: OpenAIResponsesStreamOptions<'_>,
) -> (
    anyhow::Result<()>,
    AssistantMessage,
    Vec<AssistantMessageEvent>,
) {
    let mut output = output_for(model);
    let (sender, mut stream) = AssistantMessageEventStream::new();
    let result = process_responses_stream(
        futures::stream::iter(events.into_iter().map(Ok)),
        &mut output,
        &sender,
        model,
        options,
    )
    .await;
    sender.end(None);
    let mut pushed = Vec::new();
    while let Some(event) = stream.next_event().await {
        pushed.push(event);
    }
    (result, output, pushed)
}

fn function_call_events(arguments: &str) -> Vec<Value> {
    vec![
        json!({ "type": "response.output_item.added", "item": { "type": "function_call", "id": "fc_test", "call_id": "call_test", "name": "edit", "arguments": "" } }),
        json!({ "type": "response.function_call_arguments.delta", "delta": "{\"path\":\"README.md\"" }),
        json!({ "type": "response.function_call_arguments.delta", "delta": ",\"content\":\"updated\"}" }),
        json!({ "type": "response.function_call_arguments.done", "arguments": arguments }),
        json!({ "type": "response.output_item.done", "item": { "type": "function_call", "id": "fc_test", "call_id": "call_test", "name": "edit", "arguments": arguments } }),
        json!({ "type": "response.completed", "sequence_number": 5, "response": { "id": "resp_test", "status": "completed" } }),
    ]
}

fn tool_call_block(output: &AssistantMessage, index: usize) -> ToolCall {
    output.content[index]
        .as_tool_call()
        .cloned()
        .expect("tool call block")
}

// ---------------------------------------------------------------------------
// convertResponsesMessages
// ---------------------------------------------------------------------------

#[test]
fn empty_tool_results_use_placeholder() {
    let model = get_builtin_model("openai", "gpt-4o-mini").unwrap();
    for text in ["", "\r\n", " \n\t"] {
        let assistant = AssistantMessage {
            content: vec![AssistantContent::ToolCall(ToolCall {
                id: "tool-1".into(),
                name: "bash".into(),
                arguments: json!({ "command": "true" }).as_object().cloned().unwrap(),
                ..Default::default()
            })].into(),
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            stop_reason: StopReason::ToolUse,
            timestamp: 2,
            ..Default::default()
        };
        let result = ToolResultMessage {
            tool_call_id: "tool-1".into(),
            tool_name: "bash".into(),
            content: vec![UserContentBlock::text(text)],
            timestamp: 3,
            ..Default::default()
        };
        let context = ctx(
            None,
            vec![user("Run the command"), assistant.into(), result.into()],
            None,
        );
        let input =
            convert_responses_messages(&model, &context, &OPENAI_PROVIDERS, &Default::default())
                .unwrap();
        let output = input
            .iter()
            .find(|i| i["type"] == "function_call_output")
            .unwrap();
        assert_eq!(output["output"], "(no tool output)");
    }
}

#[test]
fn foreign_copilot_item_ids_are_hashed() {
    let raw = "call_4VnzVawQXPB9MgYib7CiQFEY|I9b95oN1wD/cHXKTw3PpRkL6KkCtzTJhUxMouMWYwHeTo2j3htzfSk7YPx2vifiIM4g3A8XXyOj8q4Bt6SLUG7gqY1E3ELkrkVQNHglRfUmWj84lqxJY+Puieb3VKyX0FB+83TUzn91cDMF/4gzt990IzqVrc+nIb9RRscRD070Du16q1glydVjWR0SBJsE6TbY/esOjFpqplogQqrajm1eI++f3eLi73R6q7hVusY0QbeFySVxABCjhN0lXB04caBe1rzHjYzul6MAXj7uq+0r17VLq+yrtyYhN12wkmFqHeqTyEei6EFPbMy24Nc+IbJlkP0OCg02W+gOnyBFcbi2ctvJFSOhSjt1CqBdqCnnhwUqXjbWiT0wh3DmLScRgTHmGkaI+oAcQQjfic65nxj+TnEkReA==";
    let model = get_builtin_model("openai-codex", "gpt-6.1-sol").unwrap();
    let assistant = AssistantMessage {
        content: vec![AssistantContent::ToolCall(ToolCall {
            id: raw.into(),
            name: "edit".into(),
            arguments: json!({ "path": "src/styles/app.css" })
                .as_object()
                .cloned()
                .unwrap(),
            ..Default::default()
        })].into(),
        api: "openai-responses".into(),
        provider: "github-copilot".into(),
        model: "gpt-5.5".into(),
        stop_reason: StopReason::ToolUse,
        timestamp: 2,
        ..Default::default()
    };
    let result = ToolResultMessage {
        tool_call_id: raw.into(),
        tool_name: "edit".into(),
        content: vec![UserContentBlock::text("ok")],
        timestamp: 3,
        ..Default::default()
    };
    let context = ctx(
        Some("You are concise."),
        vec![user("Use the tool."), assistant.into(), result.into()],
        None,
    );
    let input =
        convert_responses_messages(&model, &context, &OPENAI_PROVIDERS, &Default::default())
            .unwrap();
    let call = input.iter().find(|i| i["type"] == "function_call").unwrap();
    let expected = format!("fc_{}", short_hash(raw.split('|').nth(1).unwrap()));
    assert_eq!(call["id"], json!(expected));
    assert!(expected.len() <= 64);
}

#[test]
fn unique_fallback_message_ids() {
    let model = get_builtin_model("openai-codex", "gpt-6.1-sol").unwrap();
    let assistant = AssistantMessage {
        content: vec![
            AssistantContent::Text(TextContent::new("first answer")),
            AssistantContent::Text(TextContent::new("visible answer")),
        ].into(),
        api: "anthropic-messages".into(),
        provider: "anthropic".into(),
        model: "claude-opus-4-8".into(),
        stop_reason: StopReason::Stop,
        timestamp: 2,
        ..Default::default()
    };
    let context = ctx(
        Some("You are concise."),
        vec![user("hello"), assistant.into()],
        None,
    );
    let input =
        convert_responses_messages(&model, &context, &OPENAI_PROVIDERS, &Default::default())
            .unwrap();
    let ids: Vec<&str> = input
        .iter()
        .filter(|i| i["type"] == "message")
        .filter_map(|i| i["id"].as_str())
        .collect();
    assert_eq!(ids, vec!["msg_pi_1", "msg_pi_1_1"]);
}

// ---------------------------------------------------------------------------
// processResponsesStream
// ---------------------------------------------------------------------------

#[tokio::test]
async fn partial_json_is_not_persisted_and_arguments_parse() {
    let model = base_model();
    let args = r#"{"path":"README.md","content":"updated"}"#;
    let (result, output, events) =
        run_events(&model, function_call_events(args), Default::default()).await;
    result.unwrap();
    assert_eq!(output.content.len(), 1);
    let call = tool_call_block(&output, 0);
    assert_eq!(
        Value::Object(call.arguments.clone()),
        json!({ "path": "README.md", "content": "updated" })
    );
    let end = events.iter().find_map(|e| match e {
        AssistantMessageEvent::ToolCallEnd { tool_call, .. } => Some(tool_call.clone()),
        _ => None,
    });
    assert_eq!(end.unwrap(), call);
    assert_eq!(output.stop_reason, StopReason::ToolUse);
}

#[tokio::test]
async fn malformed_final_arguments_are_marked() {
    let model = base_model();
    let args = r#"{"path":"README.md","content":"updated","mode":.5}"#;
    let (_, output, _) = run_events(&model, function_call_events(args), Default::default()).await;
    let call = tool_call_block(&output, 0);
    assert!(call.arguments.is_empty());
    assert!(
        call.arguments_error
            .unwrap()
            .ends_with(&format!("Received arguments:\n{args}"))
    );
}

fn early_eof_events() -> Vec<Value> {
    vec![
        json!({ "type": "response.created", "sequence_number": 0, "response": { "id": "resp_early_eof" } }),
        json!({ "type": "response.output_item.added", "output_index": 0, "item": { "type": "reasoning", "id": "rs_early_eof", "summary": [] } }),
        json!({ "type": "response.reasoning_text.delta", "output_index": 0, "content_index": 0, "item_id": "rs_early_eof", "delta": "partial reasoning before the stream ends" }),
    ]
}

#[tokio::test]
async fn rejects_streams_without_terminal_event() {
    let model = base_model();
    let (result, _, _) = run_events(&model, early_eof_events(), Default::default()).await;
    assert_eq!(
        result.unwrap_err().to_string(),
        "OpenAI Responses stream ended before a terminal response event"
    );
}

#[tokio::test]
async fn rejects_unfinished_tool_calls() {
    let model = base_model();
    let events = vec![
        json!({ "type": "response.output_item.added", "output_index": 0, "item": { "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "bash", "arguments": "" } }),
        json!({ "type": "response.function_call_arguments.delta", "output_index": 0, "item_id": "fc_1", "delta": "{\"command\":\"rm -rf /tmp/build" }),
        json!({ "type": "response.completed", "response": { "id": "resp_unfinished", "status": "completed" } }),
    ];
    let (result, _, _) = run_events(&model, events, Default::default()).await;
    assert_eq!(
        result.unwrap_err().to_string(),
        "OpenAI Responses stream completed with an unfinished tool call: bash (call_1|fc_1)"
    );
}

#[tokio::test]
async fn rejects_parallel_tool_calls_without_output_index() {
    let model = base_model();
    let call = |n: &str, args: &str| json!({ "type": "function_call", "id": format!("fc_{n}"), "call_id": format!("call_{n}"), "name": "bash", "arguments": args });
    let events = vec![
        json!({ "type": "response.output_item.added", "item": call("a", "") }),
        json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_a", "delta": "{\"command\":\"echo a\"}" }),
        json!({ "type": "response.output_item.added", "item": call("b", "") }),
        json!({ "type": "response.function_call_arguments.delta", "item_id": "fc_b", "delta": "{\"command\":\"echo b\"}" }),
        json!({ "type": "response.output_item.done", "item": call("a", "{\"command\":\"echo a\"}") }),
        json!({ "type": "response.output_item.done", "item": call("b", "{\"command\":\"echo b\"}") }),
        json!({ "type": "response.completed", "response": { "id": "resp_no_output_index", "status": "completed" } }),
    ];
    let (result, _, _) = run_events(&model, events, Default::default()).await;
    assert_eq!(
        result.unwrap_err().to_string(),
        "OpenAI Responses stream completed with an unfinished tool call: bash (call_a|fc_a)"
    );
}

fn phased_events(phases: [&str; 2], incomplete: bool) -> Vec<Value> {
    let mut events = vec![
        json!({ "type": "response.output_item.added", "output_index": 0, "item": { "type": "message", "id": "msg_phase", "role": "assistant", "status": "in_progress", "content": [], "phase": phases[0] } }),
        json!({ "type": "response.output_item.done", "output_index": 0, "item": { "type": "message", "id": "msg_phase", "role": "assistant", "status": "completed", "content": [{ "type": "output_text", "text": "answer", "annotations": [] }], "phase": phases[1] } }),
    ];
    if incomplete {
        events.push(json!({ "type": "response.incomplete", "response": { "id": "resp_phase", "status": "incomplete", "incomplete_details": { "reason": "max_output_tokens" } } }));
    } else {
        events.push(json!({ "type": "response.completed", "response": { "id": "resp_phase", "status": "completed" } }));
    }
    events
}

#[tokio::test]
async fn tracks_message_phases() {
    let model = base_model();
    for (phases, expected) in [
        (
            ["commentary", "commentary"],
            [StopReason::Pending, StopReason::Pending],
        ),
        (
            ["final_answer", "final_answer"],
            [StopReason::Stop, StopReason::Stop],
        ),
        (
            ["commentary", "final_answer"],
            [StopReason::Pending, StopReason::Stop],
        ),
    ] {
        let (result, output, events) =
            run_events(&model, phased_events(phases, false), Default::default()).await;
        result.unwrap();
        let observed: Vec<StopReason> = events
            .iter()
            .filter(|e| !e.is_terminal())
            .map(|e| e.message().stop_reason)
            .collect();
        assert_eq!(observed, expected.to_vec());
        assert_eq!(output.stop_reason, StopReason::Stop);
        let signature = output.content[0]
            .as_text()
            .unwrap()
            .text_signature
            .clone()
            .unwrap();
        assert_eq!(
            signature,
            json!({ "v": 1, "id": "msg_phase", "phase": phases[1] }).to_string()
        );
    }
    let (_, output, _) = run_events(
        &model,
        phased_events(["final_answer", "final_answer"], true),
        Default::default(),
    )
    .await;
    assert_eq!(output.stop_reason, StopReason::Length);
}

#[tokio::test]
async fn finalizes_terminal_events() {
    let model = base_model();
    let completed = vec![
        json!({ "type": "response.completed", "response": { "id": "resp_completed", "status": "completed", "usage": { "input_tokens": 20, "output_tokens": 7, "total_tokens": 27, "input_tokens_details": { "cached_tokens": 2, "cache_write_tokens": 3 } } } }),
    ];
    let (result, output, _) = run_events(&model, completed, Default::default()).await;
    result.unwrap();
    assert_eq!(output.response_id.as_deref(), Some("resp_completed"));
    assert_eq!(output.stop_reason, StopReason::Stop);
    assert_eq!(output.raw_stop_reason.as_deref(), Some("completed"));
    assert_eq!(
        (
            output.usage.input,
            output.usage.output,
            output.usage.cache_read,
            output.usage.cache_write,
            output.usage.total_tokens
        ),
        (15, 7, 2, 3, 27)
    );
    assert!(output.error_message.is_none());

    let incomplete = |reason: &str| {
        vec![
            json!({ "type": "response.incomplete", "response": { "id": "resp_incomplete", "status": "incomplete", "incomplete_details": { "reason": reason }, "usage": { "input_tokens": 30, "output_tokens": 12, "total_tokens": 42, "input_tokens_details": { "cached_tokens": 5 } } } }),
        ]
    };
    let (_, output, _) =
        run_events(&model, incomplete("max_output_tokens"), Default::default()).await;
    assert_eq!(output.stop_reason, StopReason::Length);
    assert_eq!(
        output.raw_stop_reason.as_deref(),
        Some("incomplete.max_output_tokens")
    );
    assert_eq!(
        (
            output.usage.input,
            output.usage.cache_read,
            output.usage.total_tokens
        ),
        (25, 5, 42)
    );
    for reason in ["content_filter", "max_time_limit"] {
        let (_, output, _) = run_events(&model, incomplete(reason), Default::default()).await;
        assert_eq!(output.stop_reason, StopReason::Error);
        assert_eq!(output.raw_stop_reason, Some(format!("incomplete.{reason}")));
        assert_eq!(
            output.error_message,
            Some(format!("Response incomplete: {reason}"))
        );
    }

    let failed = vec![
        json!({ "type": "response.failed", "response": { "id": "resp_failed", "status": "failed", "error": { "code": "server_error", "message": "boom" } } }),
    ];
    let (result, output, _) = run_events(&model, failed, Default::default()).await;
    assert_eq!(result.unwrap_err().to_string(), "server_error: boom");
    assert_eq!(output.raw_stop_reason.as_deref(), Some("failed"));
}

#[tokio::test]
async fn namespaces_round_trip() {
    let model = base_model();
    let function_events = vec![
        json!({ "type": "response.output_item.added", "output_index": 0, "item": { "type": "function_call", "id": "fc_test", "call_id": "call_test", "name": "lookup", "arguments": "" } }),
        json!({ "type": "response.output_item.done", "output_index": 0, "item": { "type": "function_call", "id": "fc_test", "call_id": "call_test", "name": "lookup", "arguments": "{\"value\":\"hello\"}", "namespace": "dynamic_tools" } }),
        json!({ "type": "response.completed", "response": { "id": "resp_test", "status": "completed" } }),
    ];
    let (result, output, _) = run_events(&model, function_events, Default::default()).await;
    result.unwrap();
    assert!(output.error_message.is_none());
    let call = tool_call_block(&output, 0);
    assert_eq!(call.id, "call_test|fc_test");
    assert_eq!(call.namespace.as_deref(), Some("dynamic_tools"));
    let replay = transcript_from_messages(vec![output.clone().into()]);
    let items =
        convert_responses_messages(&model, &replay, &["openai"], &Default::default()).unwrap();
    let item = items.iter().find(|i| i["type"] == "function_call").unwrap();
    assert_eq!(
        item,
        &json!({ "type": "function_call", "id": "fc_test", "call_id": "call_test", "name": "lookup", "arguments": "{\"value\":\"hello\"}", "namespace": "dynamic_tools" })
    );

    let grammar: HashMap<String, String> = [("query".to_string(), "input".to_string())].into();
    let custom_events = vec![
        json!({ "type": "response.output_item.added", "output_index": 0, "item": { "type": "custom_tool_call", "id": "ctc_test", "call_id": "call_test", "name": "query", "input": "" } }),
        json!({ "type": "response.output_item.done", "output_index": 0, "item": { "type": "custom_tool_call", "id": "ctc_test", "call_id": "call_test", "name": "query", "input": "hello", "namespace": "dynamic_tools" } }),
        json!({ "type": "response.completed", "response": { "id": "resp_test", "status": "completed" } }),
    ];
    let (result, output, events) = run_events(
        &model,
        custom_events,
        OpenAIResponsesStreamOptions {
            grammar_tool_input_properties: Some(&grammar),
            ..Default::default()
        },
    )
    .await;
    result.unwrap();
    let call = tool_call_block(&output, 0);
    assert_eq!(
        Value::Object(call.arguments.clone()),
        json!({ "input": "hello" })
    );
    let deltas: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AssistantMessageEvent::ToolCallDelta { delta, .. } => Some(delta.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(deltas.concat(), "{\"input\":\"hello\"}");
    let replay = transcript_from_messages(vec![output.clone().into()]);
    let options = ConvertResponsesMessagesOptions {
        grammar_tool_input_properties: Some(&grammar),
        ..Default::default()
    };
    let items = convert_responses_messages(&model, &replay, &["openai"], &options).unwrap();
    let item = items
        .iter()
        .find(|i| i["type"] == "custom_tool_call")
        .unwrap();
    assert_eq!(
        item,
        &json!({ "type": "custom_tool_call", "id": "ctc_test", "call_id": "call_test", "name": "query", "input": "hello", "namespace": "dynamic_tools" })
    );

    // Targets that cannot replay namespaces drop them.
    let mut output = output_for(&model);
    output.content.push(AssistantContent::ToolCall(ToolCall {
        id: "call_function|fc_test".into(),
        name: "lookup".into(),
        arguments: json!({ "value": "hello" }).as_object().cloned().unwrap(),
        namespace: Some("dynamic_tools".into()),
        ..Default::default()
    }));
    output.content.push(AssistantContent::ToolCall(ToolCall {
        id: "call_custom|ctc_test".into(),
        name: "query".into(),
        arguments: json!({ "input": "hello" }).as_object().cloned().unwrap(),
        namespace: Some("dynamic_tools".into()),
        ..Default::default()
    }));
    let mut targets = vec![model.clone(), model.clone(), model.clone()];
    targets[0].id = "gpt-5.2".into();
    targets[1].provider = "azure".into();
    targets[2].api = "openai-codex-responses".into();
    targets[2].provider = "openai-codex".into();
    targets[2].id = "gpt-5.3-codex-spark".into();
    for target in targets {
        let replay = transcript_from_messages(vec![output.clone().into()]);
        let items = convert_responses_messages(&target, &replay, &["openai"], &options).unwrap();
        let function = items.iter().find(|i| i["type"] == "function_call").unwrap();
        let custom = items
            .iter()
            .find(|i| i["type"] == "custom_tool_call")
            .unwrap();
        assert!(function.get("namespace").is_none());
        assert!(custom.get("namespace").is_none());
    }
}

#[tokio::test]
async fn azure_reasoning_replay_backfills_encrypted_content() {
    let mut model = base_model();
    model.api = "azure-openai-responses".into();
    model.provider = "azure".into();
    for (done_encrypted, expected) in [
        (Some("from-output-item-done"), "from-output-item-done"),
        (None, "from-response-completed"),
    ] {
        let id = if done_encrypted.is_some() {
            "rs_done"
        } else {
            "rs_missing"
        };
        let mut done_item = json!({ "type": "reasoning", "id": id, "summary": [] });
        if let Some(encrypted) = done_encrypted {
            done_item["encrypted_content"] = json!(encrypted);
        }
        let mut completed_item = done_item.clone();
        completed_item["encrypted_content"] = json!("from-response-completed");
        let events = vec![
            json!({ "type": "response.output_item.added", "output_index": 0, "item": { "type": "reasoning", "id": id, "summary": [] } }),
            json!({ "type": "response.output_item.done", "output_index": 0, "item": done_item }),
            json!({ "type": "response.completed", "response": { "id": "resp_test", "status": "completed", "output": [completed_item] } }),
        ];
        let (result, output, _) = run_events(&model, events, Default::default()).await;
        result.unwrap();
        let context = ctx(
            None,
            vec![user("first"), output.into(), user("follow-up")],
            None,
        );
        let items =
            convert_responses_messages(&model, &context, &["azure"], &Default::default()).unwrap();
        let reasoning = items.iter().find(|i| i["type"] == "reasoning").unwrap();
        assert_eq!(reasoning["id"], id);
        assert_eq!(reasoning["encrypted_content"], expected);
    }
}

// ---------------------------------------------------------------------------
// openai-responses stream (HTTP)
// ---------------------------------------------------------------------------

type Captured = Arc<Mutex<Option<Value>>>;

fn capture_payload(options: &mut StreamOptions) -> Captured {
    let captured: Captured = Arc::default();
    let sink = captured.clone();
    options.on_payload = Some(Arc::new(move |payload, _model| {
        *sink.lock() = Some(payload);
        Box::pin(async { None })
    }));
    captured
}

fn done_sse() -> Reply {
    Reply::sse("data: [DONE]\n\n")
}

async fn run_with_server(
    model: &Model,
    context: &TranscriptContext,
    mut options: StreamOptions,
    reply: Reply,
) -> (AssistantMessage, Option<Value>, Vec<Request>) {
    let server = serve_sequence(vec![reply]).await;
    let mut model = model.clone();
    model.base_url = server.base_url.clone();
    let captured = capture_payload(&mut options);
    let result = openai_responses::stream(&model, context, options)
        .result()
        .await;
    let payload = captured.lock().clone();
    (result, payload, server.requests())
}

async fn payload_only(
    model: &Model,
    context: &TranscriptContext,
    mut options: StreamOptions,
) -> Value {
    options.http_client = Some(refusing_client());
    let captured = capture_payload(&mut options);
    let _ = openai_responses::stream(model, context, options)
        .result()
        .await;
    let payload = captured.lock().clone();
    payload.expect("payload captured")
}

fn api_key(key: &str) -> StreamOptions {
    StreamOptions {
        api_key: Some(key.into()),
        ..Default::default()
    }
}

fn hi() -> TranscriptContext {
    ctx(Some("sys"), vec![user("hi")], None)
}

#[tokio::test]
async fn copilot_omits_reasoning_without_request() {
    let model = get_builtin_model("github-copilot", "gpt-6-sol").unwrap();
    let payload = payload_only(&model, &hi(), api_key("sk-test-key")).await;
    assert!(payload.get("reasoning").is_none());
}

#[tokio::test]
async fn forwards_required_tool_choice_with_additional_tools() {
    let model = get_builtin_model("openai", "gpt-5.4").unwrap();
    let tools = vec![Tool::new(
        "ping",
        "Ping",
        json!({ "type": "object", "properties": { "value": { "type": "string" } }, "required": ["value"] }),
    )];
    let context = ctx(
        None,
        vec![user("Do not call ping. Respond with text instead.")],
        Some(tools),
    );
    let payload = payload_only(
        &model,
        &context,
        api_key("sk-test-key").with_extra("toolChoice", "required"),
    )
    .await;
    assert_eq!(payload["tool_choice"], "required");
    assert_eq!(payload["input"][0]["type"], "additional_tools");
    assert_eq!(payload["input"][0]["tools"][0]["name"], "ping");
}

#[tokio::test]
async fn cloudflare_strict_mode_tools() {
    let model = get_builtin_model("cloudflare-ai-gateway", "gpt-5.6-sol").unwrap();
    assert_eq!(
        model.compat.as_ref().and_then(|c| c.supports_strict_mode),
        Some(true)
    );
    let mut constrained = Tool::new(
        "constrained",
        "A constrained tool",
        json!({ "type": "object", "properties": { "value": { "type": "string" } }, "required": ["value"] }),
    );
    constrained.constrained_sampling =
        Some(serde_json::from_value(json!({ "type": "json_schema", "strict": "prefer" })).unwrap());
    let tools = vec![
        Tool::new(
            "ordinary",
            "An ordinary tool",
            json!({ "type": "object", "properties": { "path": { "type": "string" }, "offset": { "type": "number" } }, "required": ["path"] }),
        ),
        constrained,
    ];
    let context = ctx(None, vec![user("Use a tool.")], Some(tools));
    let payload = payload_only(&model, &context, api_key("sk-test-key")).await;
    let tools = payload["tools"].as_array().unwrap();
    assert_eq!(
        (tools[0]["name"].clone(), tools[0]["strict"].clone()),
        (json!("ordinary"), json!(false))
    );
    assert_eq!(
        (tools[1]["name"].clone(), tools[1]["strict"].clone()),
        (json!("constrained"), json!(true))
    );
}

#[tokio::test]
async fn reasoning_none_and_omitted_defaults() {
    for id in ["gpt-5.1", "gpt-5.4", "gpt-5.5", "gpt-6-luna"] {
        let model = get_builtin_model("openai", id).unwrap();
        let payload = payload_only(&model, &hi(), api_key("sk-test-key")).await;
        assert_eq!(payload["reasoning"], json!({ "effort": "none" }), "{id}");
    }
    for id in ["gpt-5", "gpt-5-mini", "gpt-5-pro"] {
        let model = get_builtin_model("openai", id).unwrap();
        let payload = payload_only(&model, &hi(), api_key("sk-test-key")).await;
        assert!(payload.get("reasoning").is_none(), "{id}");
    }
}

#[tokio::test]
async fn prompt_cache_key_and_max_output_tokens() {
    let model = get_builtin_model("openai", "gpt-5.4").unwrap();
    let mut options = api_key("sk-test-key");
    options.session_id = Some("x".repeat(67));
    options.max_tokens = Some(1024);
    let payload = payload_only(&model, &hi(), options).await;
    assert_eq!(payload["prompt_cache_key"], json!("x".repeat(64)));
    assert_eq!(payload["max_output_tokens"], 1024);
    assert_eq!(payload["store"], false);

    let mut no_max = model.clone();
    no_max
        .compat
        .get_or_insert_with(Default::default)
        .supports_max_output_tokens = Some(false);
    let mut options = api_key("sk-test-key");
    options.max_tokens = Some(1024);
    let payload = payload_only(&no_max, &hi(), options).await;
    assert!(payload.get("max_output_tokens").is_none());
}

#[tokio::test]
async fn chatgpt_sign_in_omits_rejected_fields() {
    let model = base_model();
    let with = |key: &str| {
        let mut options = api_key(key);
        options.max_tokens = Some(1000);
        options.temperature = Some(0.5);
        options.cache_retention = Some(CacheRetention::Long);
        options
    };
    let context = ctx(
        None,
        vec![Message::User(UserMessage::new(
            vec![UserContentBlock::text("hi")],
            0,
        ))],
        Some(vec![]),
    );
    let payload = payload_only(&model, &context, with("chatgpt-access-token")).await;
    assert!(payload.get("max_output_tokens").is_none());
    assert!(payload.get("temperature").is_none());
    assert!(payload.get("prompt_cache_retention").is_none());

    let mut explicit = model.clone();
    explicit.compat =
        Some(serde_json::from_value(json!({ "supportsExplicitPromptCacheMode": true })).unwrap());
    let sign_in = payload_only(&explicit, &context, with("chatgpt-access-token")).await;
    let key = payload_only(&explicit, &context, with("sk-proj-test")).await;
    assert!(sign_in.get("prompt_cache_options").is_none());
    assert_eq!(key["prompt_cache_options"], json!({ "ttl": "30m" }));

    let mut gateway = model.clone();
    gateway.base_url = "https://gateway.example.com/v1".into();
    for (key, target) in [("sk-proj-test", &model), ("gateway-key", &gateway)] {
        let payload = payload_only(target, &context, with(key)).await;
        assert_eq!(payload["max_output_tokens"], 1000);
        assert_eq!(payload["temperature"], 0.5);
        assert_eq!(payload["prompt_cache_retention"], "24h");
    }
}

#[tokio::test]
async fn session_affinity_headers() {
    let base = get_builtin_model("openai", "gpt-5.4").unwrap();
    let headers_for = |model: Model, options: StreamOptions| async move {
        let (_, payload, requests) = run_with_server(&model, &hi(), options, done_sse()).await;
        let request = requests.into_iter().next().expect("request");
        (request, payload.unwrap())
    };
    let mut options = api_key("sk-test-key");
    options.session_id = Some("session-123".into());
    let (request, _) = headers_for(base.clone(), options.clone()).await;
    assert_eq!(request.header("session_id"), Some("session-123"));
    assert_eq!(request.header("x-client-request-id"), Some("session-123"));
    assert_eq!(request.header("authorization"), Some("Bearer sk-test-key"));
    assert_eq!(request.path, "/responses");
    assert_eq!(
        request.header("user-agent").map(str::to_string),
        Some(format_user_agent(None))
    );

    let mut openrouter = base.clone();
    openrouter.provider = "openrouter".into();
    let mut options = api_key("sk-test-key");
    options.session_id = Some("session-openrouter".into());
    let (request, payload) = headers_for(openrouter, options).await;
    assert_eq!(request.header("session_id"), None);
    assert_eq!(request.header("x-client-request-id"), None);
    assert_eq!(request.header("x-session-id"), Some("session-openrouter"));
    assert_eq!(payload["prompt_cache_key"], "session-openrouter");

    let mut nosession = base.clone();
    nosession.provider = "proxy".into();
    nosession.compat = Some(
        serde_json::from_value(json!({ "sessionAffinityFormat": "openai-nosession" })).unwrap(),
    );
    let mut options = api_key("sk-test-key");
    options.session_id = Some("session-proxy".into());
    let (request, _) = headers_for(nosession, options).await;
    assert_eq!(request.header("session_id"), None);
    assert_eq!(request.header("x-client-request-id"), Some("session-proxy"));
    assert_eq!(request.header("x-session-id"), None);

    let mut options = api_key("sk-test-key");
    options.session_id = Some("session-123".into());
    let mut headers = ProviderHeaders::new();
    headers.insert("session_id".into(), Some("override-session".into()));
    headers.insert(
        "x-client-request-id".into(),
        Some("override-request".into()),
    );
    headers.insert("User-Agent".into(), Some("custom-agent".into()));
    options.headers = Some(headers);
    let (request, _) = headers_for(base.clone(), options).await;
    assert_eq!(request.header("session_id"), Some("override-session"));
    assert_eq!(
        request.header("x-client-request-id"),
        Some("override-request")
    );
    assert_eq!(request.header("user-agent"), Some("custom-agent"));

    let mut options = api_key("sk-test-key");
    options.session_id = Some("session-123".into());
    options.cache_retention = Some(CacheRetention::None);
    let (request, payload) = headers_for(base, options).await;
    assert_eq!(request.header("session_id"), None);
    assert_eq!(request.header("x-client-request-id"), None);
    assert!(payload.get("prompt_cache_key").is_none());
}

#[tokio::test]
async fn service_tier_cost_multipliers() {
    for (id, tier, response_tier, multiplier) in [
        ("gpt-5.4", "priority", "priority", 2.0),
        ("gpt-5.5", "priority", "priority", 2.5),
        ("gpt-5.5", "flex", "flex", 0.5),
        ("gpt-6-luna", "priority", "fast", 2.0),
        ("gpt-6-luna", "fast", "fast", 2.0),
    ] {
        let model = get_builtin_model("openai", id).unwrap();
        let tokens = 100_000u64;
        let scale = tokens as f64 / 1_000_000.0;
        let sse = data_sse(&[
            json!({ "type": "response.completed", "response": { "status": "completed", "service_tier": response_tier, "usage": { "input_tokens": tokens, "output_tokens": tokens, "total_tokens": tokens * 2, "input_tokens_details": { "cached_tokens": 0 } } } }),
        ]);
        let (result, payload, _) = run_with_server(
            &model,
            &hi(),
            api_key("sk-test-key").with_extra("serviceTier", tier),
            Reply::sse(sse),
        )
        .await;
        assert_eq!(payload.unwrap()["service_tier"], tier);
        assert_eq!(
            result.stop_reason,
            StopReason::Stop,
            "{:?}",
            result.error_message
        );
        let rates = model.cost.rates();
        assert!(
            (result.usage.cost.input - rates.input * multiplier * scale).abs() < 1e-9,
            "{id}"
        );
        assert!(
            (result.usage.cost.output - rates.output * multiplier * scale).abs() < 1e-9,
            "{id}"
        );
    }
}

#[tokio::test]
async fn usage_limit_errors_link_to_chatgpt_usage() {
    let model = base_model();
    let context = ctx(None, vec![user("hi")], None);
    let error = json!({ "error": { "code": "subscription_sharing_usage_limit_exceeded", "message": "Usage limit reached.", "type": "rate_limit_error" } });
    let (result, _, _) = run_with_server(
        &model,
        &context,
        api_key("test"),
        Reply::json(429, error.to_string()),
    )
    .await;
    assert_eq!(result.stop_reason, StopReason::Error);
    let message = result.error_message.unwrap();
    assert!(
        message.contains("subscription_sharing_usage_limit_exceeded"),
        "{message}"
    );
    assert!(message.contains("Check your ChatGPT usage: https://chatgpt.com/settings/usage"));

    let event = json!({ "type": "response.failed", "sequence_number": 0, "response": { "id": "resp_failed", "status": "failed", "error": { "code": "subscription_sharing_usage_limit_exceeded", "message": "Usage limit reached." } } });
    let body = format!("event: response.failed\ndata: {event}\n\n");
    let (result, _, _) = run_with_server(&model, &context, api_key("test"), Reply::sse(body)).await;
    let message = result.error_message.unwrap();
    assert!(message.contains("subscription_sharing_usage_limit_exceeded: Usage limit reached."));
    assert!(message.contains("Check your ChatGPT usage"));
}

#[tokio::test]
async fn early_eof_streams_error_and_forward_provider_events() {
    let model = base_model();
    let events: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = events.clone();
    let mut options = api_key("test");
    options.on_provider_stream_event = Some(Arc::new(move |event: Value, _model: Model| {
        sink.lock()
            .push(event["type"].as_str().unwrap_or_default().to_string());
        Box::pin(async {})
    }));
    let server = serve_sequence(vec![Reply::sse(data_sse(&early_eof_events()))]).await;
    let mut model = model;
    model.base_url = server.base_url.clone();
    let (all, result) = openai_responses::stream(&model, &hi(), options)
        .collect_all()
        .await;
    assert_eq!(
        all.first().unwrap().message().stop_reason,
        StopReason::Pending
    );
    assert_eq!(all.last().unwrap().type_name(), "error");
    assert_eq!(
        result.error_message.as_deref(),
        Some("OpenAI Responses stream ended before a terminal response event")
    );
    assert_eq!(
        *events.lock(),
        vec![
            "response.created",
            "response.output_item.added",
            "response.reasoning_text.delta"
        ]
    );
}

#[tokio::test]
async fn http_error_formatting() {
    let model = base_model();
    let (result, _, _) = run_with_server(
        &model,
        &hi(),
        api_key("sk-x"),
        Reply::json(400, r#"{"error":{"message":"Bad things","code":null}}"#),
    )
    .await;
    assert_eq!(
        result.error_message.as_deref(),
        Some(r#"OpenAI API error (400): {"message":"Bad things","code":null}"#)
    );
    let missing = openai_responses::stream_simple(&model, &hi(), StreamOptions::default())
        .result()
        .await;
    assert_eq!(
        missing.error_message.as_deref(),
        Some("No API key for provider: openai")
    );
}

// ---------------------------------------------------------------------------
// configuration_update (mid-conversation effort)
// ---------------------------------------------------------------------------

fn luna(compat: Value) -> Model {
    serde_json::from_value(json!({
        "id": "gpt-6-luna", "name": "GPT-6 Luna", "api": "openai-responses", "provider": "openai",
        "baseUrl": "https://api.openai.com/v1", "reasoning": true,
        "thinkingLevelMap": { "off": "none", "minimal": null, "low": "low", "medium": "medium", "high": "high", "xhigh": "xhigh", "max": "max" },
        "input": ["text"], "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 272000, "maxTokens": 128000, "compat": compat
    }))
    .unwrap()
}

fn update(effort: &str) -> Value {
    json!({ "type": "configuration_update", "reasoning": { "effort": effort } })
}

async fn run_luna(
    messages: Vec<Message>,
    effort: Option<&str>,
    model: &Model,
    with_tool_call: bool,
) -> (Value, AssistantMessage) {
    let item = if with_tool_call {
        json!({ "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "lookup", "arguments": "{}", "status": "completed" })
    } else {
        json!({ "type": "message", "id": "msg_resp_1", "role": "assistant", "status": "completed", "content": [{ "type": "output_text", "text": "answer", "annotations": [] }] })
    };
    let sse = data_sse(&[
        json!({ "type": "response.created", "response": { "id": "resp_1" } }),
        json!({ "type": "response.output_item.added", "output_index": 0, "item": item }),
        json!({ "type": "response.output_item.done", "output_index": 0, "item": item }),
        json!({ "type": "response.completed", "response": { "id": "resp_1", "status": "completed" } }),
    ]);
    let mut options = api_key("sk-test");
    if let Some(effort) = effort {
        options = options.with_extra("reasoningEffort", effort);
    }
    let (message, payload, _) =
        run_with_server(model, &ctx(None, messages, None), options, Reply::sse(sse)).await;
    assert_ne!(
        message.stop_reason,
        StopReason::Error,
        "{:?}",
        message.error_message
    );
    (payload.unwrap(), message)
}

fn strip_base(mut message: AssistantMessage, model: &Model) -> AssistantMessage {
    // The test server rewrote base_url only; the message identity stays the model's.
    message.provider = model.provider.clone();
    message
}

#[tokio::test]
async fn configuration_update_keeps_effort_constant() {
    let model = luna(json!({ "supportsMidConvoEffort": true }));
    let mut history = vec![user("one")];
    let (first_payload, first) = run_luna(history.clone(), Some("high"), &model, false).await;
    history.push(strip_base(first.clone(), &model).into());
    history.push(user("two"));
    let (second_payload, second) = run_luna(history.clone(), Some("high"), &model, false).await;
    history.push(second.clone().into());
    history.push(user("three"));
    let (third_payload, third) = run_luna(history.clone(), Some("low"), &model, false).await;

    assert_eq!(
        first_payload["input"],
        json!([{ "role": "user", "content": [{ "type": "input_text", "text": "one" }] }, update("high")])
    );
    for payload in [&first_payload, &second_payload, &third_payload] {
        assert_eq!(payload["reasoning"]["effort"], "medium");
    }
    let first_input = first_payload["input"].as_array().unwrap();
    let second_input = second_payload["input"].as_array().unwrap();
    let third_input = third_payload["input"].as_array().unwrap();
    assert_eq!(&second_input[..first_input.len()], first_input.as_slice());
    assert_eq!(&third_input[..second_input.len()], second_input.as_slice());
    let updates: Vec<&Value> = third_input
        .iter()
        .filter(|i| i["type"] == "configuration_update")
        .collect();
    assert_eq!(
        updates,
        vec![&update("high"), &update("high"), &update("low")]
    );
    assert_eq!(third_input.last().unwrap(), &update("low"));
    let levels: Vec<Option<String>> = [first, second, third]
        .iter()
        .map(|m| m.provider_thinking_level.clone())
        .collect();
    assert_eq!(
        levels,
        vec![Some("high".into()), Some("high".into()), Some("low".into())]
    );

    // A level change during a tool loop applies to the next reply.
    let (tool_payload, tool_message) =
        run_luna(vec![user("one")], Some("high"), &model, true).await;
    let result = ToolResultMessage {
        tool_call_id: "call_1|fc_1".into(),
        tool_name: "lookup".into(),
        content: vec![UserContentBlock::text("result")],
        ..Default::default()
    };
    let (after_payload, after) = run_luna(
        vec![user("one"), tool_message.into(), result.into()],
        Some("low"),
        &model,
        false,
    )
    .await;
    let tool_input = tool_payload["input"].as_array().unwrap();
    let after_input = after_payload["input"].as_array().unwrap();
    assert_eq!(&after_input[..tool_input.len()], tool_input.as_slice());
    let tail = &after_input[after_input.len() - 2..];
    assert_eq!(tail[0]["type"], "function_call_output");
    assert_eq!(tail[0]["call_id"], "call_1");
    assert_eq!(tail[1], update("low"));
    assert_eq!(after.provider_thinking_level.as_deref(), Some("low"));

    // Thinking off and compat off send reasoning.effort directly.
    let (_, first) = run_luna(vec![user("one")], Some("high"), &model, false).await;
    let (payload, message) = run_luna(
        vec![user("one"), first.into(), user("two")],
        None,
        &model,
        false,
    )
    .await;
    assert_eq!(payload["reasoning"]["effort"], "none");
    assert!(
        !payload["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["type"] == "configuration_update")
    );
    assert!(message.provider_thinking_level.is_none());
    let compat_off = luna(json!({ "supportsMidConvoEffort": false }));
    let (payload, message) = run_luna(vec![user("one")], Some("low"), &compat_off, false).await;
    assert_eq!(payload["reasoning"]["effort"], "low");
    assert!(message.provider_thinking_level.is_none());
}

#[tokio::test]
async fn configuration_update_disabled_for_chatgpt_sign_in() {
    let model = luna(json!({ "supportsMidConvoEffort": true }));
    let payload = payload_only(
        &model,
        &ctx(None, vec![user("one")], None),
        api_key("chatgpt-access-token").with_extra("reasoningEffort", "low"),
    )
    .await;
    assert_eq!(payload["reasoning"]["effort"], "low");
    assert!(
        !payload["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["type"] == "configuration_update")
    );
}

// ---------------------------------------------------------------------------
// xAI via Responses
// ---------------------------------------------------------------------------

#[tokio::test]
async fn xai_responses_request_fields() {
    let model = get_builtin_model("xai", "grok-4.5").unwrap();
    assert_eq!(model.api, "openai-responses");
    let completed = data_sse(&[
        json!({ "type": "response.completed", "sequence_number": 0, "response": { "id": "resp_xai_test", "status": "completed", "output": [], "usage": { "input_tokens": 1, "output_tokens": 1, "total_tokens": 2, "input_tokens_details": { "cached_tokens": 0 } } } }),
    ]);
    let mut options = api_key("xai-test-token");
    options.session_id = Some("pi-session-123".into());
    options.cache_retention = Some(CacheRetention::Long);
    let context = ctx(
        Some("You are a careful coding assistant."),
        vec![user("hello")],
        None,
    );
    let (result, payload, requests) = run_with_server(
        &model,
        &context,
        options.with_extra("reasoningEffort", "medium"),
        Reply::sse(completed.clone()),
    )
    .await;
    assert_eq!(
        result.stop_reason,
        StopReason::Stop,
        "{:?}",
        result.error_message
    );
    let payload = payload.unwrap();
    let request = &requests[0];
    assert_eq!(request.path, "/responses");
    assert_eq!(
        request.header("authorization"),
        Some("Bearer xai-test-token")
    );
    assert_eq!(request.header("session_id"), Some("pi-session-123"));
    assert_eq!(payload["model"], "grok-4.5");
    assert_eq!(payload["store"], false);
    assert_eq!(payload["prompt_cache_key"], "pi-session-123");
    assert_eq!(payload["reasoning"]["effort"], "medium");
    assert_eq!(payload["include"], json!(["reasoning.encrypted_content"]));
    assert!(payload.get("prompt_cache_retention").is_none());
    assert!(payload["input"].as_array().unwrap().contains(
        &json!({ "role": "developer", "content": "You are a careful coding assistant." })
    ));

    let (_, payload, _) = run_with_server(
        &model,
        &ctx(None, vec![user("hello")], None),
        api_key("xai-test-token"),
        Reply::sse(completed),
    )
    .await;
    let payload = payload.unwrap();
    assert_eq!(payload["include"], json!(["reasoning.encrypted_content"]));
    assert!(payload.get("reasoning").is_none());
}

// ---------------------------------------------------------------------------
// Azure OpenAI Responses
// ---------------------------------------------------------------------------

fn azure_model(base_url: &str) -> Model {
    let mut model = get_builtin_model("azure", "gpt-4o-mini").unwrap();
    model.base_url = base_url.to_string();
    model
}

async fn azure_payload(
    model: &Model,
    context: &TranscriptContext,
    mut options: StreamOptions,
    simple: bool,
) -> Value {
    options.http_client = Some(refusing_client());
    let captured = capture_payload(&mut options);
    let stream = if simple {
        azure_openai_responses::stream_simple(model, context, options)
    } else {
        azure_openai_responses::stream(model, context, options)
    };
    let _ = stream.result().await;
    let payload = captured.lock().clone();
    payload.expect("payload")
}

#[tokio::test]
async fn azure_requests_use_api_key_and_api_version() {
    let server = serve_sequence(vec![Reply::sse(data_sse(&[
        json!({ "type": "response.created", "response": { "id": "resp_azure" } }),
        json!({ "type": "response.completed", "response": { "id": "resp_azure", "status": "completed" } }),
    ]))])
    .await;
    let model = azure_model(&format!("{}/openai/v1", server.base_url));
    let events: Arc<Mutex<Vec<Value>>> = Arc::default();
    let sink = events.clone();
    let mut options = api_key("azure-key");
    options.session_id = Some("x".repeat(70));
    options.on_provider_stream_event = Some(Arc::new(move |event: Value, _m: Model| {
        sink.lock().push(event);
        Box::pin(async {})
    }));
    let result = azure_openai_responses::stream(&model, &hi(), options)
        .result()
        .await;
    assert_eq!(
        result.stop_reason,
        StopReason::Stop,
        "{:?}",
        result.error_message
    );
    assert_eq!(result.response_id.as_deref(), Some("resp_azure"));
    assert_eq!(events.lock().len(), 2);
    let request = &server.requests()[0];
    assert_eq!(request.path, "/openai/v1/responses?api-version=v1");
    assert_eq!(request.header("api-key"), Some("azure-key"));
    assert!(request.header("authorization").is_none());
    let body = request.json();
    assert_eq!(body["prompt_cache_key"], json!("x".repeat(64)));
    assert_eq!(body["store"], false);
    assert_eq!(body["model"], "gpt-4o-mini");
}

#[tokio::test]
async fn azure_base_url_errors_and_tool_choice() {
    let model = azure_model("not a url");
    let result = azure_openai_responses::stream(&model, &hi(), api_key("k"))
        .result()
        .await;
    assert_eq!(result.stop_reason, StopReason::Error);
    assert!(
        result
            .error_message
            .unwrap()
            .contains("Invalid Azure OpenAI base URL")
    );

    let model = azure_model("https://my-resource.openai.azure.com");
    let tools = vec![Tool::new(
        "ping",
        "Ping",
        json!({ "type": "object", "properties": {}, "required": [] }),
    )];
    let context = ctx(Some("sys"), vec![user("hi")], Some(tools));
    let payload = azure_payload(
        &model,
        &context,
        api_key("k").with_extra("toolChoice", "required"),
        false,
    )
    .await;
    assert_eq!(payload["tool_choice"], "required");
    assert_eq!(payload["tools"].as_array().unwrap().len(), 1);
    assert_eq!(payload["tools"][0]["strict"], false);
    let mut simple = api_key("k");
    simple.tool_choice = Some(ToolChoice::None);
    let payload = azure_payload(&model, &context, simple, true).await;
    assert_eq!(payload["tool_choice"], "none");

    let mut no_strict = model.clone();
    no_strict.compat =
        Some(serde_json::from_value(json!({ "supportsStrictMode": false })).unwrap());
    let payload = azure_payload(&no_strict, &context, api_key("k"), false).await;
    assert!(payload["tools"][0].get("strict").is_none());
}
