//! Interactive package goldens (PKGP-21): what `rpiv-todo`, `rpiv-ask-user-question` and ponytail
//! paint and answer in the TUI and over RPC, compared against TS Pi 1.4.0.
//!
//! Each scenario drives the host in a private tmux server ([`mock_dir`]`/tuidrive.py`) or over the
//! RPC protocol (`rpcdrive.py`) next to the scripted mock model, with the same fixture package tree
//! and isolation as the load smoke tests. Screens are normalized (sandbox path, session id, shutdown
//! banner) and recorded in `tests/fixtures/packages/golden/ts-pi-1.4.0-ui.json`, which
//! `scripts/package-golden.sh` regenerates by running these tests under TS Pi 1.4.0.
//!
//! ```text
//! scripts/package-fixtures.sh
//! PI_RS_PKG_FIXTURES=1 cargo test -p pi-coding-agent --test packages ui_
//! ```

use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::{Value, json};

use crate::support::{
    Sandbox, captain_package, fixtures, host_beside_mock, mock_dir, pins_dir, sandbox_command, skip_without_fixtures,
};

const COLS: u32 = 120;
const ROWS: u32 = 40;

fn golden_path() -> PathBuf {
    pins_dir().join("golden/ts-pi-1.4.0-ui.json")
}

/// Removes what differs between two identical runs: the sandbox path, the session id and the
/// post-exit banner tmux leaves on screen.
fn normalize(text: &str, sandbox: &Sandbox) -> String {
    let root = sandbox.project().parent().expect("sandbox root").display().to_string();
    let mut lines: Vec<String> = Vec::new();
    for line in text.lines() {
        let mut line = line.trim_end().replace(&root, "<SANDBOX>");
        if let Some(start) = line.find("pi --session ") {
            let name_at = start + "pi --session ".len();
            let end = line[name_at..].find(char::is_whitespace).map_or(line.len(), |offset| name_at + offset);
            line.replace_range(name_at..end, "<SESSION>");
        }
        // The footer's context counters are host bookkeeping, not package UI, and after `/compact`
        // pi-rs counts the transcript differently from TS Pi (↑40 ↓20 vs ↑50 ↓25).
        if line.contains('↑') {
            line = strip_counters(&line);
        }
        if line.starts_with("Pane is dead (status ") {
            line = "Pane is dead (status <EXIT>)".to_string();
        }
        lines.push(line.trim_end().to_string());
    }
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

/// Replaces the `↑N ↓M` context counters with `↑<N> ↓<N>`.
fn strip_counters(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        out.push(c);
        if c == '↑' || c == '↓' {
            let digits: String = chars.clone().take_while(char::is_ascii_digit).collect();
            if !digits.is_empty() {
                for _ in 0..digits.len() {
                    chars.next();
                }
                out.push_str("<N>");
            }
        }
    }
    out
}

/// Runs one tmux plan and returns the recorded snapshots, normalized, in plan order.
fn run_tui(sandbox: &Sandbox, name: &str, runs: Value) -> std::collections::BTreeMap<String, String> {
    let plan_path = sandbox.tmp().join(format!("{name}.plan.json"));
    let result_path = sandbox.tmp().join(format!("{name}.result.json"));
    std::fs::write(&plan_path, serde_json::to_string_pretty(&json!({
        "cols": COLS, "rows": ROWS, "runs": runs
    })).expect("serialize plan"))
    .expect("write plan");
    let driver = mock_dir().join("tuidrive.py").display().to_string();
    let status = sandbox_command(sandbox, &[
        "python3",
        &driver,
        &plan_path.display().to_string(),
        &result_path.display().to_string(),
    ])
    .status()
    .expect("spawn tuidrive.py");
    assert!(status.success(), "{name}: tuidrive.py failed with {status}");
    let result: Value = serde_json::from_str(&std::fs::read_to_string(&result_path).expect("tuidrive result"))
        .expect("parse tuidrive result");
    let mut snapshots = std::collections::BTreeMap::new();
    for (index, run) in result["runs"].as_array().expect("runs").iter().enumerate() {
        assert!(
            run["ok"].as_bool().unwrap_or(false),
            "{name} run {index} failed at {:?}; screen:\n{}",
            run["failed"],
            run["screen"].as_str().unwrap_or_default()
        );
        for (snap, screen) in run["snaps"].as_object().expect("snaps") {
            snapshots.insert(snap.clone(), normalize(screen.as_str().unwrap_or_default(), sandbox));
        }
    }
    snapshots
}

