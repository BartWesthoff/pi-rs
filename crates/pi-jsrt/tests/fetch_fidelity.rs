//! `fetch` fidelity for package traffic: the server's own `statusText`, streamed request bodies
//! (`ReadableStream`/async iterables with `duplex: "half"`), npm undici dispatchers (`Agent`,
//! `ProxyAgent`, `EnvHttpProxyAgent`, `setGlobalDispatcher`) applied natively, custom CAs and
//! `NODE_EXTRA_CA_CERTS`. Loopback mock servers (plain HTTP, HTTPS with a test CA, and a proxy that
//! logs what it is asked for) serve both runtimes; the expected values are Node 22's output, and
//! with `node` on PATH every scenario is also run under Node and compared.
//!
//! The undici tests are ignored by default. They need the pinned npm fixtures of
//! `tests/fixtures/pkgp14` installed under `$PI_RS_PKG_FIXTURES/pkgp14` (npm checks every tarball
//! against the lockfile's integrity hash):
//!
//! ```sh
//! mkdir -p "$PI_RS_PKG_FIXTURES/pkgp14"
//! cp crates/pi-jsrt/tests/fixtures/pkgp14/package*.json "$PI_RS_PKG_FIXTURES/pkgp14/"
//! npm ci --ignore-scripts --no-audit --no-fund --prefix "$PI_RS_PKG_FIXTURES/pkgp14"
//! PI_RS_PKG_FIXTURES=... cargo test -p pi-jsrt --test fetch_fidelity -- --ignored
//! ```

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

const NODE_BIN: &str = env!("CARGO_BIN_EXE_pi-jsrt-node");

/// The test CA and a `localhost`/`127.0.0.1` leaf it signed (shared with `http2.rs`).
const CA: &str = "-----BEGIN CERTIFICATE-----\nMIIBijCCATGgAwIBAgIUVs+EfMS/+Ro7R0lcA4uc/AhPkfcwCgYIKoZIzj0EAwIw\nGjEYMBYGA1UEAwwPcGktanNydCB0ZXN0IENBMCAXDTI2MTAyMjE0NTQ1NloYDzIx\nMjYwOTI4MTQ1NDU2WjAaMRgwFgYDVQQDDA9waS1qc3J0IHRlc3QgQ0EwWTATBgcq\nhkjOPQIBBggqhkjOPQMBBwNCAAQXCcWuLg51sJCkC2Jjdx3JnX9g30cJm2/Q+ntm\nIKTAK1Qf33OATy1sjP94DopCai3wP+eOi5/On5xoGRfrwSuLo1MwUTAdBgNVHQ4E\nFgQUT/pxUc0B07VmR1bUJ1CjH3qtNwYwHwYDVR0jBBgwFoAUT/pxUc0B07VmR1bU\nJ1CjH3qtNwYwDwYDVR0TAQH/BAUwAwEB/zAKBggqhkjOPQQDAgNHADBEAiBAJYPI\nkFx24T48mhhbpia1ZkznObsjhiqwHClxPMIeDgIgJThIRiRm0g2PuaoNQFBJzvKx\nnT4Y2EGoPBoqvDax6+k=\n-----END CERTIFICATE-----\n";
const LEAF_KEY: &str = "-----BEGIN EC PRIVATE KEY-----\nMHcCAQEEIDMUcB3eZ4nh1AzrsEMvsmqcOxfnBpjfLWJUJWivV79QoAoGCCqGSM49\nAwEHoUQDQgAECsujzoaPh8daHDbzr+W8cUzLjRqqoXS57N2//PYAZ7IMr47Pkxto\nbCvCjp2YpgHhoxfaNUQSnnpnt88TTmKQew==\n-----END EC PRIVATE KEY-----\n";
const LEAF_CERT: &str = "-----BEGIN CERTIFICATE-----\nMIIByjCCAW+gAwIBAgIUY2YuzVK4HDbIzlyNdAw/u74U/7UwCgYIKoZIzj0EAwIw\nGjEYMBYGA1UEAwwPcGktanNydCB0ZXN0IENBMCAXDTI2MTAyMjE0NTQ1NloYDzIx\nMjYwOTI4MTQ1NDU2WjAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwWTATBgcqhkjOPQIB\nBggqhkjOPQMBBwNCAAQKy6POho+Hx1ocNvOv5bxxTMuNGqqhdLns3b/89gBnsgyv\njs+TG2hsK8KOnZimAeGjF9o1RBKeeme3zxNOYpB7o4GWMIGTMBoGA1UdEQQTMBGC\nCWxvY2FsaG9zdIcEfwAAATAJBgNVHRMEAjAAMAsGA1UdDwQEAwIHgDAdBgNVHSUE\nFjAUBggrBgEFBQcDAQYIKwYBBQUHAwIwHQYDVR0OBBYEFEnI8FK6Ovo9sspTt0r9\nUzpPhZDQMB8GA1UdIwQYMBaAFE/6cVHNAdO1ZkdW1CdQox96rTcGMAoGCCqGSM49\nBAMCA0kAMEYCIQCSxhvJ5bcdbziBydqRjR7cFGlcu7mBHseV5NDOkRkd7QIhAK0K\nHb6U4U6rbzNvIl+I/L+wKz97yKaglZ5BgB0aHSxB\n-----END CERTIFICATE-----\n";

