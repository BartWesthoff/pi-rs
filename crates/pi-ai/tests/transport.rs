//! The provider transport seam (`pi_ai::transport`): every API family sends the exact request
//! through a custom [`HttpTransport`]/[`WsTransport`] that it sends through the default reqwest /
//! tokio-tungstenite transports, and turns the transport's response into the same result.
//!
//! Each test runs one scenario twice: once over loopback against a mock server (the default
//! transports), once through a recording mock transport that replays the same response in small
//! chunks. The recorded request must match what the server received byte for byte (method, URL,
//! every header line the provider set, body), and the two results must be equal.

mod responses_mock;

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};

use base64::Engine;
use bytes::Bytes;
use futures::future::BoxFuture;
use futures::{Sink, Stream, StreamExt};
use parking_lot::Mutex;
use pi_ai::api::{
    anthropic_messages, azure_openai_responses, google_generative_ai, mistral_conversations, openai_codex_responses,
    openai_completions, openai_responses, openrouter_images, pi_messages, typesafe_system_one,
};
use pi_ai::providers::all::get_builtin_model;
use pi_ai::transport::{
    BoxWsConnection, HeaderMap, HttpRequest, HttpResponse, HttpTransport, TransportError, WsConnectRequest, WsError,
    WsMessage, WsTransport,
};
use pi_ai::*;
use responses_mock::*;
use serde_json::{Value, json};

/// Header lines the HTTP client adds on the wire by itself (never set by provider code).
const CLIENT_ADDED_HEADERS: [&str; 4] = ["host", "content-length", "accept-encoding", "accept"];

/// One request as a transport received it.
#[derive(Debug, Clone)]
struct Recorded {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// A transport that records requests and answers with canned responses, streaming each body in
/// 7-byte chunks.
#[derive(Default)]
struct RecordingTransport {
    requests: Mutex<Vec<Recorded>>,
    replies: Mutex<VecDeque<(u16, Vec<(String, String)>, Vec<u8>)>>,
}

impl RecordingTransport {
    fn new(replies: Vec<(u16, &str, Vec<u8>)>) -> Arc<Self> {
        let replies =
            replies.into_iter().map(|(status, content_type, body)| (status, vec![("content-type".into(), content_type.into())], body));
        Arc::new(Self { requests: Mutex::default(), replies: Mutex::new(replies.collect()) })
    }

    fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().clone()
    }
}

impl HttpTransport for RecordingTransport {
    fn send(&self, request: HttpRequest) -> BoxFuture<'static, Result<HttpResponse, TransportError>> {
        let body = match request.body {
            pi_ai::transport::HttpBody::Empty => Vec::new(),
            pi_ai::transport::HttpBody::Bytes(bytes) => bytes.to_vec(),
            pi_ai::transport::HttpBody::Stream(_) => panic!("providers send complete bodies"),
        };
        self.requests.lock().push(Recorded { method: request.method.to_string(), url: request.url, headers: request.headers, body });
        let (status, headers, body) = self.replies.lock().pop_front().expect("a reply for every request");
        Box::pin(async move {
            let mut map = HeaderMap::new();
            for (name, value) in headers {
                map.append(reqwest::header::HeaderName::from_bytes(name.as_bytes()).unwrap(), value.parse().unwrap());
            }
            let chunks: Vec<Result<Bytes, TransportError>> =
                body.chunks(7).map(|c| Ok(Bytes::copy_from_slice(c))).collect();
            let mut response = HttpResponse::from_bytes(status, map, Bytes::new());
            response.body = futures::stream::iter(chunks).boxed();
            Ok(response)
        })
    }
}

/// Assert that `recorded` is exactly the request `server` received over the wire.
fn assert_same_request(recorded: &Recorded, server: &Request, base_url: &str) {
    assert_eq!(recorded.method, server.method);
    assert_eq!(recorded.url.strip_prefix(base_url), Some(server.path.as_str()), "url {}", recorded.url);
    assert_eq!(recorded.body, server.body, "body bytes");
    let lower = |headers: &[(String, String)]| -> Vec<(String, String)> {
        headers.iter().map(|(n, v)| (n.to_ascii_lowercase(), v.clone())).collect()
    };
    let wire: Vec<(String, String)> = lower(&server.headers)
        .into_iter()
        .filter(|(name, _)| recorded.headers.iter().any(|(n, _)| n.eq_ignore_ascii_case(name)) || !CLIENT_ADDED_HEADERS.contains(&name.as_str()))
        .collect();
    assert_eq!(lower(&recorded.headers), wire, "header lines in order");
}

