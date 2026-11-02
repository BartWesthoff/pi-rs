//! Streaming-path cost for local `openai-completions` servers (llama.cpp and friends).
//!
//! Every scenario streams a canned Chat Completions SSE body from a loopback mock server, so no
//! model server is contacted. Two layers are measured:
//!
//! - `provider`: `pi_ai::api::openai_completions::stream_simple`, draining every event.
//! - `session`: a full [`AgentSession`] built by `create_agent_session` (agent loop, session
//!   events, extension runner) with one subscribed listener, as print and RPC mode run it.
//!
//! Scenarios: 2k/8k/16k-token text answers (one ~4-byte token per SSE chunk) and a 200 KB `write`
//! tool call whose JSON arguments arrive in 3-byte deltas (followed, in the `session` layer, by the
//! real `write` tool and a one-token follow-up answer).
//!
//! Reported per run: wall time, process CPU time (user + system, all threads including the mock
//! server), bytes allocated, allocation count, and peak live heap above the pre-run baseline.
//!
//! Environment:
//! - `PI_STREAM_BENCH_QUICK=1`: scale every scenario down by 8× (smoke run).
//! - `PI_STREAM_BENCH_ONLY=text-2k,write-200k`: run only the named scenarios.
//! - `PI_STREAM_BENCH_LAYERS=provider|session`: run one layer.
//! - `PI_STREAM_BENCH_ENFORCE=1`: exit non-zero when a full-size run exceeds its budget.
//! - `PI_STREAM_BENCH_JSON=<path>`: also write the results as JSON.
//!
//! Run with `cargo bench -p pi-coding-agent --bench stream_local`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use pi_ai::auth::types::{ApiKeyCredential, Credential};
use pi_ai::normalize_context;
use pi_ai::types::{Context, Message, Model, StopReason, StreamOptions, Tool, UserMessage};
use pi_coding_agent::core::agent_session::{AgentSessionEvent, AgentSessionEventListener};
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use pi_coding_agent::core::resource_loader::{
    DefaultResourceLoader, DefaultResourceLoaderOptions, ResourceLoader,
};
use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::settings_manager::SettingsManager;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// ─── Allocation accounting ───

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static ALLOCATED: AtomicU64 = AtomicU64::new(0);
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

/// Wraps the system allocator and counts live, peak and cumulative heap bytes.
struct Counting;

impl Counting {
    fn grow(bytes: usize) {
        let now = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
        PEAK.fetch_max(now, Ordering::Relaxed);
        ALLOCATED.fetch_add(bytes as u64, Ordering::Relaxed);
    }
}

// SAFETY: every method forwards to `System` with the caller's arguments unchanged; the counters
// are plain atomics and never allocate.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds `GlobalAlloc::alloc`'s contract.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            Self::grow(layout.size());
        }
        pointer
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds `GlobalAlloc::alloc_zeroed`'s contract.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            Self::grow(layout.size());
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged; the caller upholds `GlobalAlloc::dealloc`'s contract.
        unsafe { System.dealloc(pointer, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds `GlobalAlloc::realloc`'s contract.
        let moved = unsafe { System.realloc(pointer, layout, size) };
        if !moved.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            if size >= layout.size() {
                Self::grow(size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - size, Ordering::Relaxed);
            }
        }
        moved
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Process CPU time (user + system, all threads).
fn cpu_time() -> Duration {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: `getrusage` fills the struct it is handed; RUSAGE_SELF is always valid.
    let usage = unsafe {
        libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr());
        usage.assume_init()
    };
    let micros = |t: libc::timeval| t.tv_sec as u64 * 1_000_000 + t.tv_usec as u64;
    Duration::from_micros(micros(usage.ru_utime) + micros(usage.ru_stime))
}

// ─── Scenarios ───

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A text answer of `size` tokens, one ~4-byte token per chunk.
    Text,
    /// A `write` tool call whose `content` is `size` bytes, arguments in 3-byte deltas.
    Write,
}

#[derive(Clone, Copy)]
struct Scenario {
    name: &'static str,
    kind: Kind,
    size: usize,
    /// Session-layer CPU budget at full size.
    cpu_budget: Duration,
    /// Session-layer peak live heap budget in bytes.
    peak_budget: usize,
}

