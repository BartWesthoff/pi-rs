//! Proxy stream function for apps that route LLM calls through a server (port of `proxy.ts`).
//! The server manages auth and proxies requests to LLM providers.

use std::collections::HashMap;

use futures::StreamExt;
use pi_ai::utils::json_parse::parse_streaming_json_object;
use pi_ai::utils::now_ms;
use pi_ai::{
    AssistantContent, AssistantMessage, AssistantMessageEvent, AssistantMessageEventSender,
    AssistantMessageEventStream, CacheRetention, Model, ProviderHeaders, SamplingParams,
    SimpleStreamOptions, StopReason, TextContent, ThinkingBudgets, ThinkingContent, ThinkingLevel,
    ToolCall, TranscriptContext, Transport, Usage,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

/// Proxy event types: the server sends these with the `partial` field stripped to reduce
/// bandwidth.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ProxyAssistantMessageEvent {
    Start,
    TextStart {
        content_index: usize,
    },
    TextDelta {
        content_index: usize,
        delta: String,
    },
    TextEnd {
        content_index: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_signature: Option<String>,
    },
    ThinkingStart {
        content_index: usize,
    },
    ThinkingDelta {
        content_index: usize,
        delta: String,
    },
    ThinkingEnd {
        content_index: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_signature: Option<String>,
    },
    #[serde(rename = "toolcall_start")]
    ToolCallStart {
        content_index: usize,
        id: String,
        tool_name: String,
    },
    #[serde(rename = "toolcall_delta")]
    ToolCallDelta {
        content_index: usize,
        delta: String,
    },
    #[serde(rename = "toolcall_end")]
    ToolCallEnd {
        content_index: usize,
        tool_call: ToolCall,
    },
    /// `reason` is stop, length or toolUse.
    Done {
        reason: StopReason,
        usage: Usage,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_thinking_level: Option<String>,
    },
    /// `reason` is aborted or error.
    Error {
        reason: StopReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
        usage: Usage,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_thinking_level: Option<String>,
    },
}

/// Options for [`stream_proxy`].
#[derive(Clone, Default)]
pub struct ProxyStreamOptions {
    /// Local abort signal for the proxy request.
    pub signal: Option<CancellationToken>,
    /// Auth token for the proxy server.
    pub auth_token: String,
    /// Proxy server URL (e.g. "https://genai.example.com").
    pub proxy_url: String,
    /// HTTP client to use; a default client is created when `None`.
    pub http_client: Option<reqwest::Client>,
    pub temperature: Option<f64>,
    pub sampling_params: Option<SamplingParams>,
    pub max_tokens: Option<u64>,
    pub reasoning: Option<ThinkingLevel>,
    pub cache_retention: Option<CacheRetention>,
    pub session_id: Option<String>,
    pub headers: Option<ProviderHeaders>,
    pub metadata: Option<Map<String, Value>>,
    pub transport: Option<Transport>,
    pub thinking_budgets: Option<ThinkingBudgets>,
    pub max_retry_delay_ms: Option<u64>,
}

impl ProxyStreamOptions {
    /// Copy the serializable stream options and signal from `options`.
    pub fn from_stream_options(
        options: &SimpleStreamOptions,
        auth_token: impl Into<String>,
        proxy_url: impl Into<String>,
    ) -> Self {
        Self {
            signal: options.signal.clone(),
            auth_token: auth_token.into(),
            proxy_url: proxy_url.into(),
            http_client: options.http_client.clone(),
            temperature: options.temperature,
            sampling_params: options.sampling_params.clone(),
            max_tokens: options.max_tokens,
            reasoning: options.reasoning,
            cache_retention: options.cache_retention,
            session_id: options.session_id.clone(),
            headers: options.headers.clone(),
            metadata: options.metadata.clone(),
            transport: options.transport,
            thinking_budgets: options.thinking_budgets.clone(),
            max_retry_delay_ms: options.max_retry_delay_ms,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProxyRequestOptions<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sampling_params: Option<&'a SamplingParams>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ThinkingLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_retention: Option<CacheRetention>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    headers: Option<&'a ProviderHeaders>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<&'a Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transport: Option<Transport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking_budgets: Option<&'a ThinkingBudgets>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_retry_delay_ms: Option<u64>,
}

fn build_proxy_request_options(options: &ProxyStreamOptions) -> ProxyRequestOptions<'_> {
    ProxyRequestOptions {
        temperature: options.temperature,
        sampling_params: options.sampling_params.as_ref(),
        max_tokens: options.max_tokens,
        reasoning: options.reasoning,
        cache_retention: options.cache_retention,
        session_id: options.session_id.as_deref(),
        headers: options.headers.as_ref(),
        metadata: options.metadata.as_ref(),
        transport: options.transport,
        thinking_budgets: options.thinking_budgets.as_ref(),
        max_retry_delay_ms: options.max_retry_delay_ms,
    }
}

#[derive(Serialize)]
struct ProxyRequestBody<'a> {
    model: &'a Model,
    context: &'a TranscriptContext,
    options: ProxyRequestOptions<'a>,
}