/// `message` without its wall-clock fields (timestamps and durations, at any depth).
fn comparable(message: &AssistantMessage) -> Value {
    fn strip(value: &mut Value) {
        match value {
            Value::Object(map) => {
                for key in ["timestamp", "timestampMs", "durationMs"] {
                    map.remove(key);
                }
                map.values_mut().for_each(strip);
            }
            Value::Array(items) => items.iter_mut().for_each(strip),
            _ => {}
        }
    }
    let mut value = serde_json::to_value(message).unwrap();
    strip(&mut value);
    value
}

fn with_transport(mut options: StreamOptions, transport: Arc<RecordingTransport>) -> StreamOptions {
    options.http_transport = Some(transport);
    options
}

fn key(key: &str) -> StreamOptions {
    StreamOptions { api_key: Some(key.into()), session_id: Some("session-1".into()), ..Default::default() }
}

fn context() -> TranscriptContext {
    normalize_context(&Context {
        system_prompt: Some("You are helpful.".into()),
        messages: vec![Message::User(UserMessage::new("Hello", 1))],
        tools: Some(vec![Tool::new(
            "read",
            "Read a file",
            json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
        )]),
    })
}

/// Run a streaming scenario over loopback and through the recording transport; compare both.
async fn differential<F>(reply: Reply, model_at: impl Fn(&str) -> Model, options: StreamOptions, run: F) -> AssistantMessage
where
    F: Fn(Model, StreamOptions) -> AssistantMessageEventStream,
{
    let server = serve_sequence(vec![reply.clone()]).await;
    let model = model_at(&server.base_url);
    let over_wire = run(model.clone(), options.clone()).result().await;
    let wire_requests = server.requests();
    assert_eq!(wire_requests.len(), 1, "{:?}", over_wire.error_message);

    let content_type = reply.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case("content-type")).map(|(_, v)| v.clone());
    let body: Vec<u8> = reply.chunks.iter().flat_map(|(_, c)| c.clone()).collect();
    let transport = RecordingTransport::new(vec![(reply.status, content_type.as_deref().unwrap_or("text/plain"), body)]);
    let through_mock = run(model, with_transport(options, transport.clone())).result().await;
    let recorded = transport.requests();
    assert_eq!(recorded.len(), 1);
    assert_same_request(&recorded[0], &wire_requests[0], &server.base_url);
    assert_eq!(comparable(&through_mock), comparable(&over_wire));
    through_mock
}

// ---------------------------------------------------------------------------
// HTTP API families
// ---------------------------------------------------------------------------

#[tokio::test]
async fn anthropic_messages_goes_through_the_transport() {
    let events: String = [
        ("message_start", json!({"type": "message_start", "message": {"id": "msg_1", "usage": {"input_tokens": 3, "output_tokens": 0}}})),
        ("content_block_start", json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}})),
        ("content_block_delta", json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hé"}})),
        ("content_block_stop", json!({"type": "content_block_stop", "index": 0})),
        ("message_delta", json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 2}})),
        ("message_stop", json!({"type": "message_stop"})),
    ]
    .iter()
    .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
    .collect();
    let model_at = |base: &str| {
        let mut model = get_builtin_model("anthropic", "claude-sonnet-4-5").unwrap();
        model.base_url = base.to_string();
        model
    };
    let message = differential(Reply::sse(events), model_at, key("sk-ant"), |model, options| {
        anthropic_messages::stream(&model, &context(), options)
    })
    .await;
    assert_eq!(message.stop_reason, StopReason::Stop, "{:?}", message.error_message);
    assert_eq!(message.text(), "Hé");
}

fn chat_chunks() -> String {
    let chunk = |delta: Value, finish: Option<&str>| {
        json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "model": "m", "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
    };
    let mut body: String = [chunk(json!({"role": "assistant", "content": "Hel"}), None), chunk(json!({"content": "lo"}), None), chunk(json!({}), Some("stop"))]
        .iter()
        .map(|c| format!("data: {c}\n\n"))
        .collect();
    body.push_str("data: [DONE]\n\n");
    body
}

