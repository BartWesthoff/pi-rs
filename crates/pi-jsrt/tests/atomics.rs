//! `SharedArrayBuffer` shared across worker threads, blocking `Atomics.wait` on every thread (the
//! main one included, as in Node), `Atomics.waitAsync`, and the packages built on them (synckit,
//! recheck). Scripts run in Node mode (`pi-jsrt-node`); the expected output is Node 22's.
//!
//! The package tests are ignored by default. They need the pinned npm fixtures of
//! `tests/fixtures/pkgp06` installed under `$PI_RS_PKG_FIXTURES/pkgp06` (npm checks every tarball
//! against the lockfile's integrity hash):
//!
//! ```sh
//! mkdir -p "$PI_RS_PKG_FIXTURES/pkgp06"
//! cp crates/pi-jsrt/tests/fixtures/pkgp06/package*.json "$PI_RS_PKG_FIXTURES/pkgp06/"
//! npm ci --ignore-scripts --no-audit --no-fund --prefix "$PI_RS_PKG_FIXTURES/pkgp06"
//! PI_RS_PKG_FIXTURES=... cargo test -p pi-jsrt --test atomics -- --ignored
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use pi_jsrt::{Error, JsRuntime, RuntimeOptions};
use serde_json::json;

const NODE_BIN: &str = env!("CARGO_BIN_EXE_pi-jsrt-node");

/// Run the files (the first is the entry point) in a temporary directory; stdout + stderr and
/// the exit code. `node_modules`, when given, is linked into the directory.
fn run_files(files: &[(&str, &str)], node_modules: Option<&Path>, env: &[(&str, &str)]) -> (String, Option<i32>) {
    let dir = tempfile::tempdir().unwrap();
    for (name, source) in files {
        std::fs::write(dir.path().join(name), source).unwrap();
    }
    if let Some(modules) = node_modules {
        std::os::unix::fs::symlink(modules, dir.path().join("node_modules")).unwrap();
    }
    let mut cmd = Command::new(NODE_BIN);
    cmd.arg(dir.path().join(files[0].0)).current_dir(dir.path());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (text.replace(&dir.path().display().to_string(), "DIR"), out.status.code())
}

fn run(name: &str, script: &str) -> String {
    let (text, code) = run_files(&[(name, script)], None, &[]);
    assert_eq!(code, Some(0), "{text}");
    text
}

#[test]
fn worker_shares_memory_and_main_thread_blocks_in_wait() {
    let script = r###"const { Worker, isMainThread, workerData, parentPort } = require("worker_threads");
if (isMainThread) {
  const sab = new SharedArrayBuffer(16);
  const i32 = new Int32Array(sab);
  const w = new Worker(__filename, { workerData: { i32, sab } });
  const t0 = Date.now();
  console.log("wait-notify", Atomics.wait(i32, 0, 0, 5000), i32[0], i32[1]);
  console.log("wait-timeout", Atomics.wait(i32, 2, 0, 50), Date.now() - t0 < 4000);
  w.postMessage({ again: i32 });
  const r = Atomics.waitAsync(i32, 3, 0, 5000);
  console.log("async", r.async);
  r.value.then((v) => { console.log("waitAsync", v, i32[3]); });
  w.on("exit", (c) => console.log("exit", c, i32[0], i32[1]));
} else {
  const { i32, sab } = workerData;
  console.log("worker sees shared", i32.buffer === sab, i32.buffer instanceof SharedArrayBuffer);
  for (let k = 0; k < 1000; k++) Atomics.add(i32, 1, 1);
  Atomics.store(i32, 0, 1);
  Atomics.notify(i32, 0);
  parentPort.once("message", ({ again }) => {
    setTimeout(() => { Atomics.store(again, 3, 7); console.log("notified", Atomics.notify(again, 3)); parentPort.close(); }, 20);
  });
}
"###;
    let expected = r###"wait-notify ok 1 1000
wait-timeout timed-out true
async true
worker sees shared true true
waitAsync ok 7
notified 1
exit 0 1 1000
"###;
    assert_eq!(run("sab.js", script), expected);
}

