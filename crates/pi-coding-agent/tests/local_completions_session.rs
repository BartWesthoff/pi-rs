//! Agent sessions on a local `openai-completions` server (llama.cpp, NInfer), against a mock.
//!
//! - Prefix-cache stability: those servers reuse KV state only for a byte-identical prefix, so
//!   every request of a session must extend the previous one: the request head and every earlier
//!   message are sent byte for byte as before, and the `tools` declaration does not change.
//!   Re-setting the same active tools in another order (as packages calling `setActiveTools` do)
//!   must not rewrite the head (#11495).
//! - Compaction with `compaction.reuseChatPrefix` (pi-rs divergence, LM-19): the summary
//!   request extends the last chat request byte for byte (one instruction message more, the
//!   chat's tools) and falls back to TS Pi's standalone requests when its answer is unusable;
//!   without the setting the summary requests stay TS Pi's standalone requests.
//! - Esc during prefill (before the response head) and while a tool call's arguments stream:
//!   the turn ends aborted, the partial tool call never runs, the session file stays valid JSONL
//!   and the session continues.

#[path = "../../pi-ai/tests/mock_http/mod.rs"]
mod mock_http;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mock_http::{MockServer, Request, Response};
use pi_agent::types::{AgentMessage, ThinkingLevel};
use pi_ai::types::AssistantMessageEvent;
use pi_coding_agent::core::agent_session::{AgentSession, AgentSessionEvent, AgentSessionEventListener};
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
use tempfile::TempDir;

fn chunk(delta: Value, finish: Option<&str>) -> Value {
    json!({"id": "chatcmpl-local", "object": "chat.completion.chunk", "model": "local-model",
           "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
}

fn sse_text(chunks: &[Value]) -> String {
    let mut body = String::new();
    for chunk in chunks {
        body.push_str(&format!("data: {chunk}\n\n"));
    }
    body.push_str("data: [DONE]\n\n");
    body
}

fn text_reply(text: &str) -> Response {
    Response::sse(&sse_text(&[
        chunk(json!({"role": "assistant", "content": null}), None),
        chunk(json!({"content": text}), None),
        chunk(json!({}), Some("stop")),
    ]))
}

/// A temp project and agent dir with one llama.cpp-shaped models.json entry pointing at `url`.
struct LocalSetup {
    dir: TempDir,
    runtime: Arc<ModelRuntime>,
}

impl LocalSetup {
    async fn new(url: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("project")).unwrap();
        std::fs::create_dir_all(dir.path().join("agent")).unwrap();
        std::fs::write(dir.path().join("project/notes.txt"), "alpha\nbeta\n").unwrap();
        // Shaped like a llama.cpp entry of a local models.json; the key is a placeholder.
        let models_path = dir.path().join("agent/models.json");
        let models = json!({
            "providers": {
                "local-llama": {
                    "baseUrl": format!("{url}/v1/"),
                    "api": "openai-completions",
                    "apiKey": "sk-local-placeholder",
                    "models": [{
                        "id": "local-model", "name": "Local model", "reasoning": true, "input": ["text"],
                        "contextWindow": 262144, "maxTokens": 4096,
                        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0}
                    }]
                }
            }
        });
        std::fs::write(&models_path, serde_json::to_string_pretty(&models).unwrap()).unwrap();
        let runtime = ModelRuntime::create(Some(&CreateModelRuntimeOptions {
            credentials: Some(AuthStorage::create(Some(&dir.path().join("agent/auth.json").to_string_lossy()))),
            models_path: Some(models_path.to_string_lossy().into_owned()),
            models_store: Some(Arc::new(InMemoryCodingAgentModelsStore::new())),
            allow_model_network: Some(false),
            ..Default::default()
        }))
        .await
        .unwrap();
        Self { dir, runtime }
    }

    fn cwd(&self) -> PathBuf {
        self.dir.path().join("project")
    }

    fn sessions_dir(&self) -> String {
        self.dir.path().join("sessions").to_string_lossy().into_owned()
    }

    async fn session(&self, session_manager: Arc<SessionManager>) -> Arc<AgentSession> {
        self.session_with_settings(session_manager, serde_json::Map::new()).await
    }

    async fn session_with_settings(
        &self,
        session_manager: Arc<SessionManager>,
        settings: serde_json::Map<String, Value>,
    ) -> Arc<AgentSession> {
        let model = ModelRegistry::new(self.runtime.clone()).find("local-llama", "local-model").expect("models.json model");
        create_agent_session(Some(&CreateAgentSessionOptions {
            cwd: Some(self.cwd().to_string_lossy().into_owned()),
            agent_dir: Some(self.dir.path().join("agent").to_string_lossy().into_owned()),
            model_runtime: Some(self.runtime.clone()),
            model: Some(model),
            thinking_level: Some(ThinkingLevel::Medium),
            settings_manager: Some(SettingsManager::in_memory_json(settings, None)),
            session_manager: Some(session_manager),
            ..Default::default()
        }))
        .await
        .unwrap()
        .session
    }

    async fn in_memory_session(&self) -> Arc<AgentSession> {
        let cwd = self.cwd().to_string_lossy().into_owned();
        self.session(SessionManager::in_memory(Some(&cwd), None, None).unwrap()).await
    }

    async fn persisted_session(&self) -> (Arc<AgentSession>, Arc<SessionManager>) {
        let cwd = self.cwd().to_string_lossy().into_owned();
        let manager = SessionManager::create(&cwd, Some(&self.sessions_dir()), None).unwrap();
        (self.session(manager.clone()).await, manager)
    }
}

