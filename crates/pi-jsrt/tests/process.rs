//! Node mode (`pi_jsrt::node`, the `pi-jsrt-node` binary) and `child_process.fork()` with its IPC
//! channel. Expected outputs are Node 22's for the same scripts; with a `node` binary on PATH
//! the IPC channel is also checked against real Node on either end.

use std::path::Path;
use std::process::Command;

use pi_jsrt::{JsRuntime, RuntimeOptions};

const NODE_BIN: &str = env!("CARGO_BIN_EXE_pi-jsrt-node");

fn write(dir: &Path, name: &str, text: &str) -> String {
    let p = dir.join(name);
    std::fs::write(&p, text).unwrap();
    p.display().to_string()
}

fn run_node_mode(dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(NODE_BIN).args(args).current_dir(dir).output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

fn real_node() -> Option<String> {
    let out = Command::new("node").arg("-e").arg("process.stdout.write(process.execPath)").output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[test]
fn node_mode_event_loop_exit_codes_and_errors() {
    let dir = tempfile::tempdir().unwrap();
    let a = write(
        dir.path(),
        "a.js",
        r#"console.log("main", require.main === module, process.argv.length, typeof process.send);
setTimeout(() => console.log("timer"), 50);
const t = setTimeout(() => console.log("never"), 100000); t.unref();
process.on("beforeExit", (c) => { if (!globalThis.once) { globalThis.once = 1; console.log("beforeExit", c); setTimeout(() => console.log("more"), 10); } });
process.on("exit", (c) => console.log("exit", c));
process.exitCode = 3;
"#,
    );
    let (out, _, code) = run_node_mode(dir.path(), &[&a]);
    assert_eq!(out, "main true 2 undefined\ntimer\nbeforeExit 3\nmore\nexit 3\n");
    assert_eq!(code, 3);

    let b = write(dir.path(), "b.mjs", "await new Promise((r) => setTimeout(r, 10));\nPromise.reject(new Error(\"boom\"));\n");
    let (_, err, code) = run_node_mode(dir.path(), &[&b]);
    assert_eq!(code, 1);
    // QuickJS positions `new Error()` at the constructor name (V8: at `new`).
    assert!(err.starts_with(&format!("{b}:2\nPromise.reject(new Error(\"boom\"));\n                   ^\n\nError: boom\n")), "{err}");
    assert!(err.ends_with("\n\nNode.js v22.19.0\n"), "{err}");

    let c = write(
        dir.path(),
        "c.js",
        r#"process.on("uncaughtException", (e, origin) => console.log("caught", e.message, origin));
setTimeout(() => { throw new Error("late"); }, 1);
Promise.reject(42);
"#,
    );
    let (out, _, code) = run_node_mode(dir.path(), &[&c]);
    assert_eq!(
        out,
        "caught This error originated either by throwing inside of an async function without a catch block, or by rejecting a promise which was not handled with .catch(). The promise rejected with the reason \"42\". unhandledRejection\ncaught late uncaughtException\n"
    );
    assert_eq!(code, 0);

    let (out, _, code) = run_node_mode(dir.path(), &["-p", "process.argv.length + 40", "x"]);
    assert_eq!((out.as_str(), code), ("42\n", 0));
    let (out, _, code) = run_node_mode(dir.path(), &["-e", "process.exit(5); console.log('no')"]);
    assert_eq!((out.as_str(), code), ("", 5));
}

const PARENT: &str = r#"
const { fork } = require("child_process");
const path = require("path");
const opts = process.env.CHILD_EXEC ? { execPath: process.env.CHILD_EXEC } : {};
const child = fork(path.join(__dirname, "child.js"), ["x", "y"], { ...opts, env: { ...process.env, FOO: "bar" } });
const log = [];
child.on("message", (m) => {
  log.push(["parent got", m]);
  if (m.hello) child.send({ cmd: "echo", data: [1, "two", { three: 3 }, null, true] });
  if (m.echoed) child.send("bye");
});
child.on("disconnect", () => log.push(["disconnect", child.connected]));
child.on("exit", (code, sig) => { log.push(["exit", code, sig]); });
child.on("close", (code) => { log.push(["close", code]); console.log(JSON.stringify(log)); });
log.push(["connected", child.connected, typeof child.pid, child.stdio.length, child.stdout]);
try { child.send(undefined); } catch (e) { log.push([e.code, e.message]); }
try { child.send(Symbol("x")); } catch (e) { log.push([e.code, e.message]); }
"#;

const CHILD: &str = r#"
process.send({ hello: true, argv: process.argv.slice(2), foo: process.env.FOO, chan: process.env.NODE_CHANNEL_FD, connected: process.connected, execArgv: process.execArgv });
process.on("message", (m) => {
  if (m && m.cmd === "echo") process.send({ echoed: m.data }, (err) => process.send({ cb: err }));
  if (m === "bye") { process.disconnect(); process.exitCode = 7; }
});
process.on("disconnect", () => {});
"#;

/// Node 22's output of `PARENT` (any combination of Node and pi-jsrt on the two ends).
const FORK_LOG: &str = r#"[["connected",true,"number",4,null],["ERR_MISSING_ARGS","The \"message\" argument must be specified"],["ERR_INVALID_ARG_TYPE","The \"message\" argument must be one of type string, object, number, or boolean. Received type symbol (Symbol(x))"],["parent got",{"hello":true,"argv":["x","y"],"foo":"bar","connected":true,"execArgv":[]}],["parent got",{"echoed":[1,"two",{"three":3},null,true]}],["parent got",{"cb":null}],["disconnect",false],["exit",7,null],["close",7]]
"#;

#[test]
fn fork_ipc_between_node_mode_processes() {
    let dir = tempfile::tempdir().unwrap();
    let parent = write(dir.path(), "parent.js", PARENT);
    write(dir.path(), "child.js", CHILD);
    let (out, err, code) = run_node_mode(dir.path(), &[&parent]);
    assert_eq!(out, FORK_LOG, "{err}");
    assert_eq!(code, 0);
    if let Some(node) = real_node() {
        // pi-jsrt parent, Node child.
        let out = Command::new(NODE_BIN).arg(&parent).env("CHILD_EXEC", &node).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), FORK_LOG);
        // Node parent, pi-jsrt child.
        let out = Command::new(&node).arg(&parent).env("CHILD_EXEC", NODE_BIN).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), FORK_LOG);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_from_an_embedded_runtime() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "child.js", CHILD);
    write(
        dir.path(),
        "silent.js",
        r#"console.log("to parent stdout"); console.error("err line"); setTimeout(() => { throw new Error("child failure"); }, 5);"#,
    );
    let rt = JsRuntime::new(RuntimeOptions {
        cwd: Some(dir.path().to_path_buf()),
        node_exec: Some(vec![NODE_BIN.to_string()]),
        ..Default::default()
    })
    .unwrap();
    let src = PARENT.replace("console.log(JSON.stringify(log));", "done(JSON.stringify(log) + \"\\n\");");
    let src = format!(
        r#"import {{ createRequire }} from "node:module";
const require = createRequire(import.meta.url);
const __dirname = {dir:?};
let done; const finished = new Promise((r) => (done = r));
{src}
const silent = await new Promise((resolve) => {{
  const c = require("child_process").fork("silent.js", [], {{ silent: true }});
  let out = "", err = "";
  c.stdout.on("data", (d) => (out += d));
  c.stderr.on("data", (d) => (err += d));
  c.on("close", (code, signal) => resolve({{ out, err: err.split("\n").filter((l) => l === "err line" || l.startsWith("Error: ") || l.startsWith("Node.js")), code, signal }}));
}});
const errors = [];
for (const f of [() => require("child_process").fork(1), () => require("child_process").fork("x", {{ stdio: ["pipe", "pipe", "pipe"] }})]) {{
  try {{ f(); }} catch (e) {{ errors.push([e.code, e.message]); }}
}}
export default {{ log: await finished, silent, errors }};
"#,
        dir = dir.path().display().to_string()
    );
    let ns = tokio::time::timeout(std::time::Duration::from_secs(60), rt.eval_module(src, "fork-test.mjs"))
        .await
        .expect("timed out")
        .unwrap_or_else(|e| panic!("{e}\n{:?}", e.as_js().and_then(|j| j.stack.clone())));
    let v = ns.get("default").await.unwrap();
    assert_eq!(v["log"].as_str().unwrap(), FORK_LOG);
    assert_eq!(
        v["silent"],
        serde_json::json!({"out": "to parent stdout\n", "err": ["err line", "Error: child failure", "Node.js v22.19.0"], "code": 1, "signal": null})
    );
    assert_eq!(
        v["errors"],
        serde_json::json!([
            ["ERR_INVALID_ARG_TYPE", "The \"modulePath\" argument must be of type string or an instance of Buffer or URL. Received type number (1)"],
            ["ERR_CHILD_PROCESS_IPC_REQUIRED", "Forked processes must have an IPC channel, missing value 'ipc' in options.stdio"]
        ])
    );
}