// ---------------------------------------------------------------------------
// Mock servers
// ---------------------------------------------------------------------------

/// How long `/slow` and `/slowbody` stall; far above the 100 ms timeouts the scripts set, so a
/// loaded machine cannot read the stalled part in time.
const STALL: Duration = Duration::from_millis(3000);

/// Read an HTTP/1 head: (request line, lowercase headers).
fn read_head(r: &mut impl BufRead) -> Option<(String, Vec<(String, String)>)> {
    let mut line = String::new();
    if r.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).ok()? == 0 || h.trim_end().is_empty() {
            break;
        }
        if let Some((k, v)) = h.trim_end().split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    Some((line.trim_end().to_string(), headers))
}

fn header<'h>(headers: &'h [(String, String)], name: &str) -> Option<&'h str> {
    headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

fn read_body(r: &mut impl BufRead, headers: &[(String, String)]) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    if header(headers, "transfer-encoding") == Some("chunked") {
        loop {
            let mut size = String::new();
            r.read_line(&mut size).ok()?;
            let n = usize::from_str_radix(size.trim(), 16).ok()?;
            let mut chunk = vec![0; n + 2];
            r.read_exact(&mut chunk).ok()?;
            if n == 0 {
                return Some(body);
            }
            body.extend_from_slice(&chunk[..n]);
        }
    }
    let n: usize = header(headers, "content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    body.resize(n, 0);
    r.read_exact(&mut body).ok()?;
    Some(body)
}

/// One request per connection: echo what arrived as JSON, under the status line the path asks for.
fn serve_http(stream: impl Read + Write) {
    let mut r = BufReader::new(stream);
    let Some((line, headers)) = read_head(&mut r) else { return };
    let mut parts = line.split(' ');
    let (method, path) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
    let Some(body) = read_body(&mut r, &headers) else { return };
    let json_str = |v: Option<&str>| v.map_or("null".to_string(), |s| serde_json::to_string(s).unwrap());
    let out = format!(
        r#"{{"method":{},"path":{},"te":{},"cl":{},"body":{}}}"#,
        json_str(Some(&method)),
        json_str(Some(&path)),
        json_str(header(&headers, "transfer-encoding")),
        json_str(header(&headers, "content-length")),
        json_str(Some(&String::from_utf8_lossy(&body))),
    );
    let status = match path.as_str() {
        "/404" => "404 Not Found",
        "/custom" => "404 Nothing To See",
        "/empty" => "404 ",
        "/teapot" => "418 I'm a teapot",
        "/odd" => "299 Whatever You Say",
        _ => "200 OK",
    };
    let w = r.get_mut();
    // Each response goes out in one write: undici keeps only the last fragment of a reason
    // phrase split across reads.
    if path == "/slow" {
        std::thread::sleep(STALL);
    }
    if path == "/slowbody" {
        let _ = w.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 10\r\n\r\nab");
        let _ = w.flush();
        std::thread::sleep(STALL);
        let _ = w.write_all(b"cdefghij");
        return;
    }
    let _ = w.write_all(format!("HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{out}", out.len()).as_bytes());
    let _ = w.flush();
}