#[test]
fn terminate_wakes_a_blocked_worker_and_a_crashed_worker_times_out() {
    let script = r###"const { Worker } = require("worker_threads");
const i32 = new Int32Array(new SharedArrayBuffer(8));
const w = new Worker("const { workerData: a } = require('worker_threads'); Atomics.store(a, 1, 1); Atomics.notify(a, 1); try { Atomics.wait(a, 0, 0); } catch (e) { console.log('caught', e.message); } console.log('after wait');", { eval: true, workerData: i32 });
w.on("error", (e) => console.log("error", e.message));
w.on("exit", (c) => console.log("exit", c));
console.log("started", Atomics.wait(i32, 1, 0, 5000));
setTimeout(async () => console.log("terminate ->", await w.terminate()), 50);
const i2 = new Int32Array(new SharedArrayBuffer(4));
const crash = new Worker("throw new Error('crash before notify')", { eval: true, workerData: i2 });
crash.on("error", (e) => console.log("crash error", e.message));
const t0 = Date.now();
console.log("crash wait", Atomics.wait(i2, 0, 0, 300), Date.now() - t0 >= 250);
"###;
    let expected = r###"started ok
crash wait timed-out true
crash error crash before notify
exit 1
terminate -> 1
"###;
    assert_eq!(run("term.js", script), expected);
}

#[test]
fn wait_async_clone_and_port_semantics() {
    let script = r###"const { MessageChannel, receiveMessageOnPort } = require("worker_threads");
const sab = new SharedArrayBuffer(16);
const c = structuredClone(sab);
new Int32Array(c)[0] = 7;
console.log("clone", c === sab, c instanceof SharedArrayBuffer, c.byteLength, new Int32Array(sab)[0]);
for (const f of [() => structuredClone(sab, { transfer: [sab] }), () => new MessageChannel().port1.postMessage(sab, [sab])]) {
  try { f(); console.log("no error"); } catch (e) { console.log(e.name, e.message); }
}
const { port1, port2 } = new MessageChannel();
port1.postMessage({ view: new Int32Array(sab, 4, 2), sab, again: sab });
const { message: m } = receiveMessageOnPort(port2);
m.view[1] = 9;
console.log("port", m.sab === m.again, m.view.buffer === m.sab, m.view.byteOffset, new Int32Array(sab)[2]);
port1.close();
const plain = new Int32Array(new ArrayBuffer(8));
for (const f of [() => Atomics.wait(plain, 0, 0, 0), () => Atomics.waitAsync(plain, 0, 0, 0), () => Atomics.waitAsync(new Float64Array(new SharedArrayBuffer(8)), 0, 0), () => Atomics.waitAsync(new Int32Array(sab), 4, 0)]) {
  try { f(); console.log("no error"); } catch (e) { console.log(e.name); }
}
const s = new Int32Array(new SharedArrayBuffer(16));
console.log("wait", Atomics.wait(s, 0, 0, 0), Atomics.wait(s, 0, 1, 0));
console.log(JSON.stringify(Atomics.waitAsync(s, 0, 0, 0)), JSON.stringify(Atomics.waitAsync(s, 0, 1)));
const b = new BigInt64Array(new SharedArrayBuffer(16));
console.log(JSON.stringify(Atomics.waitAsync(b, 0, 1n)));
(async () => {
  const r = Atomics.waitAsync(s, 1, 0, 1000);
  console.log("async", r.async, r.value instanceof Promise, Object.keys(r).join());
  console.log("notify", Atomics.notify(s, 1, 1), Atomics.notify(s, 1));
  console.log("settled", await r.value);
  const big = Atomics.waitAsync(b, 1, 0n);
  Atomics.store(b, 1, 5n);
  console.log("notify big", Atomics.notify(b, 1));
  console.log("settled big", await big.value);
  const many = [0, 1, 2].map(() => Atomics.waitAsync(s, 2, 0).value);
  console.log("notify two", Atomics.notify(s, 2, 2));
  console.log("settled two", await Promise.race([Promise.all(many.slice(0, 2)), new Promise((r) => setTimeout(() => r("stuck"), 500))]));
  console.log("notify rest", Atomics.notify(s, 2));
  await many[2];
  console.log("timeout", await Atomics.waitAsync(s, 3, 0, 20).value);
  const pending = Atomics.waitAsync(s, 3, 0, 10000);
  pending.value.then((v) => console.log("never", v));
  console.log("end");
})();
"###;
    // The pending 10 s waitAsync does not keep the process alive.
    let expected = r###"clone false true 16 7
DataCloneError Found invalid value in transferList.
DataCloneError Found invalid value in transferList.
port true true 4 9
TypeError
TypeError
TypeError
RangeError
wait timed-out not-equal
{"async":false,"value":"timed-out"} {"async":false,"value":"not-equal"}
{"async":false,"value":"not-equal"}
async true true async,value
notify 1 0
settled ok
notify big 1
settled big ok
notify two 2
settled two [ 'ok', 'ok' ]
notify rest 1
timeout timed-out
end
"###;
    let started = Instant::now();
    assert_eq!(run("wa.js", script), expected);
    assert!(started.elapsed() < Duration::from_secs(8), "a pending waitAsync kept the process alive");
}