#[test]
fn os_signals_reach_process_listeners() {
    use std::os::unix::process::ExitStatusExt;
    let dir = tempfile::tempdir().unwrap();
    // Listener, then default action once the last listener is removed; Node's errors.
    let sig = write(
        dir.path(),
        "sig.js",
        r#"process.on("SIGUSR2", (...a) => { console.log("got", JSON.stringify(a)); process.removeAllListeners("SIGUSR2"); setTimeout(() => { process.kill(process.pid, "SIGUSR2"); setTimeout(() => console.log("not reached"), 100); }, 10) });
process.kill(process.pid, "SIGUSR2");
try { process.on("SIGKILL", () => {}) } catch (e) { console.log(e.message, e.code, e.errno, e.syscall) }
try { process.kill(process.pid, "SIGFOO") } catch (e) { console.log(e.name, e.message, e.code) }
try { process.kill(999999, 0) } catch (e) { console.log(e.message, e.code, e.errno, e.syscall) }
console.log(process.kill(process.pid, 0), process.listenerCount("SIGKILL"));
"#,
    );
    let out = Command::new(NODE_BIN).arg(&sig).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("uv_signal_start EINVAL EINVAL -22 uv_signal_start\nTypeError Unknown signal: SIGFOO ERR_UNKNOWN_SIGNAL\nkill ESRCH ESRCH -3 kill\ntrue 0\ngot [\"SIGUSR2\",{}]\n", libc::SIGUSR2)
    );
    assert_eq!(out.status.signal(), Some(libc::SIGUSR2));

    let sig2 = write(
        dir.path(),
        "sig2.js",
        r#"let n = 0;
const h = (s) => { console.log("term", s, ++n); if (n === 2) { process.off("SIGTERM", h); } };
process.on("SIGTERM", h);
process.on("SIGINT", () => { console.log("int"); });
const t = setInterval(() => {}, 1000);
setTimeout(() => process.kill(process.pid, "SIGINT"), 20);
setTimeout(() => process.kill(process.pid, "SIGTERM"), 40);
setTimeout(() => process.kill(process.pid, "SIGTERM"), 60);
setTimeout(() => { console.log("still alive"); process.kill(process.pid, "SIGTERM"); }, 80);
setTimeout(() => console.log("not reached"), 300);
"#,
    );
    let out = Command::new(NODE_BIN).arg(&sig2).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "int\nterm SIGTERM 1\nterm SIGTERM 2\nstill alive\n");
    assert_eq!(out.status.signal(), Some(libc::SIGTERM));
}

