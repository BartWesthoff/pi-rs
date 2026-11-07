//! Port of `packages/coding-agent/src/core/timings.ts`.
//!
//! Central timing instrumentation for startup profiling.
//! Enable with PI_TIMING=1 environment variable.

use indexmap::IndexMap;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimingNamespaceTimings {
    pub label: String,
    #[serde(with = "pi_ai::serde_util::js_f64")]
    pub ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimingNamespace {
    pub timings: Vec<TimingNamespaceTimings>,
    pub last_time: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TimingLabel {
    #[serde(rename = "main")]
    Main,
    #[serde(rename = "extensions")]
    Extensions,
}

impl TimingLabel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Extensions => "extensions",
        }
    }
}

static ENABLED: LazyLock<bool> =
    LazyLock::new(|| std::env::var("PI_TIMING").ok().as_deref() == Some("1"));

/// Module-level `timingNamespaces` map (insertion ordered like a JS `Map`).
static TIMING_NAMESPACES: LazyLock<Mutex<IndexMap<TimingLabel, TimingNamespace>>> =
    LazyLock::new(|| Mutex::new(IndexMap::new()));

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn reset_timings(namespace: Option<&TimingLabel>) {
    if !*ENABLED {
        return;
    }
    let namespace = namespace.copied().unwrap_or(TimingLabel::Main);
    TIMING_NAMESPACES.lock().insert(
        namespace,
        TimingNamespace {
            timings: Vec::new(),
            last_time: now_ms(),
        },
    );
}

pub fn time(label: &str, namespace: Option<&TimingLabel>) {
    if !*ENABLED {
        return;
    }
    let now = now_ms();
    let namespace = namespace.copied().unwrap_or(TimingLabel::Main);
    if !TIMING_NAMESPACES.lock().contains_key(&namespace) {
        reset_timings(Some(&namespace));
    }
    let mut namespaces = TIMING_NAMESPACES.lock();
    let ns = namespaces.get_mut(&namespace).expect("namespace exists");
    ns.timings.push(TimingNamespaceTimings {
        label: label.to_string(),
        ms: (now - ns.last_time) as f64,
    });
    ns.last_time = now;
}

fn print_timing_group(title: &str, timings: &[TimingNamespaceTimings]) {
    let printable: Vec<&TimingNamespaceTimings> = timings.iter().filter(|t| t.ms >= 0.0).collect();
    if printable.is_empty() {
        return;
    }
    eprintln!("\n--- {title} ---");
    for t in &printable {
        eprintln!("  {}: {}ms", t.label, t.ms);
    }
    let total: f64 = printable.iter().map(|t| t.ms).sum();
    eprintln!("  TOTAL: {total}ms");
    eprintln!("{}\n", "-".repeat(title.chars().count() + 8));
}

pub fn print_timings() {
    if !*ENABLED {
        return;
    }
    let namespaces = TIMING_NAMESPACES.lock().clone();
    for (namespace, ns) in namespaces {
        print_timing_group(
            &format!("Startup Timings: {}", namespace.as_str()),
            &ns.timings,
        );
    }
    // JS source handed to the QuickJS compiler so far, for the package footprint benchmark
    // (`scripts/perf/packages.py`). TS Pi is measured with a `node:module` hook instead.
    let compiled = pi_jsrt::js::compiled_js_bytes();
    eprintln!("Compiled JS: {compiled} bytes\n");
}
