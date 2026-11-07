//! Sandbox, host launcher and observation for the package smoke harness.
//!
//! A [`Sandbox`] is a throwaway `HOME` whose agent dir (`~/.pi/agent`) holds a settings file shaped
//! like the captain's, a `models.json` pointing at `mockllm.py`, and (for fixture scenarios) a
//! copy of the installed package tree from `scripts/package-fixtures.sh`. [`observe`] starts the
//! host twice, in print mode and in RPC mode, and reports the tools the model was offered, the
//! slash commands and every extension-error diagnostic.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Set to run the scenarios that need the fetched package fixtures.
pub const FIXTURES_ENV: &str = "PI_RS_PKG_FIXTURES";
/// Overrides the fixture directory (default `<workspace>/target/pkg-fixtures`).
const FIXTURES_DIR_ENV: &str = "PI_RS_PKG_FIXTURES_DIR";
/// Command line of the host under test (default: this crate's `pi` binary). The golden script sets
/// it to TS Pi 1.4.0 under node.
const HOST_ENV: &str = "PI_RS_PKG_HOST";
/// Set to write observations into the golden file instead of comparing against it.
pub const GOLDEN_WRITE_ENV: &str = "PI_RS_PKG_GOLDEN_WRITE";
/// Per-run timeout in seconds.
const TIMEOUT_ENV: &str = "PI_RS_PKG_TIMEOUT_SECS";
/// How many host processes may run at once (each one loads every package).
const JOBS_ENV: &str = "PI_RS_PKG_JOBS";
/// Set to print each run's stderr (for debugging a scenario).
const VERBOSE_ENV: &str = "PI_RS_PKG_VERBOSE";

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(180);
const DEFAULT_JOBS: usize = 2;
/// Port of the mock model inside the network namespace, which has no other listeners.
const NETNS_MOCK_PORT: u16 = 18080;
const MOCK_MODEL: &str = "mock-model";
const RPC_COMMANDS_ID: &str = "pkg-harness-commands";

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Checked-in inputs: pins, settings, local fixture packages and the golden.
pub fn pins_dir() -> PathBuf {
    manifest_dir().join("tests/fixtures/packages")
}

/// Helpers and mock servers shared by the package scenarios.
pub fn mock_dir() -> PathBuf {
    manifest_dir().join("tests/fixtures/mock")
}

fn golden_path() -> PathBuf {
    pins_dir().join("golden/ts-pi-1.4.0.json")
}

fn timeout() -> Duration {
    std::env::var(TIMEOUT_ENV)
        .ok()
        .and_then(|value| value.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_TIMEOUT)
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// The installed package tree produced by `scripts/package-fixtures.sh`.
pub struct Fixtures {
    /// `<fixtures>/agent`, holding `npm/` and `git/` as laid out under `~/.pi/agent`.
    agent_tree: PathBuf,
}

/// Returns the fixtures when [`FIXTURES_ENV`] is set, `None` (the test is skipped) otherwise.
///
/// # Panics
///
/// When the variable is set but the fixtures are missing or were built from other pins.
pub fn fixtures() -> Option<&'static Fixtures> {
    static FIXTURES: OnceLock<Option<Fixtures>> = OnceLock::new();
    FIXTURES
        .get_or_init(|| {
            std::env::var_os(FIXTURES_ENV)?;
            let dir = std::env::var_os(FIXTURES_DIR_ENV)
                .map(PathBuf::from)
                .unwrap_or_else(|| manifest_dir().join("../../target/pkg-fixtures"));
            verify_fixtures(&dir);
            Some(Fixtures { agent_tree: dir.join("agent") })
        })
        .as_ref()
}

/// Prints the skip notice for a fixture scenario.
pub fn skip_without_fixtures(test: &str) {
    eprintln!("skipping {test}: set {FIXTURES_ENV}=1 after running scripts/package-fixtures.sh");
}