fn last_message(session: &AgentSession) -> Value {
    serde_json::to_value(session.messages().last().expect("a message")).unwrap()
}

/// Every line of the session file is a JSON object.
fn assert_valid_jsonl(path: &Path) -> Vec<Value> {
    let text = std::fs::read_to_string(path).unwrap();
    assert!(text.ends_with('\n'), "session file ends mid-line");
    text.lines()
        .enumerate()
        .map(|(index, line)| {
            let value: Value = serde_json::from_str(line).unwrap_or_else(|e| panic!("line {}: {e}: {line}", index + 1));
            assert!(value.is_object(), "line {}: {line}", index + 1);
            value
        })
        .collect()
}

/// Calls `on_event` for each assistant stream event; returns the unsubscribe handle.
fn on_assistant_event(
    session: &AgentSession,
    on_event: impl Fn(&AssistantMessageEvent) + Send + Sync + 'static,
) -> Arc<dyn Fn() + Send + Sync> {
    let listener: AgentSessionEventListener = Arc::new(move |event| {
        if let AgentSessionEvent::MessageUpdate { assistant_message_event, .. } = event {
            on_event(&assistant_message_event);
        }
    });
    session.subscribe(&listener)
}

fn tool_executions(session: &AgentSession) -> (Arc<parking_lot::Mutex<Vec<String>>>, Arc<dyn Fn() + Send + Sync>) {
    let started: Arc<parking_lot::Mutex<Vec<String>>> = Arc::default();
    let sink = started.clone();
    let listener: AgentSessionEventListener = Arc::new(move |event| {
        if let AgentSessionEvent::ToolExecutionStart { tool_name, .. } = event {
            sink.lock().push(tool_name);
        }
    });
    let unsubscribe = session.subscribe(&listener);
    (started, unsubscribe)
}

// ---------------------------------------------------------------------------
// Prefix-cache stability
// ---------------------------------------------------------------------------

const TURNS: usize = 5;

