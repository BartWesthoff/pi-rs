//! Provider HTTP through a JavaScript `fetch` patch (PKGP-11).
//!
//! An extension that replaces `globalThis.fetch` takes over the requests pi makes to providers:
//! the request is marshalled to the extension thread, the current `fetch` is called, and the
//! response streams back. Without a patch provider requests never enter JavaScript.

#[path = "../../pi-ai/tests/mock_http/mod.rs"]
mod mock_http;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use mock_http::{MockServer, Request, Response};
use parking_lot::Mutex;
use pi_agent::types::{AgentMessage, ThinkingLevel};
use pi_ai::transport::{HttpRequest, HttpTransport, ReqwestTransport};
use pi_ai::types::{AssistantMessageEvent, StopReason};
use pi_coding_agent::core::agent_session::{
    AgentSession, AgentSessionEvent, AgentSessionEventListener,
};
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::extensions::js::JsExtensionHost;
use pi_coding_agent::core::model_registry::ModelRegistry;
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use pi_coding_agent::core::models_store::InMemoryCodingAgentModelsStore;
use pi_coding_agent::core::resource_loader::{
    DefaultResourceLoader, DefaultResourceLoaderOptions, ResourceLoader,
};
use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::settings_manager::SettingsManager;
use serde_json::json;
use tempfile::TempDir;

/// Extensions share one process-global runtime cache, so these tests run one at a time.
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn text_reply(text: &str) -> Response {
    let body = [
        json!({"id": "c", "object": "chat.completion.chunk", "model": "m", "choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": null}]}),
        json!({"id": "c", "object": "chat.completion.chunk", "model": "m", "choices": [{"index": 0, "delta": {"content": text}, "finish_reason": null}]}),
        json!({"id": "c", "object": "chat.completion.chunk", "model": "m", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}),
    ]
    .iter()
    .map(|chunk| format!("data: {chunk}\n\n"))
    .collect::<String>();
    Response::sse(&format!("{body}data: [DONE]\n\n"))
}

/// A project with a mock `openai-completions` model and one extension loaded from `extension.ts`.
struct Setup {
    _dir: TempDir,
    server: MockServer,
    session: Arc<AgentSession>,
    cwd: String,
    log: String,
}