fn listen(handler: impl Fn(TcpStream) + Send + Sync + 'static) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handler = Arc::new(handler);
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let handler = handler.clone();
            std::thread::spawn(move || handler(conn));
        }
    });
    port
}

fn tls_config() -> Arc<rustls::ServerConfig> {
    let certs = vec![CertificateDer::from_pem_slice(LEAF_CERT.as_bytes()).unwrap()];
    let key = PrivateKeyDer::from_pem_slice(LEAF_KEY.as_bytes()).unwrap();
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    Arc::new(config)
}

/// `host:PORT` with the port masked, for logs that must not depend on ephemeral ports.
fn mask_port(target: &str) -> String {
    match target.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => format!("{host}:PORT"),
        _ => target.to_string(),
    }
}

/// A forward proxy: CONNECT tunnels and absolute-form requests. Logs
/// `METHOD target proxy-authorization` per request (ports masked).
fn proxy(log: Arc<Mutex<Vec<String>>>) -> u16 {
    listen(move |client| {
        let mut r = BufReader::new(client.try_clone().unwrap());
        let Some((line, headers)) = read_head(&mut r) else { return };
        let mut parts = line.split(' ');
        let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        let auth = header(&headers, "proxy-authorization").unwrap_or("-");
        let logged = match target.strip_prefix("http://") {
            Some(rest) => {
                let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
                format!("{method} http://{}/{path} {auth}", mask_port(authority))
            }
            None => format!("{method} {} {auth}", mask_port(target)),
        };
        log.lock().unwrap().push(logged);
        let (mut upstream, head) = if method == "CONNECT" {
            let upstream = TcpStream::connect(target).unwrap();
            let mut c = &client;
            c.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").unwrap();
            (upstream, Vec::new())
        } else {
            let rest = target.strip_prefix("http://").unwrap();
            let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
            let mut head = format!("{method} /{path} HTTP/1.1\r\n");
            for (k, v) in &headers {
                if k != "proxy-authorization" && k != "proxy-connection" {
                    head.push_str(&format!("{k}: {v}\r\n"));
                }
            }
            head.push_str("\r\n");
            (TcpStream::connect(authority).unwrap(), head.into_bytes())
        };
        upstream.write_all(&head).unwrap();
        upstream.write_all(r.buffer()).unwrap();
        let mut down = upstream.try_clone().unwrap();
        let mut back = client.try_clone().unwrap();
        let mut from_client = r.into_inner();
        let t = std::thread::spawn(move || {
            let _ = std::io::copy(&mut from_client, &mut upstream);
            let _ = upstream.shutdown(Shutdown::Write);
        });
        let _ = std::io::copy(&mut down, &mut back);
        let _ = back.shutdown(Shutdown::Both);
        let _ = t.join();
    })
}

struct Servers {
    plain: u16,
    secure: u16,
    proxy: u16,
    log: Arc<Mutex<Vec<String>>>,
}

impl Servers {
    fn start() -> Self {
        let plain = listen(serve_http);
        let config = tls_config();
        let secure = listen(move |tcp| {
            let conn = rustls::ServerConnection::new(config.clone()).unwrap();
            serve_http(rustls::StreamOwned::new(conn, tcp));
        });
        let log = Arc::new(Mutex::new(Vec::new()));
        let proxy = proxy(log.clone());
        Self { plain, secure, proxy, log }
    }

    fn ports(&self) -> String {
        format!(r#"{{"plain":{},"secure":{},"proxy":{}}}"#, self.plain, self.secure, self.proxy)
    }

    fn take_log(&self) -> Vec<String> {
        std::mem::take(&mut *self.log.lock().unwrap())
    }
}

// ---------------------------------------------------------------------------
// Running scripts
// ---------------------------------------------------------------------------

/// `node` when it is on PATH and is Node 22 or newer.
fn node() -> Option<&'static str> {
    let out = Command::new("node").arg("--version").output().ok()?;
    let version = String::from_utf8_lossy(&out.stdout);
    let major: u32 = version.trim().trim_start_matches('v').split('.').next()?.parse().ok()?;
    (out.status.success() && major >= 22).then_some("node")
}

