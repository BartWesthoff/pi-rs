//! The compiled-Pi host identity extensions see (`HostIdentity`, PKGP-04), checked by running the
//! `pi` binary with a probe extension. The probes and their expected values are the golden shared
//! with pi-jsrt (`crates/pi-jsrt/tests/fixtures/host_identity/`), which also holds Node 22's
//! values.
//!
//! The package scenarios load the unchanged upstream packages from the fixture tree of
//! `scripts/package-fixtures.sh` and are skipped unless `PI_RS_PKG_FIXTURES` is set
//! (`PI_RS_PKG_FIXTURES_DIR` overrides `<workspace>/target/pkg-fixtures`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use pi_coding_agent::core::extensions::js::HOST_IDENTITY;
use serde_json::Value;

const PROBE: &str = include_str!("../../pi-jsrt/tests/fixtures/host_identity/probe.js");
const GOLDEN: &str = include_str!("../../pi-jsrt/tests/fixtures/host_identity/golden.tsv");
const PI: &str = env!("CARGO_BIN_EXE_pi");

/// Run `pi` (RPC mode, stdin closed) with the extension `source`, which writes its result to the
/// file named by the `OUT` constant prepended to it, and return that result.
fn run_extension(source: &str, env: &[(&str, &str)]) -> (Value, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let (agent_dir, project_dir) = (root.join("agent"), root.join("project"));
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::create_dir_all(&project_dir).unwrap();
    let out = project_dir.join("out.json");
    let probe = project_dir.join("probe.js");
    std::fs::write(&probe, format!("const OUT = {};\n{source}", serde_json::to_string(&out).unwrap())).unwrap();
    let args: Vec<String> =
        ["-ne", "-e", probe.to_str().unwrap(), "--mode", "rpc", "--no-session"].map(String::from).to_vec();
    let mut command = Command::new(PI);
    command
        .args(&args)
        .current_dir(&project_dir)
        .env("PI_CODING_AGENT_DIR", &agent_dir)
        .env("PI_OFFLINE", "1")
        .env_remove(HOST_IDENTITY.pi_binary_env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        command.env(k, v);
    }
    let output = command.output().expect("spawn pi");
    let text = std::fs::read_to_string(&out)
        .unwrap_or_else(|e| panic!("{e}: no result; stderr:\n{}", String::from_utf8_lossy(&output.stderr)));
    (serde_json::from_str(&text).unwrap(), args)
}

fn golden_host_column() -> BTreeMap<String, String> {
    GOLDEN
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .map(|line| {
            let cols: Vec<&str> = line.split('\t').collect();
            (cols[0].to_string(), cols[2].to_string())
        })
        .collect()
}

fn as_rows(value: &Value) -> BTreeMap<String, String> {
    value.as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect()
}

#[test]
fn extensions_see_the_compiled_pi_identity() {
    let (value, args) = run_extension(
        &format!(
            r#"import {{ writeFileSync }} from "node:fs";
const probe = {PROBE};
export default function () {{
  writeFileSync(OUT, JSON.stringify({{ probe, execPath: process.execPath }}));
}}
"#
        ),
        &[],
    );
    let mut want = golden_host_column();
    // The golden's run had the single argument `--flag`; here they are pi's real arguments.
    want.insert("process.argv.slice(2)".to_string(), serde_json::to_string(&args).unwrap());
    assert_eq!(as_rows(&value["probe"]), want);
    assert_eq!(value["execPath"], Path::new(PI).canonicalize().unwrap().display().to_string());
    assert_eq!(HOST_IDENTITY.versions()["bun"], want["process.versions.bun"]);
    assert_eq!(HOST_IDENTITY.entry, want["process.argv[1]"]);
}

#[test]
fn an_explicit_pi_binary_is_kept() {
    let (value, _) = run_extension(
        r#"import { writeFileSync } from "node:fs";
export default function () { writeFileSync(OUT, JSON.stringify(process.env.PI_SUBAGENT_PI_BINARY)); }
"#,
        &[("PI_SUBAGENT_PI_BINARY", "/opt/other-pi")],
    );
    assert_eq!(value, "/opt/other-pi");
}

/// `node_modules` of the installed package fixtures, or `None` (skip) without `PI_RS_PKG_FIXTURES`.
fn package_fixtures() -> Option<PathBuf> {
    std::env::var_os("PI_RS_PKG_FIXTURES")?;
    let root = std::env::var_os("PI_RS_PKG_FIXTURES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/pkg-fixtures"));
    let modules = root.join("agent/npm/node_modules");
    assert!(modules.join("pi-subagents").is_dir(), "{} has no pi-subagents; run scripts/package-fixtures.sh", modules.display());
    Some(modules)
}

/// pi-subagents takes its standalone (compiled-Pi) path: async runs are available and its child
/// processes are this executable; context-mode keeps `node:sqlite`.
#[test]
fn packages_detect_a_standalone_pi_host() {
    let Some(modules) = package_fixtures() else {
        eprintln!("skipped: set PI_RS_PKG_FIXTURES to run the package scenarios");
        return;
    };
    let module = |rel: &str| serde_json::to_string(&modules.join(rel)).unwrap();
    let (value, _) = run_extension(
        &format!(
            r#"import {{ writeFileSync }} from "node:fs";
import {{ pathToFileURL }} from "node:url";
const load = (path) => import(pathToFileURL(path).href);
export default async function () {{
  const spawn = await load({spawn});
  const asyncExecution = await load({async_execution});
  const db = await load({db_base});
  writeFileSync(OUT, JSON.stringify({{
    execPath: process.execPath,
    resolveBunPiExecutable: spawn.resolveBunPiExecutable(),
    spawnCommand: spawn.getPiSpawnCommand(["--version"]),
    isAsyncAvailable: asyncExecution.isAsyncAvailable(),
    hasModernSqlite: db.hasModernSqlite(),
    bun: typeof globalThis.Bun,
  }}));
}}
"#,
            spawn = module("pi-subagents/src/runs/shared/pi-spawn.js"),
            async_execution = module("pi-subagents/src/runs/background/async-execution.js"),
            db_base = module("context-mode/build/db-base.js"),
        ),
        &[],
    );
    let exe = value["execPath"].clone();
    assert_eq!(value["resolveBunPiExecutable"], exe, "{value:#}");
    assert_eq!(value["spawnCommand"], serde_json::json!({ "command": exe, "args": ["--version"] }), "{value:#}");
    assert_eq!(value["isAsyncAvailable"], true, "{value:#}");
    assert_eq!(value["hasModernSqlite"], true, "{value:#}");
    assert_eq!(value["bun"], "undefined", "{value:#}");
}