fn run_with_stdin(dir: &Path, args: &[&str], input: &[u8]) -> (String, i32) {
    use std::io::Write;
    let mut child = Command::new(NODE_BIN)
        .args(args)
        .current_dir(dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let input = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let out = child.wait_with_output().unwrap();
    writer.join().unwrap();
    (String::from_utf8_lossy(&out.stdout).into_owned(), out.status.code().unwrap_or(-1))
}

#[test]
fn process_stdin_reads_the_real_standard_input() {
    let dir = tempfile::tempdir().unwrap();
    let input: String = (1..=20000).map(|i| format!("{i}\n")).collect();
    let a = write(
        dir.path(),
        "stdin.js",
        r#"let data = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (d) => (data += d));
process.stdin.on("end", () => console.log(JSON.stringify({ len: data.length, lines: data.split("\n").length, tty: process.stdin.isTTY ?? null, fd: process.stdin.fd })));
"#,
    );
    assert_eq!(run_with_stdin(dir.path(), &[&a], input.as_bytes()), ("{\"len\":108894,\"lines\":20001,\"tty\":null,\"fd\":0}\n".into(), 0));
    let b = write(
        dir.path(),
        "rl.js",
        r#"const rl = require("readline").createInterface({ input: process.stdin });
const lines = [];
rl.on("line", (l) => lines.push(l));
rl.on("close", () => console.log(JSON.stringify(lines)));
"#,
    );
    assert_eq!(run_with_stdin(dir.path(), &[&b], b"a\nb\nc"), ("[\"a\",\"b\",\"c\"]\n".into(), 0));
    // fs on the standard descriptors.
    assert_eq!(
        run_with_stdin(dir.path(), &["-e", "require('fs').writeSync(1, String(require('fs').readFileSync(0, 'utf8').length) + '\\n')"], b"12345"),
        ("5\n".into(), 0)
    );
    // An unread stdin does not keep the process alive (the writer side stays open here).
    let c = write(dir.path(), "noread.js", "console.log(typeof process.stdin.on);");
    let mut child = Command::new(NODE_BIN).arg(&c).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break Some(s);
        }
        if started.elapsed() > std::time::Duration::from_secs(20) {
            let _ = child.kill();
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert_eq!(status.and_then(|s| s.code()), Some(0));
}

/// `process.env` is live, as in Node: values become strings, and assigning `TZ` changes the
/// runtime's timezone (deleting it returns to the host's). Expected values are Node 22's.
#[tokio::test]
async fn process_env_tz_sets_the_runtime_timezone() {
    let mut env: std::collections::HashMap<String, String> = std::env::vars().collect();
    env.insert("TZ".into(), "UTC".into());
    let rt = JsRuntime::new(RuntimeOptions { env: Some(env), ..Default::default() }).unwrap();
    let value = rt
        .eval(
            r#"(() => {
        const read = () => [new Date(0).getHours(), new Date(0).getTimezoneOffset()];
        const utc = read();
        process.env.TZ = "Asia/Kolkata";
        const kolkata = read();
        process.env.TZ = "America/New_York";
        const july = new Date(Date.UTC(2026, 6, 1)).getTimezoneOffset();
        process.env.TZ = "";
        const empty = read();
        process.env.COUNT = 3;
        return [utc, kolkata, july, empty, typeof process.env.COUNT, process.env.TZ];
    })()"#,
        )
        .await
        .unwrap();
    assert_eq!(value, serde_json::json!([[0, 0], [5, -330], 240, [0, 0], "string", ""]));
}