/// Stream function that proxies through a server instead of calling LLM providers directly.
/// The server strips the partial field from delta events; the partial message is rebuilt
/// client-side. Must be called within a tokio runtime.
///
/// Use it as the agent's `stream_fn`:
///
/// ```ignore
/// let stream_fn: StreamFn = Arc::new(|model, context, options| {
///     let options = ProxyStreamOptions::from_stream_options(&options, token(), "https://genai.example.com");
///     Box::pin(async move { Ok(stream_proxy(model, context, options)) })
/// });
/// ```
pub fn stream_proxy(
    model: Model,
    context: TranscriptContext,
    options: ProxyStreamOptions,
) -> AssistantMessageEventStream {
    let (sender, stream) = AssistantMessageEventStream::new();
    tokio::spawn(async move {
        // Initialize the partial message that we'll build up from events.
        let mut state = ProxyState {
            partial: AssistantMessage {
                content: Vec::new().into(),
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                usage: Usage::default(),
                stop_reason: StopReason::Pending,
                timestamp: now_ms(),
                ..Default::default()
            },
            partial_json: HashMap::new(),
        };
        let signal = options.signal.clone().unwrap_or_default();

        let outcome = tokio::select! {
            outcome = run_proxy_request(&model, &context, &options, &mut state, &sender, &signal) => outcome,
            _ = signal.cancelled() => Err("Request aborted by user".to_string()),
        };

        if let Err(error_message) = outcome {
            let reason = if signal.is_cancelled() {
                StopReason::Aborted
            } else {
                StopReason::Error
            };
            state.partial.stop_reason = reason;
            state.partial.error_message = Some(error_message);
            sender.push(AssistantMessageEvent::Error {
                reason,
                error: state.partial.clone(),
            });
        }
        sender.end(None);
    });
    stream
}

struct ProxyState {
    partial: AssistantMessage,
    /// Accumulated tool-call argument JSON per content index.
    partial_json: HashMap<usize, String>,
}

async fn run_proxy_request(
    model: &Model,
    context: &TranscriptContext,
    options: &ProxyStreamOptions,
    state: &mut ProxyState,
    sender: &AssistantMessageEventSender,
    signal: &CancellationToken,
) -> Result<(), String> {
    let client = options.http_client.clone().unwrap_or_default();
    let body = ProxyRequestBody {
        model,
        context,
        options: build_proxy_request_options(options),
    };
    let response = client
        .post(format!("{}/api/stream", options.proxy_url))
        .header("Authorization", format!("Bearer {}", options.auth_token))
        .header("Content-Type", "application/json")
        .body(serde_json::to_vec(&body).map_err(|e| e.to_string())?)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = response.status();
    if !status.is_success() {
        let mut error_message = format!(
            "Proxy error: {} {}",
            status.as_u16(),
            status.canonical_reason().unwrap_or("")
        );
        if let Ok(error_data) = response.json::<Value>().await
            && let Some(error) = error_data
                .get("error")
                .and_then(Value::as_str)
                .filter(|e| !e.is_empty())
        {
            error_message = format!("Proxy error: {error}");
        }
        return Err(error_message);
    }

    let mut body = response.bytes_stream();
    let mut buffer: Vec<u8> = Vec::new();
    let mut saw_terminal_event = false;

    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        if signal.is_cancelled() {
            return Err("Request aborted by user".to_string());
        }
        buffer.extend_from_slice(&chunk);
        while let Some(newline) = buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buffer.drain(..=newline).collect();
            let line = String::from_utf8_lossy(&line[..line.len() - 1]).into_owned();
            process_line(&line, state, sender, &mut saw_terminal_event)?;
        }
    }

    if signal.is_cancelled() {
        return Err("Request aborted by user".to_string());
    }

    // The final event may not be newline-terminated; process whatever is left.
    if !buffer.is_empty() {
        let line = String::from_utf8_lossy(&buffer).into_owned();
        process_line(&line, state, sender, &mut saw_terminal_event)?;
    }

    if !saw_terminal_event {
        // A clean EOF without a done/error event means the server dropped the response
        // mid-stream. Surface it as an error instead of leaving consumers waiting.
        state.partial.stop_reason = StopReason::Error;
        state.partial.error_message =
            Some("Connection closed by proxy server before the response completed".to_string());
        sender.push(AssistantMessageEvent::Error {
            reason: StopReason::Error,
            error: state.partial.clone(),
        });
    }
    Ok(())
}