/// Budgets for the session layer, about 2x the release-build numbers measured when they were set
/// (recorded in PORTING-STATUS.md). A run is over budget when either limit is exceeded; enforced
/// only with `PI_STREAM_BENCH_ENFORCE=1`.
const SCENARIOS: [Scenario; 4] = [
    Scenario {
        name: "text-2k",
        kind: Kind::Text,
        size: 2_000,
        cpu_budget: Duration::from_millis(50),
        peak_budget: 8 << 20,
    },
    Scenario {
        name: "text-8k",
        kind: Kind::Text,
        size: 8_000,
        cpu_budget: Duration::from_millis(175),
        peak_budget: 24 << 20,
    },
    Scenario {
        name: "text-16k",
        kind: Kind::Text,
        size: 16_000,
        cpu_budget: Duration::from_millis(300),
        peak_budget: 56 << 20,
    },
    Scenario {
        name: "write-200k",
        kind: Kind::Write,
        size: 200_000,
        cpu_budget: Duration::from_millis(1_300),
        peak_budget: 16 << 20,
    },
];

/// Bytes of a ~4-byte token sequence: words with occasional newlines and Markdown.
fn token(index: usize) -> &'static str {
    const TOKENS: [&str; 16] = [
        "The ", "cat ", "sat ", "on ", "the ", "mat.", "\n", "Let ", "us ", "`fn` ", "loop ",
        "over ", "**x** ", "and ", "y, ", "then\n",
    ];
    TOKENS[(index * 7 + index / 16) % TOKENS.len()]
}

/// 200 KB of code-like file content (with quotes, backslashes and newlines to escape).
fn file_content(bytes: usize) -> String {
    let mut out = String::with_capacity(bytes + 64);
    let mut line = 0usize;
    while out.len() < bytes {
        let _ = writeln!(
            out,
            "    let value_{line} = format!(\"{{}}\\t{{}}\", items[{line}], \"q\\\"uote\");"
        );
        line += 1;
    }
    out.truncate(bytes);
    out
}

fn chunk(delta: serde_json::Value, finish: Option<&str>) -> String {
    let value = json!({
        "id": "chatcmpl-bench",
        "object": "chat.completion.chunk",
        "model": "bench-model",
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
    });
    format!("data: {value}\n\n")
}

fn usage_chunk(completion_tokens: usize) -> String {
    let value = json!({
        "id": "chatcmpl-bench",
        "object": "chat.completion.chunk",
        "model": "bench-model",
        "choices": [],
        "usage": {"prompt_tokens": 100, "completion_tokens": completion_tokens, "total_tokens": 100 + completion_tokens},
    });
    format!("data: {value}\n\n")
}

/// The SSE body for a scenario and the size of the payload it streams.
fn sse_body(kind: Kind, size: usize) -> (String, usize) {
    let mut body = chunk(json!({"role": "assistant", "content": ""}), None);
    let payload = match kind {
        Kind::Text => {
            let mut text = 0usize;
            for index in 0..size {
                let piece = token(index);
                text += piece.len();
                body.push_str(&chunk(json!({"content": piece}), None));
            }
            body.push_str(&chunk(json!({}), Some("stop")));
            body.push_str(&usage_chunk(size));
            text
        }
        Kind::Write => {
            let arguments =
                json!({"path": "bench-output.txt", "content": file_content(size)}).to_string();
            body.push_str(&chunk(
                json!({"tool_calls": [{"index": 0, "id": "call_bench", "type": "function", "function": {"name": "write", "arguments": ""}}]}),
                None,
            ));
            let bytes = arguments.as_bytes();
            for piece in bytes.chunks(3) {
                let piece = std::str::from_utf8(piece).expect("arguments are ASCII");
                body.push_str(&chunk(
                    json!({"tool_calls": [{"index": 0, "function": {"arguments": piece}}]}),
                    None,
                ));
            }
            body.push_str(&chunk(json!({}), Some("tool_calls")));
            body.push_str(&usage_chunk(bytes.len() / 3));
            arguments.len()
        }
    };
    body.push_str("data: [DONE]\n\n");
    (body, payload)
}

/// A short text answer, served after the `write` tool ran.
fn follow_up_body() -> String {
    let mut body = chunk(json!({"role": "assistant", "content": "Done."}), None);
    body.push_str(&chunk(json!({}), Some("stop")));
    body.push_str(&usage_chunk(1));
    body.push_str("data: [DONE]\n\n");
    body
}

// ─── Mock server ───