/// The fixture tree must come from the checked-in pins: the npm root's `package.json` and lockfile
/// are byte-identical and the manifest lists exactly the pinned tarball hashes.
fn verify_fixtures(dir: &Path) {
    let hint = "run scripts/package-fixtures.sh";
    let manifest = std::fs::read_to_string(dir.join("manifest.sha256"))
        .unwrap_or_else(|error| panic!("{}: {error}; {hint}", dir.join("manifest.sha256").display()));
    for file in ["package.json", "package-lock.json"] {
        let pinned = std::fs::read(pins_dir().join("npm").join(file)).expect("pinned npm root file");
        let installed = std::fs::read(dir.join("agent/npm").join(file))
            .unwrap_or_else(|error| panic!("{}: {error}; {hint}", dir.join("agent/npm").join(file).display()));
        assert!(pinned == installed, "fixture npm/{file} differs from the pinned one; {hint}");
    }
    let pinned_tarballs = std::fs::read_to_string(pins_dir().join("tarballs.sha256")).expect("pinned tarball hashes");
    let pinned: Vec<String> = pinned_tarballs
        .lines()
        .map(|line| line.replacen("  ", "  tarballs/", 1))
        .collect();
    let listed: Vec<String> = manifest
        .lines()
        .filter(|line| line.contains("  tarballs/"))
        .map(str::to_string)
        .collect();
    assert_eq!(listed, pinned, "fixture manifest does not match tests/fixtures/packages/tarballs.sha256; {hint}");
}

/// The captain's `packages` entries, in settings order.
pub fn captain_packages() -> Vec<Value> {
    let settings = read_json(&pins_dir().join("settings.json"));
    settings["packages"].as_array().expect("settings.json packages").clone()
}

/// The `packages` entry whose source is `source` (for example `npm:context-mode`).
pub fn captain_package(source: &str) -> Value {
    captain_packages()
        .into_iter()
        .find(|entry| entry.as_str().or_else(|| entry["source"].as_str()) == Some(source))
        .unwrap_or_else(|| panic!("{source} is not in tests/fixtures/packages/settings.json"))
}

fn read_json(path: &Path) -> Value {
    let text = std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

// ---------------------------------------------------------------------------
// Sandbox
// ---------------------------------------------------------------------------

/// An isolated `HOME`, agent dir and project dir for one scenario.
pub struct Sandbox {
    _dir: tempfile::TempDir,
    home: PathBuf,
    agent_dir: PathBuf,
    project: PathBuf,
    tmp: PathBuf,
}

impl Sandbox {
    /// Creates the sandbox with the captain's settings restricted to `packages`, copying the
    /// fixture package tree in when given.
    pub fn new(packages: Vec<Value>, fixtures: Option<&Fixtures>) -> Self {
        let dir = tempfile::tempdir().expect("sandbox dir");
        let root = dir.path().canonicalize().expect("canonical sandbox dir");
        let home = root.join("home");
        let agent_dir = home.join(".pi/agent");
        let project = root.join("project");
        let tmp = root.join("tmp");
        for path in [&agent_dir, &project, &tmp] {
            std::fs::create_dir_all(path).expect("create sandbox dir");
        }
        if let Some(fixtures) = fixtures {
            copy_tree(&fixtures.agent_tree.join("npm"), &agent_dir);
            copy_tree(&fixtures.agent_tree.join("git"), &agent_dir);
        }
        let mut settings = read_json(&pins_dir().join("settings.json"));
        settings["packages"] = Value::Array(packages);
        settings["defaultProvider"] = json!("mock");
        settings["defaultModel"] = json!(MOCK_MODEL);
        write_json(&agent_dir.join("settings.json"), &settings);
        Self { _dir: dir, home, agent_dir, project, tmp }
    }

    /// The project directory the host runs in.
    pub fn project(&self) -> &Path {
        &self.project
    }

    /// A scratch directory inside the sandbox, wiped with it.
    pub fn tmp(&self) -> &Path {
        &self.tmp
    }

    /// Overrides top-level settings (for example `compaction`) before the host starts.
    pub fn set_settings(&self, extra: &Value) {
        let path = self.agent_dir.join("settings.json");
        let mut settings = read_json(&path);
        if let (Some(settings), Some(extra)) = (settings.as_object_mut(), extra.as_object()) {
            settings.extend(extra.clone());
        }
        write_json(&path, &settings);
    }

    /// Points `models.json` at the mock model on `port`.
    pub fn write_models(&self, port: u16) {
        let models = json!({
            "providers": {
                "mock": {
                    "baseUrl": format!("http://127.0.0.1:{port}/v1"),
                    "api": "openai-completions",
                    "apiKey": "mock-key",
                    "models": [{ "id": MOCK_MODEL, "name": "Mock", "contextWindow": 128000, "maxTokens": 4096 }]
                }
            }
        });
        write_json(&self.agent_dir.join("models.json"), &models);
    }
}

fn write_json(path: &Path, value: &Value) {
    let text = serde_json::to_string_pretty(value).expect("serialize json");
    std::fs::write(path, text).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
}

/// `cp -a` (reflinked where the filesystem supports it) keeps `node_modules/.bin` symlinks.
fn copy_tree(from: &Path, into: &Path) {
    let status = Command::new("cp")
        .args(["-a", "--reflink=auto"])
        .arg(from)
        .arg(into)
        .status()
        .expect("spawn cp");
    assert!(status.success(), "cp -a {} {} failed: {status}", from.display(), into.display());
}

// ---------------------------------------------------------------------------
// Host processes
// ---------------------------------------------------------------------------

/// How runs are kept off the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Isolation {
    /// An empty network namespace with only loopback up, so the mock model is the only server, and
    /// a PID namespace, so detached children (billion-context's proxy) end with the run.
    NetNs,
    /// No namespace support: `PI_OFFLINE=1` only.
    Offline,
}