#[test]
fn worker_require_preloads_from_exec_argv_and_node_options() {
    let pre = r###"console.log("pre", require("worker_threads").isMainThread, JSON.stringify(require("worker_threads").workerData));
"###;
    let script = r###"const { Worker } = require("worker_threads");
const path = require("path");
const run = (opts) => new Promise((res) => { const w = new Worker("console.log('body', process.execArgv.length)", { eval: true, workerData: 7, ...opts }); w.on("error", (e) => console.log("err", e.message)); w.on("exit", res); });
(async () => {
  await run({ execArgv: ["-r", path.join(__dirname, "pre.cjs")] });
  await run({ env: { ...process.env, NODE_OPTIONS: `-r ${JSON.stringify(path.join(__dirname, "pre.cjs"))}` } });
  await run({ env: { ...process.env, NODE_OPTIONS: "--require=./pre.cjs" } });
  await run({});
})();
"###;
    let expected = r###"pre false 7
body 2
pre false 7
body 0
pre false 7
body 0
body 0
"###;
    let (text, code) = run_files(&[("main.js", script), ("pre.cjs", pre)], None, &[]);
    assert_eq!(code, Some(0), "{text}");
    assert_eq!(text, expected);
}

#[tokio::test]
async fn embedded_runtimes_block_in_wait_and_shutdown_wakes_them() {
    let rt = JsRuntime::new(RuntimeOptions::default()).unwrap();
    let v = rt.eval("Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 10)").await.unwrap();
    assert_eq!(v, json!("timed-out"));

    // A runtime blocked forever in `Atomics.wait` still shuts down.
    let blocked = rt.clone();
    let waiting = tokio::spawn(async move { blocked.eval("Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0)").await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    rt.shutdown();
    let result = tokio::time::timeout(Duration::from_secs(5), waiting).await.expect("shutdown woke the wait").unwrap();
    assert!(matches!(result, Err(Error::Terminated | Error::Js(_))), "{result:?}");
}

/// `$PI_RS_PKG_FIXTURES/pkgp06/node_modules` (see the module docs).
fn fixtures() -> PathBuf {
    let root = std::env::var_os("PI_RS_PKG_FIXTURES").expect("PI_RS_PKG_FIXTURES is not set (see tests/atomics.rs)");
    let modules = PathBuf::from(root).join("pkgp06/node_modules");
    assert!(modules.join("synckit/package.json").is_file(), "fixtures missing in {}", modules.display());
    modules
}

const SYNCKIT_WORKER: &str = r###"import { runAsWorker } from "synckit";
runAsWorker(async (a, b) => {
  await new Promise((r) => setTimeout(r, 10));
  if (a === "boom") throw Object.assign(new TypeError("bad input"), { code: "E_BAD" });
  if (a === "exit") process.exit(3);
  if (a === "throw") setTimeout(() => { throw new Error("async crash"); }, 1);
  if (a === "exit" || a === "throw") await new Promise(() => {});
  return { sum: a + b, nested: [a, b], when: "async" };
});
"###;

#[test]
#[ignore = "needs the npm fixtures in PI_RS_PKG_FIXTURES (see module docs)"]
fn synckit_create_sync_fn_round_trips() {
    let main = r###"import { createSyncFn } from "synckit";
import { fileURLToPath } from "node:url";
const fn = createSyncFn(fileURLToPath(new URL("./worker.mjs", import.meta.url)));
console.log(JSON.stringify(fn(1, 2)));
console.log(JSON.stringify(fn(40, 2)));
try { fn("boom"); } catch (e) { console.log("error", e.name, e.message, e.code); }
console.log("done");
"###;
    let expected = r###"{"sum":3,"nested":[1,2],"when":"async"}
{"sum":42,"nested":[40,2],"when":"async"}
error TypeError bad input E_BAD
done
"###;
    let modules = fixtures();
    let (text, code) = run_files(&[("main.mjs", main), ("worker.mjs", SYNCKIT_WORKER)], Some(&modules), &[]);
    assert_eq!((text.as_str(), code), (expected, Some(0)));
}

#[test]
#[ignore = "needs the npm fixtures in PI_RS_PKG_FIXTURES (see module docs)"]
fn synckit_reports_worker_crashes_without_deadlock() {
    let main = r###"import { createSyncFn } from "synckit";
import { fileURLToPath } from "node:url";
const fn = createSyncFn(fileURLToPath(new URL(process.argv[2], import.meta.url)));
const t0 = Date.now();
try { fn(process.argv[3]); } catch (e) { console.log("error:", e.message); }
console.log("returned", Date.now() - t0 < 3000 ? "fast" : "slow");
"###;
    let modules = fixtures();
    let cases = [
        ("./worker.mjs", "exit", "error: Worker exited with code 3\nreturned fast\n"),
        ("./worker.mjs", "throw", "error: async crash\nreturned fast\n"),
        // Only synckit's `-r register.cjs` preload (via the worker's NODE_OPTIONS) sees this one.
        ("./broken.mjs", "", "error: load failure\nreturned fast\n"),
    ];
    for (worker, mode, expected) in cases {
        let dir = tempfile::tempdir().unwrap();
        for (name, source) in [("main.mjs", main), ("worker.mjs", SYNCKIT_WORKER), ("broken.mjs", "throw new Error(\"load failure\");\n")] {
            std::fs::write(dir.path().join(name), source).unwrap();
        }
        std::os::unix::fs::symlink(&modules, dir.path().join("node_modules")).unwrap();
        let out = Command::new(NODE_BIN).arg("main.mjs").arg(worker).arg(mode).current_dir(dir.path()).output().unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert_eq!((text.as_str(), out.status.code()), (expected, Some(0)), "{worker} {mode}");
    }
}

#[test]
#[ignore = "needs the npm fixtures in PI_RS_PKG_FIXTURES (see module docs)"]
fn recheck_check_sync_uses_the_native_binary() {
    let main = r###"const { checkSync } = require("recheck");
const r = checkSync("^(a+)+$", "");
console.log(r.status, r.checker, r.complexity.type, typeof r.attack.string);
console.log(checkSync("^a+$", "").status);
"###;
    let modules = fixtures();
    assert!(modules.join("recheck-linux-x64/recheck").is_file(), "recheck-linux-x64 binary missing");
    // `native` fails instead of falling back to the pure-JS checker when the binary cannot run.
    let (text, code) = run_files(&[("main.cjs", main)], Some(&modules), &[("RECHECK_BACKEND", "native")]);
    assert_eq!((text.as_str(), code), ("vulnerable automaton exponential string\nsafe\n", Some(0)));
}