/// llama.cpp-shaped replies: even requests answer a user prompt with reasoning, text and a
/// `read` tool call; odd requests answer the tool result with text.
fn tool_turn_reply(index: usize) -> Response {
    let turn = index / 2;
    let usage = json!({"id": "chatcmpl-local", "choices": [],
                       "usage": {"prompt_tokens": 100 + index, "completion_tokens": 10, "total_tokens": 110 + index}});
    if index.is_multiple_of(2) {
        Response::sse(&sse_text(&[
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
        ]))
    } else {
        Response::sse(&sse_text(&[
            chunk(json!({"role": "assistant", "content": null}), None),
            chunk(json!({"content": format!("Done with turn {turn} — café.")}), None),
            chunk(json!({}), Some("stop")),
            usage,
        ]))
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
    let server = MockServer::start(|_request: &Request, index| tool_turn_reply(index)).await;
    let setup = LocalSetup::new(&server.url).await;
    let session = setup.in_memory_session().await;

    for turn in 0..TURNS {
        if turn == 3 {
            // A package re-applying the same tool set in another order (pi-web-access builds
            // `[...others, ...names, loader]`) must not touch the prompt or the declarations.
            let reversed: Vec<String> = session.get_active_tool_names().into_iter().rev().collect();
            session.set_active_tools_by_name(&reversed);
        }
        session.prompt(&format!("Turn {turn}: summarize notes.txt"), None).await.unwrap();
        let last = last_message(&session);
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

// ---------------------------------------------------------------------------
// Compaction summaries that reuse the chat prefix (LM-19)
// ---------------------------------------------------------------------------

const CHAT_PREFIX_INSTRUCTION: &str = "Stop working on the conversation above.";

/// Chat turns as in [`tool_turn_reply`]; every later request (a summary) gets `summary(index)`.
async fn compacted_session(
    reuse_chat_prefix: Option<bool>,
    summary: impl Fn(usize) -> Response + Send + Sync + 'static,
) -> (MockServer, Vec<String>) {
    let server = MockServer::start(move |_request: &Request, index| {
        if index < TURNS * 2 { tool_turn_reply(index) } else { summary(index) }
    })
    .await;
    let setup = LocalSetup::new(&server.url).await;
    let mut compaction = json!({"keepRecentTokens": 1});
    if let Some(reuse) = reuse_chat_prefix {
        compaction["reuseChatPrefix"] = json!(reuse);
    }
    let settings = json!({"compaction": compaction}).as_object().unwrap().clone();
    let cwd = setup.cwd().to_string_lossy().into_owned();
    let session = setup
        .session_with_settings(SessionManager::in_memory(Some(&cwd), None, None).unwrap(), settings)
        .await;
    for turn in 0..TURNS {
        session.prompt(&format!("Turn {turn}: summarize notes.txt"), None).await.unwrap();
    }
    let result = session.compact(None).await.unwrap();
    session.dispose();
    let bodies: Vec<String> = server.requests().iter().map(Request::text).collect();
    (server, std::iter::once(result.summary).chain(bodies).collect())
}

#[tokio::test]
async fn reuse_chat_prefix_summary_extends_the_chat_request_byte_for_byte() {
    let (_server, results) = compacted_session(Some(true), |index| text_reply(&format!("## Goal\nsummary {index}"))).await;
    let (summary, bodies) = (&results[0], &results[1..]);
    // One summary request, although keepRecentTokens 1 splits the last turn.
    assert_eq!(bodies.len(), TURNS * 2 + 1, "{bodies:#?}");
    let (chat, request) = (&bodies[TURNS * 2 - 1], &bodies[TURNS * 2]);
    // Like the next chat turn: the last chat request's messages byte for byte, then more.
    let prefix = reusable_prefix(chat);
    assert!(request.starts_with(prefix), "the summary request does not extend the chat request\nchat:    {prefix}\nsummary: {request}");
    assert_eq!(tools(request), tools(chat), "the summary request changed the tools declaration");
    let value: Value = serde_json::from_str(request).unwrap();
    let messages = value["messages"].as_array().unwrap();
    let chat_messages: Value = serde_json::from_str(chat).unwrap();
    // The final assistant answer of the last turn, then the instruction.
    assert_eq!(messages.len(), chat_messages["messages"].as_array().unwrap().len() + 2);
    let last = messages.last().unwrap();
    assert_eq!(last["role"], "user", "{last}");
    let instruction = last["content"][0]["text"].as_str().unwrap();
    assert!(instruction.starts_with(CHAT_PREFIX_INSTRUCTION), "{instruction}");
    assert!(instruction.contains("The messages above are a conversation to summarize."), "{instruction}");
    let expected = format!("## Goal\nsummary {}", TURNS * 2);
    assert!(summary.starts_with(&expected), "{summary}");
    assert!(!summary.contains("**Turn Context (split turn):**"), "{summary}");
}

#[tokio::test]
async fn without_reuse_chat_prefix_compaction_sends_the_standalone_ts_request() {
    let (_server, results) = compacted_session(Some(false), |_| text_reply("## Goal\nstandalone")).await;
    let bodies = &results[1..];
    assert_eq!(bodies.len(), TURNS * 2 + 2, "{bodies:#?}");
    for body in &bodies[TURNS * 2..] {
        let value: Value = serde_json::from_str(body).unwrap();
        let messages = value["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2, "{body}");
        assert!(messages[0]["content"].as_str().unwrap().starts_with("You are a context summarization assistant."));
        assert!(!body.contains(CHAT_PREFIX_INSTRUCTION), "{body}");
        assert!(tools(body).is_empty(), "{body}");
    }
}

/// Unset, a local `openai-completions` server is the measured default; elsewhere off.
#[tokio::test]
async fn reuse_chat_prefix_defaults_on_for_loopback_openai_completions() {
    let (_server, results) = compacted_session(None, |index| text_reply(&format!("## Goal\nsummary {index}"))).await;
    let bodies = &results[1..];
    assert_eq!(bodies.len(), TURNS * 2 + 1, "{bodies:#?}");
    assert!(bodies[TURNS * 2].contains(CHAT_PREFIX_INSTRUCTION), "{}", bodies[TURNS * 2]);
}

#[tokio::test]
async fn reuse_chat_prefix_falls_back_to_the_standalone_request_when_the_summary_calls_a_tool() {
    let (_server, results) = compacted_session(Some(true), |index| {
        if index == TURNS * 2 {
            // The chat-prefix history summary answers with a tool call instead of text.
            Response::sse(&sse_text(&[
                chunk(json!({"role": "assistant", "content": null}), None),
                chunk(
                    json!({"tool_calls": [{"index": 0, "id": "call_x", "type": "function",
                                           "function": {"name": "read", "arguments": "{\"path\":\"notes.txt\"}"}}]}),
                    None,
                ),
                chunk(json!({}), Some("tool_calls")),
            ]))
        } else {
            text_reply(&format!("## Goal\nsummary {index}"))
        }
    })
    .await;
    let (summary, bodies) = (&results[0], &results[1..]);
    // The chat-prefix attempt, then TS Pi's history and turn prefix summaries.
    assert_eq!(bodies.len(), TURNS * 2 + 3, "{bodies:#?}");
    assert!(bodies[TURNS * 2].contains(CHAT_PREFIX_INSTRUCTION));
    for body in &bodies[TURNS * 2 + 1..] {
        assert!(!body.contains(CHAT_PREFIX_INSTRUCTION), "{body}");
        assert!(body.contains("You are a context summarization assistant."), "{body}");
    }
    let fallback_index = TURNS * 2 + 1;
    assert!(summary.starts_with(&format!("## Goal\nsummary {fallback_index}")), "{summary}");
    assert!(summary.contains("**Turn Context (split turn):**"), "{summary}");
}

// ---------------------------------------------------------------------------
// Abort
// ---------------------------------------------------------------------------

#[tokio::test]
async fn esc_during_prefill_aborts_without_retrying_and_the_session_continues() {
    let server = MockServer::start(|_request: &Request, index| match index {
        // Still prefilling: the response head would only come after five seconds.
        0 => text_reply("too late").with_delay(5_000),
        _ => text_reply("continued"),
    })
    .await;
    let setup = LocalSetup::new(&server.url).await;
    let (session, manager) = setup.persisted_session().await;

    let started = Instant::now();
    let running = {
        let session = session.clone();
        tokio::spawn(async move { session.prompt("Summarize notes.txt", None).await })
    };
    while server.requests().is_empty() {
        assert!(started.elapsed() < Duration::from_secs(5), "request never arrived");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    session.abort().await.unwrap();
    running.await.unwrap().unwrap();
    assert!(started.elapsed() < Duration::from_secs(4), "abort waited for the response head: {:?}", started.elapsed());

    let last = last_message(&session);
    assert_eq!(last["role"], "assistant");
    assert_eq!(last["stopReason"], "aborted", "{last}");
    assert_eq!(last["errorMessage"], "Request aborted", "{last}");
    assert_eq!(server.requests().len(), 1, "an aborted request is not retried");

    session.prompt("Try again", None).await.unwrap();
    let last = last_message(&session);
    assert_eq!(last["stopReason"], "stop", "{last}");
    assert_eq!(server.requests().len(), 2);
    // The aborted turn is not replayed as an assistant message.
    let retry: Value = server.request(1).json();
    let roles: Vec<&str> = retry["messages"].as_array().unwrap().iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(roles, ["developer", "user", "user"], "{retry}");

    let entries = assert_valid_jsonl(Path::new(&manager.get_session_file().expect("persisted session")));
    assert!(entries.iter().any(|e| e["message"]["stopReason"] == "aborted"));
    session.dispose();
}

#[tokio::test]
async fn esc_while_a_tool_call_streams_never_runs_the_partial_call() {
    let server = MockServer::start(|_request: &Request, index| match index {
        0 => {
            let body = sse_text(&[
                chunk(json!({"role": "assistant", "content": null}), None),
                chunk(
                    json!({"tool_calls": [{"index": 0, "id": "call_a", "type": "function",
                                           "function": {"name": "bash", "arguments": "{\"command\":"}}]}),
                    None,
                ),
                chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": "\"touch marker"}}]}), None),
                chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": ".txt\"}"}}]}), None),
                chunk(json!({}), Some("tool_calls")),
            ]);
            let frames = body.split_inclusive("\n\n").map(|frame| frame.as_bytes().to_vec()).collect();
            Response::chunked(200, "text/event-stream", frames).with_chunk_delay(300)
        }
        _ => text_reply("continued"),
    })
    .await;
    let setup = LocalSetup::new(&server.url).await;
    let (session, manager) = setup.persisted_session().await;
    let (executions, stop_tracking) = tool_executions(&session);

    let (delta_tx, mut delta_rx) = tokio::sync::mpsc::unbounded_channel();
    let unsubscribe = on_assistant_event(&session, move |event| {
        if matches!(event, AssistantMessageEvent::ToolCallDelta { .. }) {
            let _ = delta_tx.send(());
        }
    });
    let running = {
        let session = session.clone();
        tokio::spawn(async move { session.prompt("Create the marker", None).await })
    };
    tokio::time::timeout(Duration::from_secs(5), delta_rx.recv()).await.expect("tool call delta").unwrap();
    session.abort().await.unwrap();
    running.await.unwrap().unwrap();
    unsubscribe();

    let last = last_message(&session);
    assert_eq!(last["stopReason"], "aborted", "{last}");
    assert_eq!(last["errorMessage"], "Request was aborted", "{last}");
    assert_eq!(last["content"][0]["type"], "toolCall", "{last}");
    assert!(executions.lock().is_empty(), "the partial tool call ran: {:?}", executions.lock());
    assert!(!session.messages().iter().any(|m| matches!(m, AgentMessage::ToolResult(_))));
    assert!(!setup.cwd().join("marker.txt").exists());

    session.prompt("Never mind", None).await.unwrap();
    assert_eq!(last_message(&session)["stopReason"], "stop");
    stop_tracking();
    assert!(executions.lock().is_empty());
    // The aborted tool call is not replayed: no tool_calls without matching tool results.
    let retry: Value = server.request(1).json();
    assert!(!retry.to_string().contains("call_a"), "{retry}");

    let entries = assert_valid_jsonl(Path::new(&manager.get_session_file().expect("persisted session")));
    assert!(entries.iter().any(|e| e["message"]["stopReason"] == "aborted"));
    session.dispose();
}