/// Run `script` with `bin`; stdout + stderr with the process id and the executable name in
/// warning hints masked (Node prints both).
fn run(bin: &str, dir: &Path, script: &str, env: &[(&str, &str)]) -> String {
    let path = dir.join("script.js");
    std::fs::write(&path, script).unwrap();
    let mut cmd = Command::new(bin);
    cmd.arg(&path).current_dir(dir).env_remove("HTTP_PROXY").env_remove("HTTPS_PROXY").env_remove("http_proxy").env_remove("https_proxy");
    cmd.env_remove("NO_PROXY").env_remove("no_proxy").env_remove("NODE_EXTRA_CA_CERTS");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let mut masked = String::new();
    for line in text.lines() {
        let line = line.replace("pi-jsrt-node --trace-warnings", "node --trace-warnings");
        let line = match line.strip_prefix("(node:").and_then(|rest| rest.split_once(')')) {
            Some((_, rest)) => format!("(node:PID){rest}"),
            None => line,
        };
        masked.push_str(&line);
        masked.push('\n');
    }
    masked
}

/// Run `script` under pi-jsrt (and Node, when present) against fresh servers; both outputs and
/// proxy logs must equal `expected` and `expected_log`.
fn differential(script: &str, extra_env: &[(&str, &str)], expected: &str, expected_log: &[&str], node_too: bool) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ca.pem"), CA).unwrap();
    let ca = dir.path().join("ca.pem").display().to_string();
    let mut runtimes = vec![NODE_BIN];
    if node_too && let Some(node) = node() {
        runtimes.push(node);
    }
    for bin in runtimes {
        let servers = Servers::start();
        let ports = servers.ports();
        // A port nothing listens on.
        let closed = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port().to_string();
        let mut env = vec![("PORTS", ports.as_str()), ("CA", ca.as_str()), ("CLOSED_PORT", closed.as_str())];
        if bin != NODE_BIN {
            env.push(("IS_NODE", "1"));
        }
        env.extend_from_slice(extra_env);
        let out = run(bin, dir.path(), script, &env);
        assert_eq!(out, expected, "{bin}");
        assert_eq!(servers.take_log(), expected_log, "{bin} proxy log");
    }
}

fn fixtures() -> PathBuf {
    let root = std::env::var_os("PI_RS_PKG_FIXTURES").expect("PI_RS_PKG_FIXTURES must point at the installed package fixtures");
    let modules = PathBuf::from(root).join("pkgp14/node_modules");
    assert!(modules.join("undici/package.json").exists() && modules.join("undici6/package.json").exists(), "install tests/fixtures/pkgp14 first (see the module docs)");
    modules
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

const PRELUDE: &str = r#"const { plain, secure, proxy } = JSON.parse(process.env.PORTS);
const fs = require("fs");
const out = (...a) => console.log(JSON.stringify(a));
const err = (e) => ({ name: e?.name, message: e?.message, cause: e?.cause && { name: e.cause.name, code: e.cause.code, message: e.cause.message } });
"#;

const STATUS_AND_UPLOAD: &str = r#"(async () => {
  for (const p of ["/404", "/custom", "/empty", "/teapot", "/odd", "/"]) {
    const r = await fetch(`http://127.0.0.1:${plain}${p}`);
    out("status", p, r.status, r.statusText, r.ok);
    await r.text();
  }
  const mk = () => new ReadableStream({ start(c) { c.enqueue(new TextEncoder().encode("ab")); c.enqueue(new Uint8Array([99, 100])); c.close(); } });
  try { await fetch(`http://127.0.0.1:${plain}/up`, { method: "POST", body: mk() }); out("noduplex", "ok"); } catch (e) { out("noduplex", err(e)); }
  try { new Request(`http://127.0.0.1:${plain}/up`, { method: "POST", body: mk() }); out("reqnoduplex", "ok"); } catch (e) { out("reqnoduplex", err(e)); }
  { const r = await fetch(`http://127.0.0.1:${plain}/up`, { method: "POST", body: mk(), duplex: "half" }); out("stream", r.status, await r.json()); }
  { const r = await fetch(`http://127.0.0.1:${plain}/up`, { method: "PUT", body: (async function* () { yield new TextEncoder().encode("x"); yield new TextEncoder().encode("yz"); })(), duplex: "half" }); out("asyncgen", r.status, await r.json()); }
  { const rq = new Request(`http://127.0.0.1:${plain}/up`, { method: "POST", body: mk(), duplex: "half" }); out("reqduplex", rq.duplex, rq.body instanceof ReadableStream); const r = await fetch(rq); out("reqstream", r.status, await r.json(), rq.bodyUsed); }
  { const big = new Uint8Array(256 * 1024).fill(97); let n = 0; const r = await fetch(`http://127.0.0.1:${plain}/up`, { method: "POST", duplex: "half", body: new ReadableStream({ pull(c) { if (n++ < 8) c.enqueue(big); else c.close(); } }) }); const j = await r.json(); out("bigstream", r.status, j.te, j.body.length); }
  try { const r = await fetch(`http://127.0.0.1:${plain}/up`, { method: "POST", body: new ReadableStream({ start(c) { c.enqueue("str"); c.close(); } }), duplex: "half" }); out("strchunk", r.status, await r.text()); } catch (e) { out("strchunk", err(e)); }
  try { const r = await fetch(`http://127.0.0.1:${plain}/up`, { method: "POST", body: new ReadableStream({ start(c) { c.enqueue(new Uint8Array([1])); c.error(new Error("boom")); } }), duplex: "half" }); out("errstream", r.status); } catch (e) { out("errstream", err(e)); }
  try { await fetch(`https://localhost:${secure}/`); out("noca", "ok"); } catch (e) { out("noca", err(e)); }
  try { await fetch(`http://127.0.0.1:1/`); out("badport", "ok"); } catch (e) { out("badport", err(e)); }
  try { await fetch(`http://127.0.0.1:${process.env.CLOSED_PORT}/`); out("refused", "ok"); } catch (e) { out("refused", err(e).cause.code, err(e).cause.message === `connect ECONNREFUSED 127.0.0.1:${process.env.CLOSED_PORT}`); }
})();
"#;

