#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use pi_agent::{AgentMessage, AgentToolRef, AgentToolResult, FnAgentTool, StreamFn};
use pi_ai::utils::now_ms;
use pi_ai::{
    AssistantContent, AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream,
    Message, Model, ModelCost, ModelInputModality, SimpleStreamOptions, StopReason, TextContent,
    ToolCall, TranscriptContext, Usage, UserContent, UserMessage,
};
use serde_json::{Value, json};

pub fn create_model() -> Model {
    Model {
        id: "mock".into(),
        name: "mock".into(),
        api: "openai-responses".into(),
        provider: "openai".into(),
        base_url: "https://example.invalid".into(),
        reasoning: false,
        input: vec![ModelInputModality::Text],
        cost: ModelCost::default(),
        context_window: 8192,
        max_tokens: 2048,
        ..Default::default()
    }
}

pub fn assistant(content: Vec<AssistantContent>, stop_reason: StopReason) -> AssistantMessage {
    AssistantMessage {
        content: content.into(),
        api: "openai-responses".into(),
        provider: "openai".into(),
        model: "mock".into(),
        usage: Usage::default(),
        stop_reason,
        timestamp: now_ms(),
        ..Default::default()
    }
}

pub fn assistant_text(text: &str) -> AssistantMessage {
    assistant(
        vec![AssistantContent::Text(TextContent::new(text))],
        StopReason::Stop,
    )
}

pub fn tool_call(id: &str, name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: arguments.as_object().cloned().unwrap_or_default(),
        ..Default::default()
    }
}

pub fn assistant_tool_calls(calls: Vec<ToolCall>, stop_reason: StopReason) -> AssistantMessage {
    assistant(
        calls.into_iter().map(AssistantContent::ToolCall).collect(),
        stop_reason,
    )
}

pub fn user(text: &str) -> AgentMessage {
    AgentMessage::User(UserMessage::new(text, now_ms()))
}

pub fn user_text_of(message: &AgentMessage) -> Option<String> {
    match message {
        AgentMessage::User(UserMessage {
            content: UserContent::Text(text),
            ..
        }) => Some(text.clone()),
        _ => None,
    }
}

pub fn llm_user_texts(context: &TranscriptContext) -> Vec<String> {
    context
        .messages
        .iter()
        .filter_map(|m| match m {
            Message::User(UserMessage {
                content: UserContent::Text(text),
                ..
            }) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// A stream that delivers `message` as its terminal event on a later task.
pub fn respond(message: AssistantMessage) -> AssistantMessageEventStream {
    let (sender, stream) = AssistantMessageEventStream::new();
    tokio::spawn(async move {
        tokio::task::yield_now().await;
        push_terminal(&sender, message);
    });
    stream
}

pub fn push_terminal(sender: &pi_ai::AssistantMessageEventSender, message: AssistantMessage) {
    let reason = message.stop_reason;
    if matches!(reason, StopReason::Error | StopReason::Aborted) {
        sender.push(AssistantMessageEvent::Error {
            reason,
            error: message,
        });
    } else {
        sender.push(AssistantMessageEvent::Done { reason, message });
    }
}

/// A stream fn built from a synchronous closure.
pub fn mock_stream_fn(
    f: impl Fn(Model, TranscriptContext, SimpleStreamOptions) -> AssistantMessageEventStream
    + Send
    + Sync
    + 'static,
) -> StreamFn {
    pi_agent::stream_fn(f)
}

/// A stream fn answering every request with the message produced by `f(call_index)`.
pub fn scripted_stream_fn(
    f: impl Fn(usize) -> AssistantMessage + Send + Sync + 'static,
) -> (StreamFn, Arc<Mutex<usize>>) {
    let calls = Arc::new(Mutex::new(0usize));
    let counter = calls.clone();
    let stream_fn = mock_stream_fn(move |_, _, _| {
        let index = {
            let mut calls = counter.lock();
            let index = *calls;
            *calls += 1;
            index
        };
        respond(f(index))
    });
    (stream_fn, calls)
}

pub fn value_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "value": { "type": "string" } },
        "required": ["value"]
    })
}

pub fn empty_schema() -> Value {
    json!({ "type": "object", "properties": {} })
}

pub fn text_result(text: impl Into<String>, details: Value) -> AgentToolResult {
    AgentToolResult::text(text, Some(details))
}

/// An echo tool recording executed values.
pub fn echo_tool(executed: Arc<Mutex<Vec<String>>>) -> AgentToolRef {
    FnAgentTool::new(
        "echo",
        "Echo",
        "Echo tool",
        value_schema(),
        move |_id, params, _signal, _update| {
            let executed = executed.clone();
            async move {
                let value = params["value"].as_str().unwrap_or_default().to_string();
                executed.lock().push(value.clone());
                Ok(text_result(
                    format!("echoed: {value}"),
                    json!({ "value": value }),
                ))
            }
        },
    )
    .into_ref()
}

pub fn noop_tool(name: &str) -> AgentToolRef {
    FnAgentTool::new(
        name,
        name,
        format!("{name} tool"),
        empty_schema(),
        |_id, _params, _signal, _update| async { Ok(text_result("done", json!({}))) },
    )
    .into_ref()
}

pub async fn sleep_ms(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

pub fn roles(messages: &[AgentMessage]) -> Vec<String> {
    messages.iter().map(|m| m.role().to_string()).collect()
}
