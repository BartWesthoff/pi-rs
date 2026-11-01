//! `require()` of ES modules: synchronous evaluation as in Node, without running queued jobs.
//!
//! Expected values are Node v22's (`require()` of an ES module, `createRequire`).

use std::path::Path;

use pi_jsrt::{JsRuntime, RuntimeOptions};
use serde_json::{Value, json};

fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

fn runtime(root: &Path) -> JsRuntime {
    JsRuntime::new(RuntimeOptions {
        cwd: Some(root.to_path_buf()),
        ..Default::default()
    })
    .unwrap()
}

/// A module with top-level await that settles through a job (a failed dynamic import).
const TLA_MODULE: &str = r#"
let v;
try { v = (await import("./missing-dep.mjs")).x; } catch { v = "fallback"; }
export const value = v;
"#;

/// Requires a synchronous ES module while it is itself being evaluated.
const SYNC_REQUIRE_MODULE: &str = r#"
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
export const viaRequire = require("./plain.mjs").plain;
"#;

#[tokio::test]
async fn require_of_sync_esm_while_an_async_module_graph_is_evaluating() {
    // An extension whose graph has a top-level-await module next to a module that calls
    // `require()` on a synchronous ES module during its evaluation (as pi-ai's lazy TypeBox
    // `require` does inside `StringEnum`).
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "tla.mjs", TLA_MODULE);
    write(root, "sync-require.mjs", SYNC_REQUIRE_MODULE);
    write(root, "plain.mjs", "export const plain = 'plain-esm';\n");
    write(
        root,
        "entry.mjs",
        "import { value } from './tla.mjs';\nimport { viaRequire } from './sync-require.mjs';\nexport default { value, viaRequire };\n",
    );
    let rt = runtime(root);
    let ns = rt.import("./entry.mjs").await.unwrap();
    let rt_value = ns.get("default").await.unwrap();
    assert_eq!(rt_value, json!({ "value": "fallback", "viaRequire": "plain-esm" }));
}

#[tokio::test]
async fn require_of_esm_does_not_run_queued_microtasks() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "plain.mjs", "export const plain = 'plain-esm';\n");
    let rt = runtime(root);
    let order = rt
        .eval(
            r#"
(async () => {
  const order = [];
  Promise.resolve().then(() => order.push("microtask"));
  order.push("plain=" + require("./plain.mjs").plain);
  order.push("after-require");
  await new Promise((resolve) => setTimeout(resolve, 0));
  return JSON.stringify(order);
})()
"#,
        )
        .await
        .unwrap();
    let order: Value = serde_json::from_str(order.as_str().unwrap()).unwrap();
    assert_eq!(order, json!(["plain=plain-esm", "after-require", "microtask"]));
}

#[tokio::test]
async fn require_of_esm_graph_with_top_level_await_throws_err_require_async_module() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "tla.mjs", "globalThis.tlaRan = true;\nawait 0;\nexport const value = 1;\n");
    write(root, "imports-tla.mjs", "export { value } from './tla.mjs';\n");
    let rt = runtime(root);
    for target in ["./tla.mjs", "./imports-tla.mjs"] {
        let got = rt
            .eval(&format!(
                r#"
(() => {{
  try {{ require("{target}"); return "no error"; }}
  catch (e) {{ return JSON.stringify({{ code: e.code, name: e.name, first: e.message.split("\n")[0] }}); }}
}})()
"#
            ))
            .await
            .unwrap();
        let got: Value = serde_json::from_str(got.as_str().unwrap()).unwrap();
        assert_eq!(
            got,
            json!({
                "code": "ERR_REQUIRE_ASYNC_MODULE",
                "name": "Error",
                "first": "require() cannot be used on an ESM graph with top-level await. Use import() instead. To see where the top-level await comes from, use --experimental-print-required-tla."
            }),
            "require({target})"
        );
    }
    // Node checks the graph before evaluating it: the module body never ran.
    let ran = rt.eval("globalThis.tlaRan === undefined").await.unwrap();
    assert_eq!(ran.as_bool(), Some(true));
}

#[tokio::test]
async fn require_of_esm_being_evaluated_throws_err_require_cycle_module() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "cycle-a.mjs",
        r#"
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
let r;
try { require("./cycle-a.mjs"); r = "no error"; } catch (e) { r = { code: e.code, name: e.name }; }
export const result = r;
"#,
    );
    let rt = runtime(root);
    let ns = rt.import("./cycle-a.mjs").await.unwrap();
    let got = ns.get("result").await.unwrap();
    assert_eq!(got, json!({ "code": "ERR_REQUIRE_CYCLE_MODULE", "name": "Error" }));
}

#[tokio::test]
async fn require_of_esm_rethrows_its_evaluation_error_every_time() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "throws.mjs", "throw new Error('boom at eval');\n");
    let rt = runtime(root);
    let got = rt
        .eval(
            r#"
(async () => {
  const unhandled = [];
  process.on("unhandledRejection", (reason) => unhandled.push(String(reason)));
  const out = [];
  for (let i = 0; i < 2; i++) {
    try { require("./throws.mjs"); out.push("no error"); } catch (e) { out.push(e.message); }
  }
  await new Promise((resolve) => setTimeout(resolve, 10));
  return JSON.stringify({ out, unhandled });
})()
"#,
        )
        .await
        .unwrap();
    let got: Value = serde_json::from_str(got.as_str().unwrap()).unwrap();
    // A failed require() is a thrown error, not also an unhandled promise rejection.
    assert_eq!(got, json!({ "out": ["boom at eval", "boom at eval"], "unhandled": [] }));
}

#[tokio::test]
async fn require_of_already_evaluated_esm_returns_the_same_namespace() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "counter.mjs", "globalThis.evals = (globalThis.evals ?? 0) + 1;\nexport const n = 42;\n");
    write(
        root,
        "entry.mjs",
        r#"
import * as viaImport from "./counter.mjs";
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
const viaRequire = require("./counter.mjs");
export default { same: viaRequire === viaImport, n: viaRequire.n, evals: globalThis.evals };
"#,
    );
    let rt = runtime(root);
    let ns = rt.import("./entry.mjs").await.unwrap();
    let got = ns.get("default").await.unwrap();
    assert_eq!(got, json!({ "same": true, "n": 42, "evals": 1 }));
}