/// Serves `bodies` in order, one per connection, cycling when exhausted.
async fn serve(bodies: Vec<Arc<String>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let address = listener.local_addr().expect("local address");
    tokio::spawn(async move {
        let mut index = 0usize;
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let body = bodies[index % bodies.len()].clone();
            index += 1;
            tokio::spawn(async move {
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 16 * 1024];
                let header_end = loop {
                    let Ok(read) = socket.read(&mut chunk).await else {
                        return;
                    };
                    if read == 0 {
                        return;
                    }
                    buffer.extend_from_slice(&chunk[..read]);
                    if let Some(position) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                        break position + 4;
                    }
                };
                let head = String::from_utf8_lossy(&buffer[..header_end]).to_ascii_lowercase();
                let length: usize = head
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse().ok())
                    .unwrap_or(0);
                while buffer.len() < header_end + length {
                    match socket.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(read) => buffer.extend_from_slice(&chunk[..read]),
                    }
                }
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(head.as_bytes()).await;
                // Flush in 4 KiB writes, like a server that streams as it generates.
                for piece in body.as_bytes().chunks(4096) {
                    if socket.write_all(piece).await.is_err() {
                        return;
                    }
                }
                let _ = socket.shutdown().await;
            });
        }
    });
    format!("http://{address}/v1")
}

// ─── Probe ───

/// Counter snapshot taken when the measured section starts.
#[derive(Default)]
struct Probe {
    live: usize,
    allocated: u64,
    allocations: u64,
    cpu: Duration,
    started: Option<Instant>,
}

/// Counter deltas over the measured section.
struct Sample {
    wall: Duration,
    cpu: Duration,
    allocated: u64,
    allocations: u64,
    peak: usize,
}

impl Probe {
    fn start(&mut self) {
        self.live = LIVE.load(Ordering::Relaxed);
        PEAK.store(self.live, Ordering::Relaxed);
        self.allocated = ALLOCATED.load(Ordering::Relaxed);
        self.allocations = ALLOCATIONS.load(Ordering::Relaxed);
        self.cpu = cpu_time();
        self.started = Some(Instant::now());
    }

    fn finish(&self) -> Sample {
        let started = self.started.expect("probe started");
        Sample {
            wall: started.elapsed(),
            cpu: cpu_time().saturating_sub(self.cpu),
            allocated: ALLOCATED.load(Ordering::Relaxed) - self.allocated,
            allocations: ALLOCATIONS.load(Ordering::Relaxed) - self.allocations,
            peak: PEAK.load(Ordering::Relaxed).saturating_sub(self.live),
        }
    }
}

// ─── Layers ───

#[derive(Clone, Copy, PartialEq, Eq)]
enum Layer {
    Provider,
    Session,
}

impl Layer {
    fn name(self) -> &'static str {
        match self {
            Layer::Provider => "provider",
            Layer::Session => "session",
        }
    }
}

fn bench_model(base_url: &str) -> Model {
    serde_json::from_value(json!({
        "id": "bench-model",
        "name": "Bench Model",
        "api": "openai-completions",
        "provider": "bench",
        "baseUrl": base_url,
        "reasoning": false,
        "input": ["text"],
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
        "contextWindow": 1_000_000,
        "maxTokens": 100_000,
    }))
    .expect("bench model")
}