/// `process.execPath` is pi's own binary, which is also a Node: `spawn`/`execFileSync` with it
/// and a script, or with a Node CLI flag first, re-enter Node mode instead of starting the pi
/// agent. Anything else (`--help`, a non-script argument) still runs the pi CLI.
#[test]
fn exec_path_as_node_rewrites_scripts_and_node_cli_flags() {
    let dir = tempfile::tempdir().unwrap();
    let driver = write(
        dir.path(),
        "driver.js",
        r#"const { execFileSync, spawnSync } = require("node:child_process");
const out = {};
out.node = process.execPath;
out.p = execFileSync(process.execPath, ["-p", "process.execPath"], { encoding: "utf8" }).trim();
out.version = execFileSync(process.execPath, ["--version"], { encoding: "utf8" }).trim();
out.eval = execFileSync(process.execPath, ["-e", "process.stdout.write('ev')"], { encoding: "utf8" });
const bad = spawnSync(process.execPath, ["--check", "bad.js"], { encoding: "utf8" });
out.badStatus = bad.status;
out.badStderr = /SyntaxError/.test(bad.stderr);
out.okStatus = spawnSync(process.execPath, ["--check", "ok.js"], { encoding: "utf8" }).status;
out.script = execFileSync(process.execPath, ["child.js"], { encoding: "utf8" }).trim();
console.log(JSON.stringify(out));
"#,
    );
    write(dir.path(), "ok.js", "const a = 1;\n");
    write(dir.path(), "bad.js", "const a = (;\n");
    write(dir.path(), "child.js", "import('node:fs').then(() => process.stdout.write('script\\n'));\n");
    let (stdout, stderr, code) = run_node_mode(dir.path(), &[&driver]);
    assert_eq!(code, 0, "{stderr}");
    let out: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(out["p"], out["node"], "`-p process.execPath` prints the pi path");
    assert_eq!(out["version"], "v22.19.0");
    assert_eq!(out["eval"], "ev");
    assert_eq!(out["okStatus"], 0);
    assert_eq!(out["badStatus"], 1);
    assert_eq!(out["badStderr"], true);
    assert_eq!(out["script"], "script");
}