impl Setup {
    async fn new<F>(server: MockServer, extension: &str, prepare: F) -> Setup
    where
        F: FnOnce(&Path),
    {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let agent = dir.path().join("agent");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&agent).unwrap();
        prepare(&project);
        let models = json!({"providers": {"mock": {
            "baseUrl": format!("{}/v1/", server.url),
            "api": "openai-completions",
            "apiKey": "sk-mock",
            "models": [{"id": "mock-model", "name": "Mock", "input": ["text"],
                        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
                        "contextWindow": 100000, "maxTokens": 1000}],
        }}});
        std::fs::write(agent.join("models.json"), models.to_string()).unwrap();
        let log = dir.path().join("fetch.log").display().to_string();
        let extension_path = project.join("extension.ts");
        std::fs::write(
            &extension_path,
            with_logging(extension)
                .replace("@LOG@", &log)
                .replace("@URL@", &server.url),
        )
        .unwrap();
        let cwd = project.display().to_string();
        let runtime = ModelRuntime::create(Some(&CreateModelRuntimeOptions {
            credentials: Some(AuthStorage::create(Some(
                &agent.join("auth.json").display().to_string(),
            ))),
            models_path: Some(agent.join("models.json").display().to_string()),
            models_store: Some(Arc::new(InMemoryCodingAgentModelsStore::new())),
            allow_model_network: Some(false),
            ..Default::default()
        }))
        .await
        .unwrap();
        let loader = Arc::new(DefaultResourceLoader::new(&DefaultResourceLoaderOptions {
            cwd: cwd.clone(),
            agent_dir: agent.display().to_string(),
            additional_extension_paths: Some(vec![extension_path.display().to_string()]),
            no_skills: Some(true),
            no_prompt_templates: Some(true),
            no_context_files: Some(true),
            ..Default::default()
        }));
        loader.reload(None).await.unwrap();
        assert!(
            loader.get_extensions().errors.is_empty(),
            "{:#?}",
            loader.get_extensions().errors
        );
        let model = ModelRegistry::new(runtime.clone())
            .find("mock", "mock-model")
            .expect("mock model");
        let session = create_agent_session(Some(&CreateAgentSessionOptions {
            cwd: Some(cwd.clone()),
            agent_dir: Some(agent.display().to_string()),
            model_runtime: Some(runtime.clone()),
            model: Some(model),
            thinking_level: Some(ThinkingLevel::Low),
            resource_loader: Some(loader),
            settings_manager: Some(SettingsManager::in_memory_json(
                serde_json::Map::new(),
                None,
            )),
            session_manager: Some(SessionManager::in_memory(Some(&cwd), None, None).unwrap()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .session;
        session.bind_extensions(&Default::default()).await.unwrap();
        Setup {
            _dir: dir,
            server,
            session,
            cwd,
            log,
        }
    }

    fn logged(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    async fn prompt(&self, text: &str) {
        self.session.prompt(text, None).await.unwrap();
    }

    fn last_assistant(&self) -> pi_ai::types::AssistantMessage {
        self.session
            .messages()
            .iter()
            .rev()
            .find_map(|m| m.as_assistant().cloned())
            .expect("an assistant message")
    }

    fn text(&self) -> String {
        self.last_assistant()
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect()
    }
}

/// Prepend the file-backed `__pi_log` the patches write to; `@LOG@` becomes the log path.
fn with_logging(extension: &str) -> String {
    format!(
        "import {{ appendFileSync }} from \"node:fs\";\nglobalThis.__pi_log = (line) => appendFileSync(\"@LOG@\", line + \"\\n\");\n{extension}"
    )
}

const LOGGING_PATCH: &str = r#"
export default function (pi) {
    const native = globalThis.fetch;
    globalThis.fetch = (url, init) => {
        const body = init?.body ? new TextDecoder().decode(init.body) : "";
        __pi_log(`${url} ${init?.method ?? "GET"} ${body}`.trim());
        return native(url, init);
    };
}
"#;

#[tokio::test]
async fn patched_fetch_serves_provider_requests() {
    let _guard = LOCK.lock().await;
    let server = MockServer::start(|_request: &Request, _index| text_reply("patched reply")).await;
    let setup = Setup::new(server, LOGGING_PATCH, |_| {}).await;
    assert!(
        JsExtensionHost::fetch_transport(&setup.cwd).is_some(),
        "a patched runtime offered no JS transport"
    );
    setup.prompt("hello").await;
    assert_eq!(setup.text(), "patched reply");
    let logged = setup.logged();
    assert!(
        logged.contains("/v1/chat/completions"),
        "the patch did not see the request: {logged}"
    );
    assert!(
        logged.contains("\"role\":\"user\""),
        "the patch did not see the request body: {logged}"
    );
    assert!(
        !setup.server.requests().is_empty(),
        "the request did not reach the server"
    );
}

#[tokio::test]
async fn unpatched_fetch_never_calls_javascript() {
    let _guard = LOCK.lock().await;
    let server = MockServer::start(|_request: &Request, _index| text_reply("native reply")).await;
    let setup = Setup::new(server, "export default function (pi) {}", |_| {}).await;
    assert!(
        JsExtensionHost::fetch_transport(&setup.cwd).is_none(),
        "an unpatched runtime offered a JS transport"
    );
    setup.prompt("hello").await;
    assert_eq!(setup.text(), "native reply");
    assert!(
        setup.logged().is_empty(),
        "an unpatched provider request entered JavaScript"
    );
}

#[tokio::test]
async fn throwing_patch_fails_the_request_with_its_message() {
    let _guard = LOCK.lock().await;
    let server = MockServer::start(|_request: &Request, _index| text_reply("never")).await;
    let setup =
        Setup::new(server, "export default function (pi) { globalThis.fetch = () => { throw new Error(\"patched fetch exploded\"); }; }", |_| {})
            .await;
    setup.prompt("hello").await;
    let message = setup.last_assistant();
    assert_eq!(message.stop_reason, StopReason::Error);
    let error = message.error_message.unwrap_or_default();
    assert!(
        error.contains("patched fetch exploded"),
        "unexpected error: {error}"
    );
    assert!(
        setup.server.requests().is_empty(),
        "a request reached the server through the throwing patch"
    );
}

#[tokio::test]
async fn after_provider_response_sees_status_and_headers_through_a_patch() {
    let _guard = LOCK.lock().await;
    let server = MockServer::start(|_request: &Request, _index| {
        text_reply("ok").with_header("x-mock-header", "seen")
    })
    .await;
    let patch = r#"
export default function (pi) {
    pi.on("after_provider_response", (event) => {
        __pi_log(`response ${event.status} ${event.headers["x-mock-header"] ?? ""}`);
    });
    const native = globalThis.fetch;
    globalThis.fetch = (url, init) => native(url, init);
}
"#;
    let setup = Setup::new(server, patch, |_| {}).await;
    setup.prompt("hello").await;
    let logged = setup.logged();
    assert!(
        logged.contains("response 200"),
        "no response event: {logged}"
    );
    assert!(
        logged.contains("seen"),
        "response headers missing from the event: {logged}"
    );
}

#[tokio::test]
async fn undici_fetch_is_not_a_patch() {
    let _guard = LOCK.lock().await;
    let server = MockServer::start(|_request: &Request, _index| text_reply("undici reply")).await;
    // A package named `undici` re-exporting the native fetch: assigning that is not a patch.
    let setup = Setup::new(
        server,
        "import { fetch } from \"undici\";\nexport default function (pi) { globalThis.fetch = fetch; }",
        |project| {
            let undici = project.join("node_modules/undici");
            std::fs::create_dir_all(&undici).unwrap();
            std::fs::write(undici.join("package.json"), r#"{"name":"undici","type":"module","main":"index.js"}"#).unwrap();
            std::fs::write(undici.join("index.js"), "export const fetch = globalThis.fetch;\n").unwrap();
        },
    )
    .await;
    assert!(
        JsExtensionHost::fetch_transport(&setup.cwd).is_none(),
        "undici's fetch counted as a patch"
    );
    setup.prompt("hello").await;
    assert_eq!(setup.text(), "undici reply");
}

#[tokio::test]
async fn aborting_a_request_reaches_the_patch_signal() {
    let _guard = LOCK.lock().await;
    let server =
        MockServer::start(|_request: &Request, _index| text_reply("slow").with_delay(5_000)).await;
    let patch = r#"
export default function (pi) {
    const native = globalThis.fetch;
    globalThis.fetch = (url, init) => {
        init.signal?.addEventListener("abort", () => __pi_log("aborted"));
        return native(url, init);
    };
}
"#;
    let setup = Setup::new(server, patch, |_| {}).await;
    let session = setup.session.clone();
    let run = tokio::spawn(async move { session.prompt("hello", None).await });
    tokio::time::sleep(Duration::from_millis(500)).await;
    setup.session.abort().await.unwrap();
    let _ = run.await;
    assert!(
        setup.logged().contains("aborted"),
        "the patch never saw the abort: {}",
        setup.logged()
    );
    assert_eq!(setup.last_assistant().stop_reason, StopReason::Aborted);
}

#[tokio::test]
async fn options_fetch_from_extension_js_carries_provider_traffic() {
    let _guard = LOCK.lock().await;
    let server =
        MockServer::start(|_request: &Request, _index| text_reply("via options.fetch")).await;
    // An extension calling pi-ai from JavaScript with `options.fetch`: that function serves the
    // request, not the native client.
    let extension = r#"
import { complete } from "@earendil-works/pi-ai";
export default function (pi) {
    const native = globalThis.fetch;
    complete(
        { provider: "mock", id: "mock-model", name: "Mock", api: "openai-completions", baseUrl: "@URL@/v1/",
          input: ["text"], cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 100000, maxTokens: 1000 },
        { messages: [{ role: "user", content: "probe", timestamp: 0 }] },
        { apiKey: "sk-mock", fetch: (url, init) => { __pi_log("options.fetch " + url); return native(url, init); } },
    ).then((result) => __pi_log("complete " + result.content.map((c) => c.type === "text" ? c.text : "").join("")));
}
"#;
    let setup = Setup::new(server, extension, |_| {}).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !setup.logged().contains("complete ") && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let logged = setup.logged();
    assert!(
        logged.contains("options.fetch"),
        "options.fetch was not used for the request: {logged}"
    );
    assert!(
        logged.contains("complete via options.fetch"),
        "the completion did not come back: {logged}"
    );
}

/// A pass-through patch must stay within the transport gates: first text delta no more than 5 ms
/// later, and response-body throughput at least 90% of native. The full-turn time is diagnostic.
#[tokio::test]
async fn pass_through_patch_keeps_latency() {
    let _guard = LOCK.lock().await;
    let big = "x".repeat(500_000);
    let mut chunks = vec![
        json!({"id":"c","object":"chat.completion.chunk","model":"m","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}),
    ];
    for part in big.as_bytes().chunks(5_000) {
        chunks.push(json!({"id":"c","object":"chat.completion.chunk","model":"m","choices":[{"index":0,"delta":{"content":std::str::from_utf8(part).unwrap()},"finish_reason":null}]}));
    }
    chunks.push(json!({"id":"c","object":"chat.completion.chunk","model":"m","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}));
    let body_chunks = chunks
        .iter()
        .map(|chunk| format!("data: {chunk}\n\n").into_bytes())
        .chain(std::iter::once(b"data: [DONE]\n\n".to_vec()))
        .collect();
    let response = Response::chunked(200, "text/event-stream", body_chunks).with_chunk_delay(1);
    let patched_response = response.clone();
    let request_arrivals = Arc::new(Mutex::new(Vec::new()));
    let arrivals = request_arrivals.clone();
    let server = MockServer::start(move |_request: &Request, _index| {
        arrivals.lock().push(Instant::now());
        patched_response.clone()
    })
    .await;
    let url = server.url.clone();

    fn server_at(url: &str) -> MockServer {
        MockServer {
            url: url.to_string(),
            requests: Arc::default(),
        }
    }

    let pass_through_patch = r#"
export default function (pi) {
    const native = globalThis.fetch;
    globalThis.fetch = (url, init) => native(url, init);
}
"#;
    let native = Setup::new(server_at(&url), "export default function (pi) {}", |_| {}).await;
    let patched = Setup::new(server, pass_through_patch, |_| {}).await;
    let patched_transport = JsExtensionHost::fetch_transport(&patched.cwd).unwrap();

    async fn transport_stats(
        transport: &dyn HttpTransport,
        url: &str,
        arrivals: &Arc<Mutex<Vec<Instant>>>,
    ) -> (Duration, Duration, Duration, usize) {
        let started = Instant::now();
        let arrival_index = arrivals.lock().len();
        let mut response = transport
            .send(HttpRequest::post(url).body(br#"{"probe":true}"#.to_vec()))
            .await
            .unwrap();
        let mut body_bytes = 0;
        let first_chunk = response.body.next().await.unwrap().unwrap();
        body_bytes += first_chunk.len();
        let first_byte = started.elapsed();
        let body_started = Instant::now();
        while let Some(chunk) = response.body.next().await {
            body_bytes += chunk.unwrap().len();
        }
        let request_arrival = arrivals.lock()[arrival_index] - started;
        (
            first_byte,
            request_arrival,
            body_started.elapsed(),
            body_bytes,
        )
    }
    let native_transport = ReqwestTransport::for_target(&url, None).unwrap();
    // Let extension setup finish its background work before comparing first-request latency.
    tokio::time::sleep(Duration::from_millis(20)).await;

    async fn measure(
        setup: &Setup,
        arrivals: &Arc<Mutex<Vec<Instant>>>,
    ) -> (Duration, Duration, Duration, Duration) {
        let started = Instant::now();
        let arrival_index = arrivals.lock().len();
        let timing = Arc::new(Mutex::new((None, None)));
        let sink = timing.clone();
        let listener: AgentSessionEventListener = Arc::new(move |event| {
            let mut timing = sink.lock();
            match event {
                AgentSessionEvent::MessageUpdate {
                    assistant_message_event,
                    ..
                } if matches!(&*assistant_message_event, AssistantMessageEvent::TextDelta { delta, .. } if !delta.is_empty()) =>
                {
                    timing.0.get_or_insert_with(Instant::now);
                }
                AgentSessionEvent::MessageEnd {
                    message: AgentMessage::Assistant(_),
                    ..
                } => {
                    timing.1 = Some(Instant::now());
                }
                _ => {}
            }
        });
        let unsubscribe = setup.session.subscribe(&listener);
        setup.prompt("hello").await;
        let full_turn = started.elapsed();
        unsubscribe();
        let (first, end) = *timing.lock();
        let first = first.expect("no first text delta");
        let end = end.expect("no assistant end");
        let request_arrival = arrivals.lock()[arrival_index] - started;
        (first - started, end - first, full_turn, request_arrival)
    }
    let (native_first, native_body, native_turn, native_arrival) =
        measure(&native, &request_arrivals).await;
    let (patched_first, patched_body, patched_turn, patched_arrival) =
        measure(&patched, &request_arrivals).await;
    let native_transport_stats = transport_stats(&native_transport, &url, &request_arrivals).await;
    let patched_transport_stats =
        transport_stats(patched_transport.as_ref(), &url, &request_arrivals).await;
    assert_eq!(
        patched
            .last_assistant()
            .content
            .iter()
            .filter_map(|part| part.as_text().map(|text| text.text.len()))
            .sum::<usize>(),
        big.len()
    );
    let native_rate = big.len() as f64 / native_body.as_secs_f64();
    let patched_rate = big.len() as f64 / patched_body.as_secs_f64();
    let native_transport_rate =
        native_transport_stats.3 as f64 / native_transport_stats.2.as_secs_f64();
    let patched_transport_rate =
        patched_transport_stats.3 as f64 / patched_transport_stats.2.as_secs_f64();
    eprintln!(
        "performance: first-token native={native_first:?} patched={patched_first:?}; request arrival native={native_arrival:?} patched={patched_arrival:?}; provider event-processing rate native={native_rate:.0} B/s patched={patched_rate:.0} B/s; full turn native={native_turn:?} patched={patched_turn:?}; POST transport arrival/first-byte/body throughput native={:?}/{:?}/{:.0} B/s ({} bytes), patched={:?}/{:?}/{:.0} B/s ({} bytes)",
        native_transport_stats.1,
        native_transport_stats.0,
        native_transport_rate,
        native_transport_stats.3,
        patched_transport_stats.1,
        patched_transport_stats.0,
        patched_transport_rate,
        patched_transport_stats.3,
    );
    assert!(
        patched_first <= native_first + Duration::from_millis(5),
        "first-token latency: native {native_first:?}, patched {patched_first:?}"
    );
    assert!(
        patched_transport_rate >= native_transport_rate * 0.90,
        "transport body throughput: native {native_transport_rate:.0} B/s, patched {patched_transport_rate:.0} B/s"
    );
}