/// Streams one response through the provider and returns the stop reason.
async fn run_provider(
    base_url: &str,
    kind: Kind,
    probe: &mut Probe,
) -> (StopReason, usize, Sample) {
    let model = bench_model(base_url);
    let tools = (kind == Kind::Write).then(|| {
        vec![Tool::new(
            "write",
            "Write a file",
            json!({"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}}, "required": ["path", "content"]}),
        )]
    });
    let context = normalize_context(&Context {
        system_prompt: Some("Bench.".into()),
        messages: vec![Message::User(UserMessage::new("go", 1))],
        tools,
    });
    let options = StreamOptions {
        api_key: Some("bench-key".into()),
        ..Default::default()
    };
    probe.start();
    let mut stream = pi_ai::api::openai_completions::stream_simple(&model, &context, options);
    let mut events = 0usize;
    while stream.next_event().await.is_some() {
        events += 1;
    }
    let result = stream.result().await;
    let sample = probe.finish();
    (result.stop_reason, events, sample)
}

struct SessionDirs {
    root: tempfile::TempDir,
}

impl SessionDirs {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(root.path().join("project")).expect("project dir");
        std::fs::create_dir_all(root.path().join("agent")).expect("agent dir");
        Self { root }
    }
    fn cwd(&self) -> String {
        self.root
            .path()
            .join("project")
            .to_string_lossy()
            .into_owned()
    }
    fn agent_dir(&self) -> String {
        self.root
            .path()
            .join("agent")
            .to_string_lossy()
            .into_owned()
    }
}

/// Runs one prompt through a full session and returns the number of session events.
async fn run_session(base_url: &str, kind: Kind, probe: &mut Probe) -> (StopReason, usize, Sample) {
    let dirs = SessionDirs::new();
    let models_json = json!({"providers": {"bench": {
        "baseUrl": base_url,
        "api": "openai-completions",
        "apiKey": "bench-key",
        "models": [{"id": "bench-model", "name": "Bench Model", "contextWindow": 1_000_000, "maxTokens": 100_000}],
    }}});
    std::fs::write(
        format!("{}/models.json", dirs.agent_dir()),
        models_json.to_string(),
    )
    .expect("models.json");
    let mut credentials: IndexMap<String, Credential> = IndexMap::new();
    credentials.insert(
        "bench".into(),
        Credential::ApiKey(ApiKeyCredential {
            key: Some("bench-key".into()),
            env: None,
        }),
    );
    let runtime = ModelRuntime::create(Some(&CreateModelRuntimeOptions {
        credentials: Some(AuthStorage::in_memory(Some(&credentials))),
        models_path: Some(format!("{}/models.json", dirs.agent_dir())),
        allow_model_network: Some(false),
        ..Default::default()
    }))
    .await
    .expect("model runtime");
    let model = runtime
        .get_model("bench", "bench-model")
        .expect("bench model from models.json");
    let settings = SettingsManager::in_memory_json(serde_json::Map::new(), None);
    let resource_loader = Arc::new(DefaultResourceLoader::new(&DefaultResourceLoaderOptions {
        cwd: dirs.cwd(),
        agent_dir: dirs.agent_dir(),
        settings_manager: Some(settings.clone()),
        ..Default::default()
    }));
    resource_loader.reload(None).await.expect("resource loader");
    let tools = match kind {
        Kind::Text => Vec::new(),
        Kind::Write => vec!["write".to_string()],
    };
    let session = create_agent_session(Some(&CreateAgentSessionOptions {
        cwd: Some(dirs.cwd()),
        agent_dir: Some(dirs.agent_dir()),
        model: Some(model),
        model_runtime: Some(runtime),
        tools: Some(tools),
        settings_manager: Some(settings),
        session_manager: Some(
            SessionManager::in_memory(Some(&dirs.cwd()), None, None).expect("session manager"),
        ),
        resource_loader: Some(resource_loader as Arc<dyn ResourceLoader>),
        ..Default::default()
    }))
    .await
    .expect("agent session")
    .session;

    let events = Arc::new(AtomicUsize::new(0));
    let counter = events.clone();
    let listener: AgentSessionEventListener = Arc::new(move |event: AgentSessionEvent| {
        std::hint::black_box(&event);
        counter.fetch_add(1, Ordering::Relaxed);
    });
    let unsubscribe = session.subscribe(&listener);
    probe.start();
    session.prompt("go", None).await.expect("prompt");
    let sample = probe.finish();
    unsubscribe();
    let stop_reason = session
        .messages()
        .iter()
        .rev()
        .find_map(|message| match message {
            pi_agent::types::AgentMessage::Assistant(assistant) => Some(assistant.stop_reason),
            _ => None,
        })
        .unwrap_or(StopReason::Error);
    session.dispose();
    (stop_reason, events.load(Ordering::Relaxed), sample)
}

async fn run_layer(
    layer: Layer,
    base_url: &str,
    kind: Kind,
    probe: &mut Probe,
) -> (StopReason, usize, Sample) {
    match layer {
        Layer::Provider => run_provider(base_url, kind, probe).await,
        Layer::Session => run_session(base_url, kind, probe).await,
    }
}

// ─── Driver ───

struct Measurement {
    scenario: &'static str,
    layer: Layer,
    payload: usize,
    events: usize,
    wall: Duration,
    cpu: Duration,
    allocated: u64,
    allocations: u64,
    peak: usize,
    over_budget: bool,
}

fn env_list(name: &str) -> Option<Vec<String>> {
    let value = std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())?;
    Some(
        value
            .split(',')
            .map(|part| part.trim().to_string())
            .collect(),
    )
}