/// Runs one RPC plan with scripted dialog answers next to the mock model and returns the
/// package-facing events: dialog prompts, tool results and extension errors, in arrival order.
fn run_rpc(sandbox: &Sandbox, name: &str, script: Value, answers: Value, steps: Value) -> Vec<Value> {
    let argv = host_beside_mock(sandbox, name, &script, &["--mode", "rpc"]);
    let steps_path = sandbox.tmp().join(format!("{name}.steps.json"));
    std::fs::write(&steps_path, serde_json::to_string_pretty(&json!({
        "answers": answers, "steps": steps
    })).expect("serialize steps"))
    .expect("write steps");
    let prefix = sandbox.tmp().join(name);
    let driver = mock_dir().join("rpcdrive.py").display().to_string();
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let output = sandbox_command(sandbox, &[
        "python3",
        &driver,
        &steps_path.display().to_string(),
        &prefix.display().to_string(),
        "300",
        "--",
    ])
    .args(&argv)
    .output()
    .expect("spawn rpcdrive.py");
    let summary = String::from_utf8_lossy(&output.stdout);
    assert!(
        summary.contains("\"status\": \"ok\""),
        "{name}: rpcdrive.py did not finish the script: {summary}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = std::fs::read_to_string(prefix.with_extension("events.jsonl")).expect("rpc events");
    events
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|entry| match entry["rec"]["type"].as_str()? {
            // Dialog prompts the extension raised, and the tool result it produced from the answer.
            "extension_ui_request" => {
                let rec = &entry["rec"];
                Some(json!({
                    "type": "extension_ui_request",
                    "method": rec["method"],
                    "title": rec["title"],
                    "options": rec["options"],
                }))
            }
            "tool_execution_end" => {
                let rec = &entry["rec"];
                Some(json!({
                    "type": "tool_execution_end",
                    "tool": rec["toolName"],
                    "isError": rec["isError"],
                    "text": rec["result"]["content"][0]["text"],
                    "details": rec["result"]["details"],
                }))
            }
            "extension_error" => Some(json!({ "type": "extension_error", "event": entry["rec"] })),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Golden
// ---------------------------------------------------------------------------

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct UiGolden {
    generator: String,
    scenarios: std::collections::BTreeMap<String, Value>,
}

/// Asserts one scenario against the TS Pi golden, or records it when the golden script asks.
fn assert_ui(scenario: &str, observed: Value) {
    static WRITE_LOCK: Mutex<()> = Mutex::new(());
    let path = golden_path();
    if std::env::var_os(crate::support::GOLDEN_WRITE_ENV).is_some() {
        let _guard = WRITE_LOCK.lock().expect("golden lock poisoned");
        let mut golden: UiGolden = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        golden.generator = std::env::var("PI_RS_PKG_GOLDEN_GENERATOR")
            .unwrap_or_else(|_| crate::support::host_command().join(" "));
        golden.scenarios.insert(scenario.to_string(), observed);
        std::fs::create_dir_all(path.parent().expect("golden dir")).expect("create golden dir");
        std::fs::write(&path, serde_json::to_string_pretty(&golden).expect("serialize golden") + "\n")
            .expect("write golden");
        return;
    }
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let golden: UiGolden = serde_json::from_str(&text).expect("parse ui golden");
    let want = golden
        .scenarios
        .get(scenario)
        .unwrap_or_else(|| panic!("scenario {scenario} is missing from {}", path.display()));
    let want_pretty = serde_json::to_string_pretty(want).expect("serialize golden scenario");
    let got_pretty = serde_json::to_string_pretty(&observed).expect("serialize observation");
    assert_eq!(got_pretty, want_pretty, "{scenario}: differs from {}", golden.generator);
}

/// The sandbox for one scenario with the given packages, or `None` (skip) without fixtures.
fn sandbox_for(test: &str, sources: &[&str]) -> Option<Sandbox> {
    let Some(fixtures) = fixtures() else {
        skip_without_fixtures(test);
        return None;
    };
    let packages = sources.iter().map(|source| captain_package(source)).collect();
    Some(Sandbox::new(packages, Some(fixtures)))
}

fn snapshots_value(snapshots: std::collections::BTreeMap<String, String>) -> Value {
    Value::Object(snapshots.into_iter().map(|(name, text)| (name, Value::String(text))).collect())
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

/// The todo tool's overlay widget survives `/reload` with its tasks intact.
#[test]
fn ui_todo_reload() {
    let Some(sandbox) = sandbox_for("ui_todo_reload", &["npm:@juicesharp/rpiv-todo"]) else { return };
    let argv = host_beside_mock(&sandbox, "ui_todo_reload", &json!([
        {"tool_calls": [{"name": "todo", "arguments": {
            "action": "create",
            "tasks": [{"subject": "Write tests"}, {"subject": "Ship it"}]
        }}]},
        {"text": "Tasks created."}
    ]), &[]);
    let snapshots = run_tui(&sandbox, "ui_todo_reload", json!([{
        "argv": argv,
        "steps": [
            {"wait": "mock-model", "timeout": 120},
            {"text": "Plan the work."}, {"keys": ["Enter"]},
            {"wait": "Tasks created.", "timeout": 120},
            {"snap": "after_create"},
            {"text": "/reload"}, {"keys": ["Enter"]},
            {"wait": "Reloaded", "timeout": 60},
            {"snap": "after_reload"},
            {"keys": ["C-d"]}, {"exit": 30}
        ]
    }]));
    assert_ui("ui_todo_reload", snapshots_value(snapshots));
}

/// `/compact` after a todo lists leaves the overlay widget rendering the same tasks.
#[test]
fn ui_todo_compact() {
    let Some(sandbox) = sandbox_for("ui_todo_compact", &["npm:@juicesharp/rpiv-todo"]) else { return };
    // A tiny keep window makes the 15-token transcript compactable; both hosts then summarize it.
    sandbox.set_settings(&json!({"compaction": {"keepRecentTokens": 1, "reserveTokens": 4096}}));
    let argv = host_beside_mock(&sandbox, "ui_todo_compact", &json!([
        {"tool_calls": [{"name": "todo", "arguments": {
            "action": "create",
            "tasks": [{"subject": "Write tests"}, {"subject": "Ship it"}]
        }}]},
        {"text": "Tasks created."},
        {"text": "Noted."},
        {"text": "## Goal\nShip it."}
    ]), &[]);
    let snapshots = run_tui(&sandbox, "ui_todo_compact", json!([{
        "argv": argv,
        "steps": [
            {"wait": "mock-model", "timeout": 120},
            {"text": "Plan the work."}, {"keys": ["Enter"]},
            {"wait": "Tasks created.", "timeout": 120},
            {"text": "Carry on."}, {"keys": ["Enter"]},
            {"wait": "Noted.", "timeout": 120},
            {"text": "/compact"}, {"keys": ["Enter"]},
            {"wait": "Compacted from", "timeout": 120},
            {"snap": "after_compact"},
            {"keys": ["C-d"]}, {"exit": 30}
        ]
    }]));
    assert_ui("ui_todo_compact", snapshots_value(snapshots));
}

/// A question asked while another capturing overlay is open stacks as an overlay above it, takes
/// the keys, and leaves the overlay below readable once answered.
#[test]
fn ui_question_over_overlay() {
    let Some(fixtures) = fixtures() else {
        skip_without_fixtures("ui_question_over_overlay");
        return;
    };
    let sandbox = Sandbox::new(
        vec![
            Value::String(pins_dir().join("overlay-probe").display().to_string()),
            captain_package("npm:@juicesharp/rpiv-ask-user-question"),
        ],
        Some(fixtures),
    );
    let argv = host_beside_mock(&sandbox, "ui_question_over_overlay", &json!([
        {"tool_calls": [{"name": "open_overlay_probe", "arguments": {}}]},
        {"tool_calls": [{"name": "ask_user_question", "arguments": {"questions": [{
            "question": "Which color should we use?",
            "header": "Color",
            "options": [
                {"label": "Red", "description": "A warm color"},
                {"label": "Blue", "description": "A cool color"}
            ]
        }]}}]},
        {"text": "Answer received."}
    ]), &[]);
    let snapshots = run_tui(&sandbox, "ui_question_over_overlay", json!([{
        "argv": argv,
        "steps": [
            {"wait": "mock-model", "timeout": 120},
            {"text": "Ask me."}, {"keys": ["Enter"]},
            // The probe overlay mounts before the question is raised; waiting for it keeps the
            // snapshot from catching the question before the overlay below it has painted.
            {"wait": "OVERLAY PROBE OPEN", "timeout": 120},
            {"wait": "Which color should we use?", "timeout": 120},
            {"snap": "question"},
            {"keys": ["Down"]}, {"snap": "moved"},
            {"keys": ["Enter"]},
            {"wait": "Answer received.", "timeout": 60},
            {"snap": "answered"},
            {"keys": ["Escape"]}, {"sleep": 1}, {"snap": "closed"},
            {"keys": ["C-d"]}, {"exit": 30}
        ]
    }]));
    assert_ui("ui_question_over_overlay", snapshots_value(snapshots));
}

/// Esc cancels the questionnaire and the tool reports a cancelled answer.
#[test]
fn ui_question_escape_cancels() {
    let Some(sandbox) = sandbox_for("ui_question_escape_cancels", &["npm:@juicesharp/rpiv-ask-user-question"])
    else {
        return;
    };
    let argv = host_beside_mock(&sandbox, "ui_question_escape_cancels", &json!([
        {"tool_calls": [{"name": "ask_user_question", "arguments": {"questions": [{
            "question": "Which color should we use?",
            "header": "Color",
            "options": [
                {"label": "Red", "description": "A warm color"},
                {"label": "Blue", "description": "A cool color"}
            ]
        }]}}]},
        {"text": "Cancelled noted."}
    ]), &[]);
    let snapshots = run_tui(&sandbox, "ui_question_escape_cancels", json!([{
        "argv": argv,
        "steps": [
            {"wait": "mock-model", "timeout": 120},
            {"text": "Ask me."}, {"keys": ["Enter"]},
            {"wait": "Which color should we use?", "timeout": 120},
            {"snap": "question"},
            {"keys": ["Escape"]},
            {"wait": "Cancelled noted.", "timeout": 60},
            {"snap": "cancelled"},
            {"keys": ["C-d"]}, {"exit": 30}
        ]
    }]));
    assert_ui("ui_question_escape_cancels", snapshots_value(snapshots));
}

/// Ponytail's `ultra` level persists into the next session opened with `--continue`.
#[test]
fn ui_ponytail_ultra_continue() {
    let Some(sandbox) = sandbox_for("ui_ponytail_ultra_continue", &["git:github.com/DietrichGebert/ponytail"]) else {
        return;
    };
    // Distinct mock names: each run gets its own script and request log.
    let first = host_beside_mock(&sandbox, "ui_ponytail_first", &json!([{"text": "First reply."}]), &[]);
    let second = host_beside_mock(&sandbox, "ui_ponytail_second", &json!([{"text": "Second reply."}]), &["--continue"]);
    let snapshots = run_tui(&sandbox, "ui_ponytail_ultra_continue", json!([
        {
            "argv": first,
            "steps": [
                {"wait": "ponytail: ⚡ FULL", "timeout": 120},
                {"text": "/ponytail ultra"},
                // Re-sending Enter cannot repair characters dropped while the editor boots.
                {"wait": "/ponytail ultra", "timeout": 30},
                {"keys": ["Enter"], "until": "Ponytail mode set to ultra.", "tries": 3, "timeout": 5},
                {"wait": "ULTRA", "timeout": 60},
                {"snap": "ultra"},
                {"text": "Hello."}, {"keys": ["Enter"]},
                {"wait": "First reply.", "timeout": 60},
                {"keys": ["C-d"]}, {"exit": 30}
            ]
        },
        {
            "argv": second,
            "steps": [
                {"wait": "ULTRA", "timeout": 120},
                {"snap": "continued"},
                {"text": "Again."}, {"keys": ["Enter"]},
                {"wait": "Second reply.", "timeout": 60},
                {"snap": "after_prompt"},
                {"keys": ["C-d"]}, {"exit": 30}
            ]
        }
    ]));
    assert_ui("ui_ponytail_ultra_continue", snapshots_value(snapshots));
}

/// Over RPC the questionnaire falls back to Pi's dialog primitives; the answer the client sends
/// back reaches the model, and the tool result records it.
#[test]
fn ui_question_rpc_answer() {
    let Some(sandbox) = sandbox_for("ui_question_rpc_answer", &["npm:@juicesharp/rpiv-ask-user-question"]) else {
        return;
    };
    let events = run_rpc(
        &sandbox,
        "ui_question_rpc_answer",
        json!([
            {"tool_calls": [{"name": "ask_user_question", "arguments": {"questions": [{
                "question": "Which color should we use?",
                "header": "Color",
                "options": [
                    {"label": "Red", "description": "A warm color"},
                    {"label": "Blue", "description": "A cool color"}
                ]
            }]}}]},
            {"text": "Answer received."}
        ]),
        json!({"select": 1}),
        json!([{"cmd": {"type": "prompt", "message": "Ask me."}, "wait": "settled", "timeout": 300}]),
    );
    assert_ui("ui_question_rpc_answer", Value::Array(events));
}