/// Probes once whether unprivileged network namespaces work here.
pub fn isolation() -> Isolation {
    static ISOLATION: OnceLock<Isolation> = OnceLock::new();
    *ISOLATION.get_or_init(|| {
        let mut probe = netns_command(&["true"]);
        let ok = probe
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        let isolation = if ok { Isolation::NetNs } else { Isolation::Offline };
        eprintln!("package harness isolation: {isolation:?}");
        isolation
    })
}

/// `unshare` twice: the outer namespaces (network, PID with its own `/proc`) map root so `lo` can
/// be brought up, the inner one maps the real uid/gid back so the host runs as the invoking user.
fn netns_command(program_and_args: &[&str]) -> Command {
    // SAFETY: getuid/getgid have no preconditions and cannot fail.
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    let mut command = Command::new("unshare");
    command.args(["--net", "--pid", "--fork", "--mount-proc", "--map-root-user", "--", "sh", "-c"]).arg(
        r#"ip link set lo up && uid=$1 && gid=$2 && shift 2 && exec unshare --user --map-user="$uid" --map-group="$gid" -- "$@""#,
    );
    command.arg("sh").arg(uid.to_string()).arg(gid.to_string()).args(program_and_args);
    command
}

/// The host command line: [`HOST_ENV`] split on whitespace, or this crate's `pi` binary.
pub fn host_command() -> Vec<String> {
    match std::env::var(HOST_ENV) {
        Ok(line) if !line.trim().is_empty() => line.split_whitespace().map(str::to_string).collect(),
        _ => vec![env!("CARGO_BIN_EXE_pi").to_string()],
    }
}

/// The command line that starts `host_args` next to a mock model playing `script`.
pub fn host_beside_mock(sandbox: &Sandbox, name: &str, script: &Value, host_args: &[&str]) -> Vec<String> {
    let port = mock_port();
    sandbox.write_models(port);
    let script_path = sandbox.tmp.join(format!("{name}.mock-script.json"));
    write_json(&script_path, script);
    let log = sandbox.tmp.join(format!("{name}.mock-log.jsonl"));
    let mut argv: Vec<String> = vec![
        mock_dir().join("with-mockllm.sh").display().to_string(),
        port.to_string(),
        script_path.display().to_string(),
        log.display().to_string(),
    ];
    argv.extend(host_command());
    argv.extend(host_args.iter().map(|arg| arg.to_string()));
    argv
}

/// The port the mock model listens on for this run: the netns one, or a free port.
pub fn mock_port() -> u16 {
    match isolation() {
        Isolation::NetNs => NETNS_MOCK_PORT,
        Isolation::Offline => free_port(),
    }
}