fn process_line(
    line: &str,
    state: &mut ProxyState,
    sender: &AssistantMessageEventSender,
    saw_terminal_event: &mut bool,
) -> Result<(), String> {
    let Some(data) = line.strip_prefix("data: ") else {
        return Ok(());
    };
    let data = data.trim();
    if data.is_empty() {
        return Ok(());
    }
    let value: Value = pi_ai::utils::js_json::js_json_parse(data)?;
    // TypeScript warns about and skips event types it does not know.
    if !is_known_proxy_event_type(value.get("type").and_then(Value::as_str)) {
        eprintln!(
            "Unhandled proxy event type: {}",
            value.get("type").map_or("undefined".into(), |t| match t {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
        );
        return Ok(());
    }
    let proxy_event: ProxyAssistantMessageEvent =
        serde_json::from_value(value).map_err(|e| e.to_string())?;
    if let Some(event) = process_proxy_event(proxy_event, state)? {
        if event.is_terminal() {
            *saw_terminal_event = true;
        }
        sender.push(event);
    }
    Ok(())
}

fn is_known_proxy_event_type(event_type: Option<&str>) -> bool {
    matches!(
        event_type,
        Some(
            "start"
                | "text_start"
                | "text_delta"
                | "text_end"
                | "thinking_start"
                | "thinking_delta"
                | "thinking_end"
                | "toolcall_start"
                | "toolcall_delta"
                | "toolcall_end"
                | "done"
                | "error"
        )
    )
}

fn set_content(partial: &mut AssistantMessage, index: usize, content: AssistantContent) {
    while partial.content.len() <= index {
        partial
            .content
            .push(AssistantContent::Text(TextContent::new("")));
    }
    partial.content[index] = content;
}

/// Process a proxy event and update the partial message.
fn process_proxy_event(
    proxy_event: ProxyAssistantMessageEvent,
    state: &mut ProxyState,
) -> Result<Option<AssistantMessageEvent>, String> {
    let partial = &mut state.partial;
    Ok(Some(match proxy_event {
        ProxyAssistantMessageEvent::Start => AssistantMessageEvent::Start {
            partial: partial.clone(),
        },

        ProxyAssistantMessageEvent::TextStart { content_index } => {
            set_content(
                partial,
                content_index,
                AssistantContent::Text(TextContent::new("")),
            );
            AssistantMessageEvent::TextStart {
                content_index,
                partial: partial.clone(),
            }
        }

        ProxyAssistantMessageEvent::TextDelta {
            content_index,
            delta,
        } => {
            match partial.content.get_mut(content_index) {
                Some(AssistantContent::Text(content)) => content.text.push_str(&delta),
                _ => return Err("Received text_delta for non-text content".to_string()),
            }
            AssistantMessageEvent::TextDelta {
                content_index,
                delta,
                partial: partial.clone(),
            }
        }

        ProxyAssistantMessageEvent::TextEnd {
            content_index,
            content_signature,
        } => {
            let text = match partial.content.get_mut(content_index) {
                Some(AssistantContent::Text(content)) => {
                    content.text_signature = content_signature;
                    content.text.clone()
                }
                _ => return Err("Received text_end for non-text content".to_string()),
            };
            AssistantMessageEvent::TextEnd {
                content_index,
                content: text,
                partial: partial.clone(),
            }
        }

        ProxyAssistantMessageEvent::ThinkingStart { content_index } => {
            set_content(
                partial,
                content_index,
                AssistantContent::Thinking(ThinkingContent::new("")),
            );
            AssistantMessageEvent::ThinkingStart {
                content_index,
                partial: partial.clone(),
            }
        }

        ProxyAssistantMessageEvent::ThinkingDelta {
            content_index,
            delta,
        } => {
            match partial.content.get_mut(content_index) {
                Some(AssistantContent::Thinking(content)) => content.thinking.push_str(&delta),
                _ => return Err("Received thinking_delta for non-thinking content".to_string()),
            }
            AssistantMessageEvent::ThinkingDelta {
                content_index,
                delta,
                partial: partial.clone(),
            }
        }

        ProxyAssistantMessageEvent::ThinkingEnd {
            content_index,
            content_signature,
        } => {
            let thinking = match partial.content.get_mut(content_index) {
                Some(AssistantContent::Thinking(content)) => {
                    content.thinking_signature = content_signature;
                    content.thinking.clone()
                }
                _ => return Err("Received thinking_end for non-thinking content".to_string()),
            };
            AssistantMessageEvent::ThinkingEnd {
                content_index,
                content: thinking,
                partial: partial.clone(),
            }
        }

        ProxyAssistantMessageEvent::ToolCallStart {
            content_index,
            id,
            tool_name,
        } => {
            set_content(
                partial,
                content_index,
                AssistantContent::ToolCall(ToolCall {
                    id,
                    name: tool_name,
                    ..Default::default()
                }),
            );
            state.partial_json.insert(content_index, String::new());
            AssistantMessageEvent::ToolCallStart {
                content_index,
                partial: partial.clone(),
            }
        }

        ProxyAssistantMessageEvent::ToolCallDelta {
            content_index,
            delta,
        } => {
            match partial.content.get_mut(content_index) {
                Some(AssistantContent::ToolCall(content)) => {
                    let json = state.partial_json.entry(content_index).or_default();
                    json.push_str(&delta);
                    content.arguments = parse_streaming_json_object(Some(json));
                }
                _ => return Err("Received toolcall_delta for non-toolCall content".to_string()),
            }
            AssistantMessageEvent::ToolCallDelta {
                content_index,
                delta,
                partial: partial.clone(),
            }
        }

        ProxyAssistantMessageEvent::ToolCallEnd {
            content_index,
            tool_call,
        } => {
            let merged = match partial.content.get_mut(content_index) {
                Some(AssistantContent::ToolCall(content)) => {
                    // Fields present on the final tool call replace the streamed ones.
                    let previous = std::mem::take(content);
                    *content = ToolCall {
                        thought_signature: tool_call
                            .thought_signature
                            .or(previous.thought_signature),
                        namespace: tool_call.namespace.or(previous.namespace),
                        arguments_error: tool_call.arguments_error.or(previous.arguments_error),
                        id: tool_call.id,
                        name: tool_call.name,
                        arguments: tool_call.arguments,
                    };
                    state.partial_json.remove(&content_index);
                    content.clone()
                }
                _ => return Ok(None),
            };
            AssistantMessageEvent::ToolCallEnd {
                content_index,
                tool_call: merged,
                partial: partial.clone(),
            }
        }

        ProxyAssistantMessageEvent::Done {
            reason,
            usage,
            provider_thinking_level,
        } => {
            partial.stop_reason = reason;
            partial.usage = usage;
            if provider_thinking_level.is_some() {
                partial.provider_thinking_level = provider_thinking_level;
            }
            AssistantMessageEvent::Done {
                reason,
                message: partial.clone(),
            }
        }

        ProxyAssistantMessageEvent::Error {
            reason,
            error_message,
            usage,
            provider_thinking_level,
        } => {
            partial.stop_reason = reason;
            partial.error_message = error_message;
            partial.usage = usage;
            if provider_thinking_level.is_some() {
                partial.provider_thinking_level = provider_thinking_level;
            }
            AssistantMessageEvent::Error {
                reason,
                error: partial.clone(),
            }
        }
    }))
}