const STATUS_AND_UPLOAD_NODE: &str = r#"["status","/404",404,"Not Found",false]
["status","/custom",404,"Nothing To See",false]
["status","/empty",404,"",false]
["status","/teapot",418,"I'm a teapot",false]
["status","/odd",299,"Whatever You Say",true]
["status","/",200,"OK",true]
["noduplex",{"name":"TypeError","message":"RequestInit: duplex option is required when sending a body."}]
["reqnoduplex",{"name":"TypeError","message":"RequestInit: duplex option is required when sending a body."}]
["stream",200,{"method":"POST","path":"/up","te":"chunked","cl":null,"body":"abcd"}]
["asyncgen",200,{"method":"PUT","path":"/up","te":"chunked","cl":null,"body":"xyz"}]
["reqduplex","half",true]
["reqstream",200,{"method":"POST","path":"/up","te":"chunked","cl":null,"body":"abcd"},true]
["bigstream",200,"chunked",2097152]
["strchunk",200,"{\"method\":\"POST\",\"path\":\"/up\",\"te\":\"chunked\",\"cl\":null,\"body\":\"str\"}"]
["errstream",{"name":"TypeError","message":"fetch failed","cause":{"name":"Error","message":"boom"}}]
["noca",{"name":"TypeError","message":"fetch failed","cause":{"name":"Error","code":"UNABLE_TO_VERIFY_LEAF_SIGNATURE","message":"unable to verify the first certificate"}}]
["badport",{"name":"TypeError","message":"fetch failed","cause":{"name":"Error","message":"bad port"}}]
["refused","ECONNREFUSED",true]
"#;

#[test]
fn status_text_and_streamed_upload_match_node() {
    differential(&format!("{PRELUDE}{STATUS_AND_UPLOAD}"), &[], STATUS_AND_UPLOAD_NODE, &[], true);
}

const EXTRA_CA: &str = r#"fetch(`https://localhost:${secure}/extra`).then(async (r) => out("extra", r.status, r.statusText, (await r.json()).path), (e) => out("extra", err(e)));
"#;