/// A command for a helper program (`tuidrive.py`, `rpcdrive.py`) that must see exactly what a host
/// run sees — the sandbox `HOME`, agent dir and project — under the same network isolation.
pub fn sandbox_command(sandbox: &Sandbox, argv: &[&str]) -> Command {
    let mut command = match isolation() {
        Isolation::NetNs => netns_command(argv),
        Isolation::Offline => {
            let mut command = Command::new(argv[0]);
            command.args(&argv[1..]);
            command
        }
    };
    let path = std::env::var_os("PATH").unwrap_or_default();
    command
        .env_clear()
        .env("PATH", path)
        .env("HOME", &sandbox.home)
        .env("PI_CODING_AGENT_DIR", &sandbox.agent_dir)
        .env("PI_OFFLINE", "1")
        .env("TMPDIR", &sandbox.tmp)
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .env("LANG", "C.UTF-8")
        .env("TZ", "UTC")
        .current_dir(&sandbox.project)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// Bounds the number of concurrently running hosts.
struct Slots {
    used: Mutex<usize>,
    freed: Condvar,
}

struct SlotGuard(&'static Slots);

impl Drop for SlotGuard {
    fn drop(&mut self) {
        *self.0.used.lock().expect("slot lock poisoned") -= 1;
        self.0.freed.notify_one();
    }
}

fn acquire_slot() -> SlotGuard {
    static SLOTS: Slots = Slots { used: Mutex::new(0), freed: Condvar::new() };
    let limit = std::env::var(JOBS_ENV).ok().and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_JOBS).max(1);
    let mut used = SLOTS.used.lock().expect("slot lock poisoned");
    while *used >= limit {
        used = SLOTS.freed.wait(used).expect("slot lock poisoned");
    }
    *used += 1;
    SlotGuard(&SLOTS)
}

/// How long output may keep arriving after the host exited. Detached children of extensions can
/// hold the pipes open past that; their output is not part of the observation.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// One host run next to the mock model.
struct Run {
    child: Child,
    started: Instant,
    stdout: Receiver<String>,
    stderr: Receiver<String>,
    log: PathBuf,
}

impl Run {
    fn start(sandbox: &Sandbox, name: &str, host_args: &[&str], stdin: Stdio) -> Self {
        let isolation = isolation();
        let port = match isolation {
            Isolation::NetNs => NETNS_MOCK_PORT,
            Isolation::Offline => free_port(),
        };
        sandbox.write_models(port);
        let script = sandbox.tmp.join(format!("{name}.mock-script.json"));
        std::fs::write(&script, r#"[{"text": "ok"}]"#).expect("mock script");
        let log = sandbox.tmp.join(format!("{name}.mock-log.jsonl"));

        let launcher = mock_dir().join("with-mockllm.sh");
        let mut argv: Vec<String> = vec![
            launcher.display().to_string(),
            port.to_string(),
            script.display().to_string(),
            log.display().to_string(),
        ];
        argv.extend(host_command());
        argv.extend(host_args.iter().map(|arg| arg.to_string()));
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let mut command = match isolation {
            Isolation::NetNs => netns_command(&argv),
            Isolation::Offline => {
                let mut command = Command::new(argv[0]);
                command.args(&argv[1..]);
                command
            }
        };
        let path = std::env::var_os("PATH").unwrap_or_default();
        command
            .env_clear()
            .env("PATH", path)
            .env("HOME", &sandbox.home)
            .env("PI_CODING_AGENT_DIR", &sandbox.agent_dir)
            .env("PI_OFFLINE", "1")
            .env("TMPDIR", &sandbox.tmp)
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .env("LANG", "C.UTF-8")
            .env("TZ", "UTC")
            .current_dir(&sandbox.project)
            .stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let mut child = command.spawn().unwrap_or_else(|error| panic!("spawn {argv:?}: {error}"));

        let stdout = read_lines(child.stdout.take().expect("piped stdout"));
        let stderr = read_lines(child.stderr.take().expect("piped stderr"));
        Self { child, started: Instant::now(), stdout, stderr, log }
    }

    fn deadline(&self) -> Instant {
        self.started + timeout()
    }

    /// Waits for exit until the deadline, killing the whole process group past it.
    fn finish(mut self) -> Finished {
        let status = loop {
            if let Some(status) = self.child.try_wait().expect("wait for host") {
                break Some(status);
            }
            if Instant::now() >= self.deadline() {
                kill_group(&self.child);
                let _ = self.child.wait();
                break None;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        // The launcher has exited; stop what is left of its process group so the pipes close.
        kill_group(&self.child);
        let grace = Instant::now() + DRAIN_GRACE;
        let stdout = drain(&self.stdout, grace);
        let stderr = drain(&self.stderr, grace).join("\n");
        let requests = std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .map(|entry| entry["req"].clone())
            .collect();
        Finished { status, stdout, stderr, requests }
    }
}

struct Finished {
    /// `None` when the run timed out.
    status: Option<ExitStatus>,
    stdout: Vec<String>,
    stderr: String,
    requests: Vec<Value>,
}

/// Forwards lines from `pipe` on a reader thread, so a pipe held open by a straggler never blocks.
fn read_lines(pipe: impl Read + Send + 'static) -> Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(pipe).lines() {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    receiver
}

/// Collects lines until the writer side closes or `until` passes.
fn drain(lines: &Receiver<String>, until: Instant) -> Vec<String> {
    let mut collected = Vec::new();
    loop {
        match lines.recv_timeout(until.saturating_duration_since(Instant::now())) {
            Ok(line) => collected.push(line),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return collected,
        }
    }
}

fn kill_group(child: &Child) {
    let Ok(pid) = libc::pid_t::try_from(child.id()) else { return };
    // SAFETY: plain syscall; the group id is the child's pid (process_group(0)), and a group that
    // no longer exists only yields ESRCH.
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a free port");
    listener.local_addr().expect("local addr").port()
}

// ---------------------------------------------------------------------------
// Observation
// ---------------------------------------------------------------------------

/// A slash command as `get_commands` reports it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SlashCommand {
    pub source: String,
    pub name: String,
}

/// What one scenario registers, as compared against the TS Pi golden.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registered {
    /// Tool names offered to the model on the first print-mode request, sorted.
    pub tools: Vec<String>,
    /// Commands from RPC `get_commands`, sorted by source and name.
    pub commands: Vec<SlashCommand>,
}

/// Everything a scenario produced.
#[derive(Debug)]
pub struct Observation {
    pub registered: Registered,
    /// Extension errors, load failures, non-zero exits and timeouts, from both modes.
    pub diagnostics: Vec<String>,
}

/// Runs the host in print mode and in RPC mode inside `sandbox`.
pub fn observe(sandbox: &Sandbox) -> Observation {
    let _slot = acquire_slot();
    let mut diagnostics = Vec::new();

    let print = Run::start(sandbox, "print", &["-p", "Reply with ok."], Stdio::null()).finish();
    collect_diagnostics("print", &print, &mut diagnostics);
    let tools = match print.requests.first() {
        Some(request) => tool_names(request),
        None => {
            diagnostics.push(format!("print: the model received no request; stdout: {:?}", print.stdout));
            Vec::new()
        }
    };

    let (rpc, commands) = run_rpc(sandbox);
    collect_diagnostics("rpc", &rpc, &mut diagnostics);
    for line in &rpc.stdout {
        if let Ok(event) = serde_json::from_str::<Value>(line)
            && event["type"] == "extension_error"
        {
            diagnostics.push(format!("rpc: extension_error event: {event}"));
        }
    }
    let commands = commands.unwrap_or_else(|| {
        diagnostics.push("rpc: no get_commands response".to_string());
        Vec::new()
    });

    Observation { registered: Registered { tools, commands }, diagnostics }
}

fn run_rpc(sandbox: &Sandbox) -> (Finished, Option<Vec<SlashCommand>>) {
    let mut run = Run::start(sandbox, "rpc", &["--mode", "rpc"], Stdio::piped());
    let mut stdin: Option<ChildStdin> = run.child.stdin.take();
    let request = json!({ "id": RPC_COMMANDS_ID, "type": "get_commands" });
    if let Some(stdin) = stdin.as_mut() {
        // A host that already exited closes the pipe; that shows up as a missing response.
        let _ = writeln!(stdin, "{request}");
        let _ = stdin.flush();
    }
    let mut seen = Vec::new();
    let mut commands = None;
    while commands.is_none() {
        let remaining = run.deadline().saturating_duration_since(Instant::now());
        match run.stdout.recv_timeout(remaining) {
            Ok(line) => {
                if let Ok(value) = serde_json::from_str::<Value>(&line)
                    && value["type"] == "response"
                    && value["id"] == RPC_COMMANDS_ID
                {
                    commands = Some(parse_commands(&value));
                }
                seen.push(line);
            }
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
        }
    }
    // End of input shuts the RPC host down.
    drop(stdin);
    let mut finished = run.finish();
    seen.append(&mut finished.stdout);
    finished.stdout = seen;
    (finished, commands)
}

fn parse_commands(response: &Value) -> Vec<SlashCommand> {
    let mut commands: Vec<SlashCommand> = response["data"]["commands"]
        .as_array()
        .map(|commands| {
            commands
                .iter()
                .map(|command| SlashCommand {
                    source: command["source"].as_str().unwrap_or_default().to_string(),
                    name: command["name"].as_str().unwrap_or_default().to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    commands.sort();
    commands
}

fn tool_names(request: &Value) -> Vec<String> {
    let mut names: Vec<String> = request["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| tool["function"]["name"].as_str().or_else(|| tool["name"].as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// Lines a host prints for extension failures: `Failed to load extension "…": …` at startup and
/// `Extension error (…): …` from event handlers.
fn is_extension_diagnostic(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    line.contains("extension error") || line.contains("failed to load extension")
}

fn collect_diagnostics(mode: &str, run: &Finished, diagnostics: &mut Vec<String>) {
    if std::env::var_os(VERBOSE_ENV).is_some() {
        eprintln!("--- {mode} stderr ---\n{}\n--- end {mode} stderr ---", run.stderr);
    }
    for line in run.stderr.lines().filter(|line| is_extension_diagnostic(line)) {
        diagnostics.push(format!("{mode}: {line}"));
    }
    match run.status {
        None => diagnostics.push(format!("{mode}: timed out after {:?}; stderr:\n{}", timeout(), run.stderr)),
        Some(status) if !status.success() => {
            diagnostics.push(format!("{mode}: exited with {status}; stderr:\n{}", run.stderr));
        }
        Some(_) => {}
    }
}

// ---------------------------------------------------------------------------
// Golden
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Serialize, Deserialize)]
struct Golden {
    generator: String,
    scenarios: std::collections::BTreeMap<String, Registered>,
}

/// Asserts a scenario ran without extension diagnostics and registered what TS Pi 1.4.0 does.
///
/// With [`GOLDEN_WRITE_ENV`] set the observation is recorded as the golden first (the golden
/// script runs this under TS Pi), and diagnostics still fail the run.
pub fn assert_scenario(scenario: &str, observation: &Observation) {
    let record = std::env::var_os(GOLDEN_WRITE_ENV).is_some();
    if record {
        record_golden(scenario, &observation.registered);
    }
    assert!(
        observation.diagnostics.is_empty(),
        "{scenario}: extension diagnostics:\n{}",
        observation.diagnostics.join("\n")
    );
    if !record {
        compare_golden(scenario, &observation.registered);
    }
}

fn record_golden(scenario: &str, registered: &Registered) {
    static WRITE_LOCK: Mutex<()> = Mutex::new(());
    let _guard = WRITE_LOCK.lock().expect("golden lock poisoned");
    let path = golden_path();
    let mut golden: Golden = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    golden.generator = std::env::var("PI_RS_PKG_GOLDEN_GENERATOR").unwrap_or_else(|_| host_command().join(" "));
    golden.scenarios.insert(scenario.to_string(), registered.clone());
    std::fs::create_dir_all(path.parent().expect("golden dir")).expect("create golden dir");
    let text = serde_json::to_string_pretty(&golden).expect("serialize golden") + "\n";
    std::fs::write(&path, text).expect("write golden");
}

/// A documented, plan-tracked difference between pi-rs and the TS golden for one scenario.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KnownGap {
    /// Tools TS Pi offers that pi-rs does not, until the named plan items land.
    missing_tools: Vec<String>,
    /// Host gap and plan items that close it; shown when the gap changes.
    closed_by: String,
}

/// Known gaps apply only to pi-rs; the TS host is compared with the golden as recorded.
fn known_gap(scenario: &str) -> Option<KnownGap> {
    if std::env::var_os(HOST_ENV).is_some() {
        return None;
    }
    let path = pins_dir().join("golden/known-gaps.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let mut gaps: std::collections::BTreeMap<String, KnownGap> =
        serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    gaps.remove(scenario)
}

fn compare_golden(scenario: &str, registered: &Registered) {
    let path = golden_path();
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let golden: Golden = serde_json::from_str(&text).expect("parse golden");
    let want = golden
        .scenarios
        .get(scenario)
        .unwrap_or_else(|| panic!("scenario {scenario} is missing from {}", path.display()));
    let mut want_tools = want.tools.clone();
    let mut gap_note = String::new();
    if let Some(gap) = known_gap(scenario) {
        for tool in &gap.missing_tools {
            assert!(want_tools.contains(tool), "{scenario}: known gap lists {tool}, which the golden does not have");
        }
        want_tools.retain(|tool| !gap.missing_tools.contains(tool));
        gap_note = format!(
            " minus the known gap {:?} ({}); if the gap closed, remove it from tests/fixtures/packages/golden/known-gaps.json",
            gap.missing_tools, gap.closed_by
        );
    }
    assert_eq!(
        registered.tools, want_tools,
        "{scenario}: tool names differ from {}{gap_note}",
        golden.generator
    );
    assert_eq!(registered.commands, want.commands, "{scenario}: commands differ from {}", golden.generator);
}