/// Arguments that are neither a Node CLI flag nor a script path are left alone: `--help` is
/// not rewritten into Node mode (the pi CLI keeps it; `pi-jsrt-node`, whose `execPath` is not
/// the pi binary, prints nothing for it).
#[test]
fn exec_path_without_node_args_is_not_rewritten() {
    let dir = tempfile::tempdir().unwrap();
    let driver = write(
        dir.path(),
        "driver.js",
        r#"const { spawnSync } = require("node:child_process");
const help = spawnSync(process.execPath, ["--help"], { encoding: "utf8" });
const plain = spawnSync(process.execPath, ["not-a-script"], { encoding: "utf8" });
console.log(JSON.stringify({ help: help.status, helpNode: /__pi_jsrt_node__/.test(help.stdout ?? ""), plain: plain.status }));
"#,
    );
    let (stdout, stderr, code) = run_node_mode(dir.path(), &[&driver]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout.trim(), r#"{"help":0,"helpNode":false,"plain":1}"#);
}

/// `detached:true` is Node's: the child leads a new session (`getsid(pid) == pid`), so it
/// survives the parent's exit.
#[test]
fn detached_child_leads_its_own_session() {
    let dir = tempfile::tempdir().unwrap();
    let child = write(
        dir.path(),
        "child.js",
        r#"require("node:fs").writeFileSync(process.argv[2], String(process.pid));
setTimeout(() => {}, 60000);
"#,
    );
    let driver = write(
        dir.path(),
        "driver.js",
        r#"const { spawn } = require("node:child_process");
const d = spawn(process.execPath, [process.argv[2], process.argv[3]], { detached: true, stdio: "ignore" });
d.unref();
"#,
    );
    let pid_file = dir.path().join("pid");
    let (_, stderr, code) = run_node_mode(dir.path(), &[&driver, &child, &pid_file.display().to_string()]);
    assert_eq!(code, 0, "{stderr}");
    // The detached child outlives the parent, so it may still be starting when the parent exits.
    let started = std::time::Instant::now();
    let pid: i32 = loop {
        if let Ok(text) = std::fs::read_to_string(&pid_file) {
            break text.trim().parse().unwrap();
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(20), "detached child never wrote its pid");
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    // SAFETY: getsid is a plain syscall on a pid the test just created.
    let sid = unsafe { libc::getsid(pid) };
    // SAFETY: kill on a live child the test started; SIGKILL because the child just sleeps.
    unsafe { libc::kill(pid, libc::SIGKILL) };
    assert_eq!(sid, pid, "the detached child is its own session leader");
}

/// Differential: the same `--check` / `-p` / `--version` / `-e` invocations as real Node. Exit
/// codes must match; stdout matches exactly for `-p`/`-e`, and for `--version` in shape (the
/// version number itself differs: pi-rs reports pi's `engines` minimum, `v22.19.0`).
#[test]
fn node_cli_flags_match_real_node() {
    let Some(node) = real_node() else { return };
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "ok.js", "const a = 1;\n");
    write(dir.path(), "bad.js", "const a = (;\n");
    let cases: &[&[&str]] = &[
        &["--check", "ok.js"],
        &["--check", "bad.js"],
        &["-c", "ok.js"],
        &["-p", "1+1"],
        &["--print", "'x' + 'y'"],
        &["-e", "process.stdout.write('e')"],
        &["--check", "missing.js"],
    ];
    for args in cases {
        let real = Command::new(&node).args(*args).current_dir(dir.path()).output().unwrap();
        let (ours_out, ours_err, ours_code) = run_node_mode(dir.path(), args);
        let real_code = real.status.code().unwrap_or(-1);
        assert_eq!(ours_code, real_code, "{args:?}: exit code");
        if !args.contains(&"bad.js") && !args.contains(&"missing.js") {
            assert_eq!(ours_out, String::from_utf8_lossy(&real.stdout), "{args:?}: stdout");
        }
        // The error shape: Node prints `SyntaxError` (or MODULE_NOT_FOUND) before its version line.
        if args.contains(&"bad.js") {
            assert!(ours_err.contains("SyntaxError"), "{args:?}: {ours_err}");
            assert!(String::from_utf8_lossy(&real.stderr).contains("SyntaxError"));
        }
    }
    // `--version` prints the Node version on both, and pi-rs reports the engines minimum.
    let (version, _, code) = run_node_mode(dir.path(), &["--version"]);
    assert_eq!(code, 0);
    assert_eq!(version.trim(), "v22.19.0");
    let real_version = Command::new(&node).arg("--version").output().unwrap();
    assert!(String::from_utf8_lossy(&real_version.stdout).starts_with('v'));
}

/// `--import` preloads the module before the main one, `--experimental-strip-types` is accepted
/// (TypeScript is always stripped), and `--input-type=module` evaluates `-e` as an ES module.
#[test]
fn node_mode_accepts_import_strip_types_and_input_type() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "pre.mjs", "globalThis.preloaded = 'yes';\n");
    let main = write(dir.path(), "main.mjs", "console.log(String(globalThis.preloaded));\n");
    assert_eq!(run_node_mode(dir.path(), &["--import", "./pre.mjs", &main]).0, "yes\n");
    assert_eq!(run_node_mode(dir.path(), &["--experimental-strip-types", "-p", "1+1"]).0, "2\n");
    assert_eq!(run_node_mode(dir.path(), &["--input-type=module", "-e", "console.log('mod')"]).0, "mod\n");
}