#[tokio::test]
async fn openai_completions_goes_through_the_transport() {
    let model_at = |base: &str| {
        let mut model = get_builtin_model("openai", "gpt-4o-mini").unwrap();
        model.api = "openai-completions".into();
        model.base_url = base.to_string();
        model
    };
    let message = differential(Reply::sse(chat_chunks()), model_at, key("sk-test"), |model, options| {
        openai_completions::stream(&model, &context(), options)
    })
    .await;
    assert_eq!(message.stop_reason, StopReason::Stop, "{:?}", message.error_message);
    assert_eq!(message.text(), "Hello");
}

fn responses_events() -> String {
    data_sse(&[
        json!({ "type": "response.created", "response": { "id": "resp_1" } }),
        json!({ "type": "response.output_item.added", "item": { "type": "message", "id": "msg_1", "role": "assistant", "status": "in_progress", "content": [] } }),
        json!({ "type": "response.content_part.added", "part": { "type": "output_text", "text": "" } }),
        json!({ "type": "response.output_text.delta", "delta": "Hello" }),
        json!({ "type": "response.output_item.done", "item": { "type": "message", "id": "msg_1", "role": "assistant", "status": "completed", "content": [{ "type": "output_text", "text": "Hello" }] } }),
        json!({ "type": "response.completed", "response": { "id": "resp_1", "status": "completed", "usage": { "input_tokens": 5, "output_tokens": 3, "total_tokens": 8, "input_tokens_details": { "cached_tokens": 0 } } } }),
    ])
}

#[tokio::test]
async fn openai_responses_goes_through_the_transport() {
    let model_at = |base: &str| {
        let mut model = get_builtin_model("openai", "gpt-5-mini").unwrap();
        model.base_url = base.to_string();
        model
    };
    let message = differential(Reply::sse(responses_events()), model_at, key("sk-test"), |model, options| {
        openai_responses::stream(&model, &context(), options)
    })
    .await;
    assert_eq!(message.stop_reason, StopReason::Stop, "{:?}", message.error_message);
    assert_eq!(message.text(), "Hello");
}

#[tokio::test]
async fn azure_openai_responses_goes_through_the_transport() {
    let model_at = |base: &str| {
        let mut model = get_builtin_model("azure", "gpt-4o-mini").unwrap();
        model.base_url = format!("{base}/openai/v1");
        model
    };
    let message = differential(Reply::sse(responses_events()), model_at, key("azure-key"), |model, options| {
        azure_openai_responses::stream(&model, &context(), options)
    })
    .await;
    assert_eq!(message.stop_reason, StopReason::Stop, "{:?}", message.error_message);
}