#[test]
fn node_extra_ca_certs_match_node() {
    let script = format!("{PRELUDE}{EXTRA_CA}");
    let dir = tempfile::tempdir().unwrap();
    let ca = dir.path().join("extra.pem");
    std::fs::write(&ca, CA).unwrap();
    let ca = ca.display().to_string();
    differential(&script, &[("NODE_EXTRA_CA_CERTS", &ca)], "[\"extra\",200,\"OK\",\"/extra\"]\n", &[], true);
    let missing = dir.path().join("missing.pem").display().to_string();
    let expected = format!(
        "Warning: Ignoring extra certs from `{missing}`, load failed: error:80000002:system library::No such file or directory\n[\"extra\",{{\"name\":\"TypeError\",\"message\":\"fetch failed\",\"cause\":{{\"name\":\"Error\",\"code\":\"UNABLE_TO_VERIFY_LEAF_SIGNATURE\",\"message\":\"unable to verify the first certificate\"}}}}]\n"
    );
    // Node writes the warning to stderr first; this harness prints stdout first.
    let expected = {
        let (warning, rest) = expected.split_once('\n').unwrap();
        format!("{rest}{warning}\n")
    };
    differential(&script, &[("NODE_EXTRA_CA_CERTS", &missing)], &expected, &[], true);
}

/// Dispatchers built the way undici builds them (its symbol-named fields), without undici
/// itself: proves the native settings path offline. pi-jsrt only: Node would call `dispatch`.
const SHAPED_DISPATCHERS: &str = r#"class Agent {
  constructor(opts = {}) { const { connect, ...rest } = opts; this[Symbol("options")] = { ...rest, connect }; }
  dispatch() { throw new Error("native fetch must not call dispatch"); }
}
class ProxyAgent {
  constructor(opts) {
    const url = new URL(opts.uri);
    this[Symbol("proxy agent options")] = { uri: url.href, protocol: url.protocol };
    this[Symbol("request tls settings")] = opts.requestTls;
    this[Symbol("proxy headers")] = { ...(opts.headers ?? {}), ...(opts.token ? { "proxy-authorization": opts.token } : {}) };
    this[Symbol("tunnel proxy")] = opts.proxyTunnel;
    this[Symbol("proxy agent")] = new Agent(opts);
  }
  dispatch() { throw new Error("native fetch must not call dispatch"); }
}
const ca = fs.readFileSync(process.env.CA, "utf8");
(async () => {
  { const r = await fetch(`https://localhost:${secure}/ca`, { dispatcher: new Agent({ connect: { ca } }) }); out("ca", r.status, (await r.json()).path); }
  { const r = await fetch(`https://127.0.0.1:${secure}/insecure`, { dispatcher: new Agent({ connect: { rejectUnauthorized: false } }) }); out("insecure", r.status, (await r.json()).path); }
  try { await fetch(`http://127.0.0.1:${plain}/slow`, { dispatcher: new Agent({ headersTimeout: 100 }) }); out("hto", "ok"); } catch (e) { out("hto", err(e)); }
  const pa = (o) => new ProxyAgent({ uri: `http://127.0.0.1:${proxy}`, ...o });
  { const r = await fetch(`https://localhost:${secure}/p-https`, { dispatcher: pa({ requestTls: { ca }, token: "Bearer t" }) }); out("p-https", r.status, (await r.json()).path); }
  { const r = await fetch(`http://localhost:${plain}/p-http`, { dispatcher: pa({}) }); out("p-http", r.status, (await r.json()).path); }
  { const r = await fetch(`http://localhost:${plain}/p-tunnel?q=1`, { dispatcher: pa({ proxyTunnel: true, token: "Basic eDp5" }) }); out("p-tunnel", r.status, (await r.json()).path); }
  globalThis[Symbol.for("undici.globalDispatcher.1")] = new Agent({ connect: { ca } });
  { const r = await fetch(`https://localhost:${secure}/global`); out("global", r.status, (await r.json()).path); }
})();
"#;