fn mib(bytes: f64) -> String {
    format!("{:.1} MiB", bytes / 1024.0 / 1024.0)
}

fn main() {
    let quick =
        std::env::var("PI_STREAM_BENCH_QUICK").is_ok_and(|value| !value.is_empty() && value != "0");
    let only = env_list("PI_STREAM_BENCH_ONLY");
    let layers: Vec<Layer> = match std::env::var("PI_STREAM_BENCH_LAYERS").as_deref() {
        Ok("provider") => vec![Layer::Provider],
        Ok("session") => vec![Layer::Session],
        _ => vec![Layer::Provider, Layer::Session],
    };
    let enforce = std::env::var("PI_STREAM_BENCH_ENFORCE").is_ok_and(|value| value == "1");
    // Keep every session away from the user's agent directory (auth, settings, packages).
    let isolated = tempfile::tempdir().expect("isolated agent dir");
    // SAFETY: no other thread exists yet; the runtime is built below.
    unsafe { std::env::set_var("PI_CODING_AGENT_DIR", isolated.path()) };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let mut results = Vec::new();
    for scenario in SCENARIOS {
        if only
            .as_ref()
            .is_some_and(|names| !names.iter().any(|name| name == scenario.name))
        {
            continue;
        }
        let size = if quick {
            scenario.size / 8
        } else {
            scenario.size
        };
        let (body, payload) = sse_body(scenario.kind, size);
        let mut bodies = vec![Arc::new(body)];
        if scenario.kind == Kind::Write {
            bodies.push(Arc::new(follow_up_body()));
        }
        for &layer in &layers {
            // Warm-up on a short answer: lazy statics, HTTP client setup, the extension runtime.
            let warm_up = runtime.block_on(serve(vec![Arc::new(sse_body(Kind::Text, 8).0)]));
            runtime.block_on(run_layer(
                layer,
                &warm_up,
                Kind::Text,
                &mut Probe::default(),
            ));
            let base_url = runtime.block_on(serve(bodies.clone()));
            let mut probe = Probe::default();
            let (stop_reason, events, sample) =
                runtime.block_on(run_layer(layer, &base_url, scenario.kind, &mut probe));
            let Sample {
                wall,
                cpu,
                allocated,
                allocations,
                peak,
            } = sample;
            let expected = match (layer, scenario.kind) {
                (Layer::Provider, Kind::Write) => StopReason::ToolUse,
                _ => StopReason::Stop,
            };
            assert_eq!(
                stop_reason,
                expected,
                "{} {} ended with {stop_reason:?}",
                scenario.name,
                layer.name()
            );
            let over_budget = !quick
                && layer == Layer::Session
                && (cpu > scenario.cpu_budget || peak > scenario.peak_budget);
            results.push(Measurement {
                scenario: scenario.name,
                layer,
                payload,
                events,
                wall,
                cpu,
                allocated,
                allocations,
                peak,
                over_budget,
            });
            let last = results.last().expect("just pushed");
            println!(
                "{:<11} {:<8} payload {:>9} B  events {:>6}  wall {:>9.1?}  cpu {:>9.1?}  allocated {:>11}  allocs {:>9}  peak {:>10}{}",
                last.scenario,
                last.layer.name(),
                last.payload,
                last.events,
                last.wall,
                last.cpu,
                mib(last.allocated as f64),
                last.allocations,
                mib(last.peak as f64),
                if last.over_budget {
                    "  OVER BUDGET"
                } else {
                    ""
                },
            );
        }
    }

    if let Ok(path) = std::env::var("PI_STREAM_BENCH_JSON") {
        let rows: Vec<serde_json::Value> = results
            .iter()
            .map(|m| {
                json!({
                    "scenario": m.scenario,
                    "layer": m.layer.name(),
                    "payloadBytes": m.payload,
                    "events": m.events,
                    "wallMs": m.wall.as_secs_f64() * 1000.0,
                    "cpuMs": m.cpu.as_secs_f64() * 1000.0,
                    "allocatedBytes": m.allocated,
                    "allocations": m.allocations,
                    "peakBytes": m.peak,
                    "overBudget": m.over_budget,
                })
            })
            .collect();
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&rows).expect("serialize results"),
        )
        .unwrap_or_else(|error| panic!("write {path}: {error}"));
    }
    if enforce && results.iter().any(|m| m.over_budget) {
        eprintln!("stream_local: at least one scenario is over budget");
        std::process::exit(1);
    }
}