#[tokio::test]
async fn mistral_conversations_goes_through_the_transport() {
    let events = [
        json!({ "id": "r", "model": "mistral-large-latest", "choices": [{ "index": 0, "delta": { "content": "Bonjour" } }] }),
        json!({ "id": "r", "model": "mistral-large-latest", "choices": [{ "index": 0, "finish_reason": "stop", "delta": {} }],
                "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 } }),
    ];
    let body: Vec<String> = events.iter().map(|e| format!("data: {e}")).collect();
    let body = format!("{}\r\n\r\ndata: [DONE]\r\n\r\n", body.join("\r\n\r\n"));
    let model_at = |base: &str| {
        let mut model = get_builtin_model("mistral", "mistral-large-latest").unwrap();
        model.base_url = base.to_string();
        model
    };
    let message = differential(Reply::sse(body), model_at, key("mistral-key"), |model, options| {
        mistral_conversations::stream(&model, &context(), options)
    })
    .await;
    assert_eq!(message.stop_reason, StopReason::Stop, "{:?}", message.error_message);
    assert_eq!(message.text(), "Bonjour");
}

#[tokio::test]
async fn pi_messages_goes_through_the_transport() {
    let usage = json!({ "input": 10, "output": 5, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 15,
                        "cost": { "input": 0.1, "output": 0.2, "cacheRead": 0, "cacheWrite": 0, "total": 0.3 } });
    let body = data_sse(&[
        json!({ "type": "start" }),
        json!({ "type": "text_start", "contentIndex": 0 }),
        json!({ "type": "text_delta", "contentIndex": 0, "delta": "Hi" }),
        json!({ "type": "text_end", "contentIndex": 0, "content": "Hi" }),
        json!({ "type": "done", "reason": "stop", "usage": usage, "responseId": "resp_1" }),
    ]);
    let model_at = |base: &str| -> Model {
        serde_json::from_value(json!({
            "id": "auto", "name": "Radius Auto", "api": "pi-messages", "provider": "radius", "baseUrl": format!("{base}/v1"),
            "reasoning": false, "input": ["text"], "cost": { "input": 1, "output": 2, "cacheRead": 0.1, "cacheWrite": 0.2 },
            "contextWindow": 128000, "maxTokens": 16384
        }))
        .unwrap()
    };
    let message = differential(Reply::sse(body), model_at, key("pi-key"), |model, options| {
        pi_messages::stream(&model, &context(), options)
    })
    .await;
    assert_eq!(message.stop_reason, StopReason::Stop, "{:?}", message.error_message);
    assert_eq!(message.text(), "Hi");
}

#[tokio::test]
async fn pi_messages_http_errors_read_through_the_transport() {
    let model_at = |base: &str| -> Model {
        serde_json::from_value(json!({
            "id": "auto", "name": "Radius Auto", "api": "pi-messages", "provider": "radius", "baseUrl": format!("{base}/v1"),
            "reasoning": false, "input": ["text"], "cost": { "input": 1, "output": 2, "cacheRead": 0.1, "cacheWrite": 0.2 },
            "contextWindow": 128000, "maxTokens": 16384
        }))
        .unwrap()
    };
    let reply = Reply::json(401, json!({ "error": { "message": "bad key", "code": "auth" } }).to_string());
    let message = differential(reply, model_at, key("stale"), |model, options| pi_messages::stream(&model, &context(), options)).await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.error_message.as_deref(), Some("401 Unauthorized: bad key (auth)"));
}

fn codex_token() -> String {
    let payload = base64::engine::general_purpose::STANDARD
        .encode(json!({ "https://api.openai.com/auth": { "chatgpt_account_id": "acc_test" } }).to_string());
    format!("aaa.{payload}.bbb")
}

fn codex_model(base_url: &str) -> Model {
    serde_json::from_value(json!({
        "id": "gpt-5.1-codex", "name": "GPT-5.1 Codex", "api": "openai-codex-responses", "provider": "openai-codex",
        "baseUrl": base_url, "reasoning": true, "input": ["text"],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }, "contextWindow": 400000, "maxTokens": 128000
    }))
    .unwrap()
}

#[tokio::test]
async fn openai_codex_sse_goes_through_the_transport() {
    let options = StreamOptions { transport: Some(Transport::Sse), ..key(&codex_token()) };
    let message = differential(Reply::sse(responses_events()), codex_model, options, |model, options| {
        openai_codex_responses::stream(&model, &context(), options)
    })
    .await;
    assert_eq!(message.stop_reason, StopReason::Stop, "{:?}", message.error_message);
    assert_eq!(message.text(), "Hello");
}

#[tokio::test]
async fn google_rejects_a_custom_transport_like_a_custom_fetch() {
    let mut model = get_builtin_model("google", "gemini-2.5-flash").unwrap();
    model.base_url = "http://127.0.0.1:1".into();
    let transport = RecordingTransport::new(vec![]);
    let message = google_generative_ai::stream(&model, &context(), with_transport(key("g"), transport.clone())).result().await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert!(transport.requests().is_empty());
}