#[test]
fn undici_shaped_dispatchers_apply_natively() {
    let expected = r#"["ca",200,"/ca"]
["insecure",200,"/insecure"]
["hto",{"name":"TypeError","message":"fetch failed","cause":{"name":"HeadersTimeoutError","code":"UND_ERR_HEADERS_TIMEOUT","message":"Headers Timeout Error"}}]
["p-https",200,"/p-https"]
["p-http",200,"/p-http"]
["p-tunnel",200,"/p-tunnel?q=1"]
["global",200,"/global"]
"#;
    let log = ["CONNECT localhost:PORT Bearer t", "GET http://localhost:PORT/p-http -", "CONNECT localhost:PORT Basic eDp5"];
    differential(&format!("{PRELUDE}{SHAPED_DISPATCHERS}"), &[], expected, &log, false);
}

/// The undici scenarios; `UNDICI` names the fixture package (`undici6` or `undici`).
const UNDICI_DISPATCHERS: &str = r#"const undici = require("module").createRequire(process.env.FIX + "/")(process.env.UNDICI);
const ca = fs.readFileSync(process.env.CA, "utf8");
(async () => {
  { const r = await fetch(`https://localhost:${secure}/ca`, { dispatcher: new undici.Agent({ connect: { ca } }) }); out("ca", r.status, r.statusText, (await r.json()).path); }
  { const r = await fetch(`https://127.0.0.1:${secure}/insecure`, { dispatcher: new undici.Agent({ connect: { rejectUnauthorized: false } }) }); out("insecure", r.status, (await r.json()).path); }
  process.env.HTTPS_PROXY = `http://127.0.0.1:${proxy}`;
  process.env.HTTP_PROXY = `http://127.0.0.1:${proxy}`;
  { const r = await fetch(`https://localhost:${secure}/via-proxy`, { dispatcher: new undici.EnvHttpProxyAgent({ requestTls: { ca } }) }); out("proxy-https", r.status, (await r.json()).path); }
  { const r = await fetch(`http://localhost:${plain}/via-proxy-tunnel`, { dispatcher: new undici.EnvHttpProxyAgent({ proxyTunnel: true }) }); out("proxy-tunnel", r.status, (await r.json()).path); }
  { const r = await fetch(`http://localhost:${plain}/noproxy`, { dispatcher: new undici.EnvHttpProxyAgent({ noProxy: "localhost" }) }); out("noproxy", r.status, (await r.json()).path); }
  process.env.NO_PROXY = "127.0.0.1";
  { const r = await fetch(`http://127.0.0.1:${plain}/env-noproxy`, { dispatcher: new undici.EnvHttpProxyAgent() }); out("env-noproxy", r.status, (await r.json()).path); }
  delete process.env.NO_PROXY;
  { const r = await fetch(`http://localhost:${plain}/pa`, { dispatcher: new undici.ProxyAgent({ uri: `http://u:p@127.0.0.1:${proxy}`, proxyTunnel: true }) }); out("proxyagent", r.status, (await r.json()).path); }
  { const r = await fetch(`https://localhost:${secure}/pa-token`, { dispatcher: new undici.ProxyAgent({ uri: `http://127.0.0.1:${proxy}`, token: "Bearer tok", requestTls: { ca } }) }); out("proxyagent-token", r.status, (await r.json()).path); }
  try { await fetch(`http://127.0.0.1:${plain}/slow`, { dispatcher: new undici.Agent({ headersTimeout: 100 }) }); out("hto", "ok"); } catch (e) { out("hto", err(e)); }
  try { const r = await fetch(`http://127.0.0.1:${plain}/slowbody`, { dispatcher: new undici.Agent({ bodyTimeout: 100 }) }); out("bto-head", r.status); await r.text(); out("bto", "ok"); } catch (e) { out("bto", err(e)); }
  undici.setGlobalDispatcher(new undici.Agent({ connect: { ca } }));
  try { const r = await fetch(`https://localhost:${secure}/global`); out("global", r.status, (await r.json()).path); } catch (e) { out("global", err(e)); }
})().catch((e) => out("FATAL", err(e)));
"#;

const UNDICI_DISPATCHERS_NODE: &str = r#"["ca",200,"OK","/ca"]
["insecure",200,"/insecure"]
["proxy-https",200,"/via-proxy"]
["proxy-tunnel",200,"/via-proxy-tunnel"]
["noproxy",200,"/noproxy"]
["env-noproxy",200,"/env-noproxy"]
["proxyagent",200,"/pa"]
["proxyagent-token",200,"/pa-token"]
["hto",{"name":"TypeError","message":"fetch failed","cause":{"name":"HeadersTimeoutError","code":"UND_ERR_HEADERS_TIMEOUT","message":"Headers Timeout Error"}}]
["bto-head",200]
["bto",{"name":"TypeError","message":"terminated","cause":{"name":"BodyTimeoutError","code":"UND_ERR_BODY_TIMEOUT","message":"Body Timeout Error"}}]
["global",200,"/global"]
"#;

const UNDICI_PROXY_LOG: &[&str] = &[
    "CONNECT localhost:PORT -",
    "CONNECT localhost:PORT -",
    "CONNECT localhost:PORT Basic dTpw",
    "CONNECT localhost:PORT Bearer tok",
];

/// undici 6 (what Node 22's own fetch accepts as a dispatcher, and what pi-mcp-adapter pins):
/// global `fetch` with its dispatchers equals Node 22, down to what the proxy is asked.
#[test]
#[ignore = "needs the pinned npm fixtures under $PI_RS_PKG_FIXTURES/pkgp14"]
fn undici6_dispatchers_match_node() {
    let modules = fixtures();
    let fix = modules.display().to_string();
    let warning = "(node:PID) [UNDICI-EHPA] Warning: EnvHttpProxyAgent is experimental, expect them to change at any time.\n(Use `node --trace-warnings ...` to show where the warning was created)\n";
    let expected = format!("{UNDICI_DISPATCHERS_NODE}{warning}");
    differential(&format!("{PRELUDE}{UNDICI_DISPATCHERS}"), &[("FIX", &fix), ("UNDICI", "undici6")], &expected, UNDICI_PROXY_LOG, true);
}

/// undici 8 objects work the same way on pi-jsrt's global `fetch` (Node 22's bundled undici 6
/// rejects them: "invalid onRequestStart method"; newer Node versions accept them).
#[test]
#[ignore = "needs the pinned npm fixtures under $PI_RS_PKG_FIXTURES/pkgp14"]
fn undici8_dispatchers_apply_natively() {
    let modules = fixtures();
    let fix = modules.display().to_string();
    differential(&format!("{PRELUDE}{UNDICI_DISPATCHERS}"), &[("FIX", &fix), ("UNDICI", "undici")], UNDICI_DISPATCHERS_NODE, UNDICI_PROXY_LOG, false);
}

/// undici 8 sends plain-HTTP targets to the proxy in absolute form unless `proxyTunnel: true`.
/// Node runs undici 8's own `fetch`; pi-jsrt its global `fetch` with the same dispatchers.
#[test]
#[ignore = "needs the pinned npm fixtures under $PI_RS_PKG_FIXTURES/pkgp14"]
fn undici8_proxy_wire_forms_match_node() {
    let modules = fixtures();
    let fix = modules.display().to_string();
    let script = r#"const undici = require("module").createRequire(process.env.FIX + "/")("undici");
const f = process.env.IS_NODE ? undici.fetch : globalThis.fetch;
process.env.HTTPS_PROXY = `http://127.0.0.1:${proxy}`;
process.env.HTTP_PROXY = `http://127.0.0.1:${proxy}`;
(async () => {
  for (const [name, path, opts] of [["http", "/v8-http", {}], ["tunnel", "/v8-tunnel", { proxyTunnel: true }]]) {
    const r = await f(`http://localhost:${plain}${path}`, { dispatcher: new undici.EnvHttpProxyAgent(opts) });
    out(name, r.status, (await r.json()).path);
  }
  const r = await f(`http://localhost:${plain}/v8-pa`, { dispatcher: new undici.ProxyAgent(`http://127.0.0.1:${proxy}`) });
  out("pa", r.status, (await r.json()).path);
})().catch((e) => out("FATAL", err(e)));
"#;
    let script = format!("{PRELUDE}{script}");
    let expected = "[\"http\",200,\"/v8-http\"]\n[\"tunnel\",200,\"/v8-tunnel\"]\n[\"pa\",200,\"/v8-pa\"]\n";
    let log = ["GET http://localhost:PORT/v8-http -", "CONNECT localhost:PORT -", "GET http://localhost:PORT/v8-pa -"];
    differential(&script, &[("FIX", &fix)], expected, &log, true);
}