#[tokio::test]
async fn classifier_requests_go_through_the_transport() {
    let model_at = |base: &str| -> ClassifierModel {
        serde_json::from_value(json!({
            "type": "classifier", "id": "jev-latest", "name": "Jev", "api": "typesafe-system-one", "provider": "typesafe",
            "baseUrl": format!("{base}/v1/"), "input": ["text"], "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }, "contextWindow": 64000
        }))
        .unwrap()
    };
    let classifier_context: ClassifierContext = serde_json::from_value(json!({
        "state": { "text": "The deployment succeeded." },
        "questions": { "approved": { "type": "bool", "instructions": "Approve?", "criteria": { "true": "Yes", "false": "No" } } }
    }))
    .unwrap();
    let body = json!({ "answers": { "approved": { "type": "noul", "noul": 0.95 } } }).to_string();
    let server = serve_sequence(vec![Reply::json(200, body.clone())]).await;
    let over_wire = typesafe_system_one::classify(&model_at(&server.base_url), &classifier_context, key("secret")).await;
    let transport = RecordingTransport::new(vec![(200, "application/json", body.into_bytes())]);
    let through_mock =
        typesafe_system_one::classify(&model_at(&server.base_url), &classifier_context, with_transport(key("secret"), transport.clone()))
            .await;
    assert_same_request(&transport.requests()[0], &server.requests()[0], &server.base_url);
    assert_eq!(through_mock.answers, over_wire.answers);
    assert_eq!(through_mock.answers["approved"], ClassifierAnswer::Bool { probability: 0.95 });
}

#[tokio::test]
async fn image_generation_goes_through_the_transport() {
    let model_at = |base: &str| -> ImageModel {
        serde_json::from_value(json!({
            "type": "image", "id": "google/gemini-3.1-flash-image-preview", "name": "img", "api": "openrouter-images", "provider": "openrouter",
            "baseUrl": base, "input": ["text"], "output": ["text", "image"], "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }
        }))
        .unwrap()
    };
    let images_context = ImagesContext { input: vec![UserContentBlock::text("Generate a dog")] };
    let body = json!({ "id": "img-1", "choices": [{ "message": { "content": "Here.", "images": [{ "image_url": "data:image/png;base64,ZmFrZQ==" }] } }] })
        .to_string();
    let server = serve_sequence(vec![Reply::json(200, body.clone())]).await;
    let options = StreamOptions { api_key: Some("k".into()), ..Default::default() };
    let over_wire = openrouter_images::generate_images(&model_at(&server.base_url), &images_context, options.clone()).await;
    let transport = RecordingTransport::new(vec![(200, "application/json", body.into_bytes())]);
    let through_mock =
        openrouter_images::generate_images(&model_at(&server.base_url), &images_context, with_transport(options, transport.clone())).await;
    assert_same_request(&transport.requests()[0], &server.requests()[0], &server.base_url);
    assert_eq!(through_mock.output, over_wire.output);
    assert_eq!(through_mock.output.len(), 2, "{:?}", through_mock.error_message);
}

#[tokio::test]
async fn transport_failures_keep_fetch_failed_wording() {
    struct Refusing;
    impl HttpTransport for Refusing {
        fn send(&self, _request: HttpRequest) -> BoxFuture<'static, Result<HttpResponse, TransportError>> {
            Box::pin(async {
                Err(TransportError::Network(pi_ai::utils::http::NetworkError {
                    code: Some("ECONNREFUSED"),
                    message: "connect ECONNREFUSED 10.0.0.1:443".into(),
                }))
            })
        }
    }
    let mut model = get_builtin_model("mistral", "mistral-large-latest").unwrap();
    model.base_url = "http://10.0.0.1".into();
    let mut options = key("k");
    options.http_transport = Some(Arc::new(Refusing));
    let message = mistral_conversations::stream(&model, &context(), options).result().await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.error_message.as_deref(), Some("fetch failed"));
}

// ---------------------------------------------------------------------------
// Codex WebSocket
// ---------------------------------------------------------------------------

/// A WebSocket transport whose connection answers every `response.create` with `events`.
struct ScriptedWsTransport {
    handshakes: Mutex<Vec<WsConnectRequest>>,
    sent: Arc<Mutex<Vec<Value>>>,
    events: Vec<Value>,
}

struct ScriptedConnection {
    sent: Arc<Mutex<Vec<Value>>>,
    events: Vec<Value>,
    inbox: VecDeque<WsMessage>,
}

impl Stream for ScriptedConnection {
    type Item = Result<WsMessage, WsError>;
    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        match self.inbox.pop_front() {
            Some(message) => Poll::Ready(Some(Ok(message))),
            // Nothing scripted yet: the test sends before it reads, so this is never polled empty.
            None => Poll::Pending,
        }
    }
}

impl Sink<WsMessage> for ScriptedConnection {
    type Error = WsError;
    fn poll_ready(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<Result<(), WsError>> {
        Poll::Ready(Ok(()))
    }
    fn start_send(mut self: Pin<&mut Self>, item: WsMessage) -> Result<(), WsError> {
        if let WsMessage::Text(text) = item {
            self.sent.lock().push(serde_json::from_str(&text).unwrap());
            let replies: Vec<WsMessage> = self.events.iter().map(|e| WsMessage::Text(e.to_string())).collect();
            self.inbox.extend(replies);
        }
        Ok(())
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<Result<(), WsError>> {
        Poll::Ready(Ok(()))
    }
    fn poll_close(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<Result<(), WsError>> {
        Poll::Ready(Ok(()))
    }
}

impl WsTransport for ScriptedWsTransport {
    fn connect(&self, request: WsConnectRequest) -> BoxFuture<'static, anyhow::Result<BoxWsConnection>> {
        self.handshakes.lock().push(request);
        let connection = ScriptedConnection { sent: self.sent.clone(), events: self.events.clone(), inbox: VecDeque::new() };
        Box::pin(async move { Ok(Box::new(connection) as BoxWsConnection) })
    }
}

#[tokio::test]
async fn openai_codex_websocket_goes_through_the_ws_transport() {
    let mut events: Vec<Value> = serde_json::from_str::<Vec<Value>>(&format!(
        "[{}]",
        responses_events().trim().split("\n\n").map(|e| e.trim_start_matches("data: ")).collect::<Vec<_>>().join(",")
    ))
    .unwrap();
    events.retain(|e| e["type"] != "response.created");
    let scripted = events.clone();
    let server = serve_codex(Arc::new(|_| Reply::text(500, "unexpected fetch")), Arc::new(move |_, _| {
        scripted.iter().map(|e| WsAction::Send(e.to_string())).collect()
    }), 0)
    .await;
    let options = StreamOptions { transport: Some(Transport::Websocket), ..key(&codex_token()) };
    let model = codex_model(&server.base_url);
    let over_wire = openai_codex_responses::stream(&model, &context(), options.clone()).result().await;
    assert_eq!(over_wire.stop_reason, StopReason::Stop, "{:?}", over_wire.error_message);
    let wire = server.connections();

    let transport = Arc::new(ScriptedWsTransport { handshakes: Mutex::default(), sent: Arc::default(), events });
    let mut mocked = options;
    mocked.session_id = Some("session-2".into());
    mocked.ws_transport = Some(transport.clone());
    let through_mock = openai_codex_responses::stream(&model, &context(), mocked).result().await;
    assert_eq!(comparable(&through_mock), comparable(&over_wire));

    let handshakes = transport.handshakes.lock().clone();
    assert_eq!(handshakes.len(), 1);
    assert!(handshakes[0].url.starts_with("ws://"), "{}", handshakes[0].url);
    // Every handshake header the provider set reached the server (the session differs).
    for (name, value) in &handshakes[0].headers {
        let on_wire = wire[0].header(name).unwrap_or_else(|| panic!("{name} missing on the wire"));
        if !value.contains("session-2") {
            assert_eq!(on_wire, value, "{name}");
        }
    }
    let sent = transport.sent.lock().clone();
    let mut expected = wire[0].messages[0].clone();
    let mut actual = sent[0].clone();
    for frame in [&mut expected, &mut actual] {
        frame.as_object_mut().unwrap().remove("prompt_cache_key");
    }
    assert_eq!(actual, expected, "response.create frame");
}

// ---------------------------------------------------------------------------
// Response helpers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn response_helpers_read_like_fetch() {
    let mut headers = HeaderMap::new();
    headers.insert("content-type", "application/json".parse().unwrap());
    let response = HttpResponse::from_bytes(200, headers.clone(), &b"\xEF\xBB\xBF{\"a\":1}"[..]);
    assert!(response.is_success());
    assert_eq!(response.status_text, "OK");
    assert_eq!(response.text().await.unwrap(), "{\"a\":1}");
    let json: Value = HttpResponse::from_bytes(200, headers.clone(), &b"{\"a\":1}"[..]).json().await.unwrap();
    assert_eq!(json, json!({ "a": 1 }));
    let error = HttpResponse::from_bytes(200, headers, &b"nope"[..]).json::<Value>().await.unwrap_err();
    assert_eq!(error.to_string(), "error decoding response body");
    assert!(!error.is_timeout());
}
