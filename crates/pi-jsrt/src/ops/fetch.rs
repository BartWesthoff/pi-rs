//! HTTP ops backing `fetch` (via reqwest), with streaming request and response bodies and abort.
//!
//! Each request carries the connection settings of its undici dispatcher, resolved in JS
//! (`js/web/fetch.js`): proxy, TLS (`ca`, client certificate, `rejectUnauthorized`), connect
//! timeout and HTTP/2 opt-in pick a cached client; the header and body idle timeouts apply per
//! request, as undici's `headersTimeout`/`bodyTimeout`. Failures carry Node's wording and codes
//! so JS can raise `TypeError: fetch failed` (or `terminated`) with an undici-shaped `cause`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use crate::js::JsArray;
use crate::js::{Context, JsArgs, JsResult, JsValue};
use parking_lot::Mutex;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::tls::{AuthSlot, FetchTls, fetch_client_config};
use super::{array, bytes_arg, bytes_to_js, object};
use crate::engine::{self, OpFn, arg_f64, arg_string, js_str};
use crate::error::{Error, JsException};

type Body = Arc<tokio::sync::Mutex<Option<reqwest::Response>>>;
type Upload = mpsc::Sender<Result<Vec<u8>, std::io::Error>>;

static BODIES: LazyLock<Mutex<HashMap<u64, (Body, Option<Duration>)>>> = LazyLock::new(Default::default);
static TOKENS: LazyLock<Mutex<HashMap<u64, CancellationToken>>> = LazyLock::new(Default::default);
static UPLOADS: LazyLock<Mutex<HashMap<u64, Upload>>> = LazyLock::new(Default::default);
/// Clients by `(redirect mode, connection settings)`; undici likewise pools per dispatcher.
static CLIENTS: LazyLock<Mutex<HashMap<String, (reqwest::Client, AuthSlot)>>> = LazyLock::new(Default::default);
/// Loopback forwarders that tunnel plain-HTTP requests through a proxy with CONNECT.
static TUNNELS: LazyLock<Mutex<HashMap<String, u16>>> = LazyLock::new(Default::default);

/// Chunks of a streamed request body buffered ahead of the socket (backpressure on the JS pump).
const UPLOAD_QUEUE: usize = 4;

/// Connection settings of one undici dispatcher (see `dispatcherSettings` in `fetch.js`).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct ClientSpec {
    /// Use the environment proxy (reqwest's `HTTP(S)_PROXY`/`NO_PROXY`): the default without a
    /// dispatcher. Otherwise `proxy` (or a direct connection) is the dispatcher's choice.
    env_proxy: bool,
    proxy: Option<ProxySpec>,
    ca: Vec<String>,
    cert: Vec<String>,
    key: Vec<String>,
    passphrase: Option<String>,
    reject_unauthorized: Option<bool>,
    /// Milliseconds; 0 disables it.
    connect_timeout: u64,
    allow_h2: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProxySpec {
    /// The proxy URL without credentials (they are in `headers` as `proxy-authorization`).
    uri: String,
    #[serde(default)]
    headers: Vec<(String, String)>,
    /// Tunnel a plain-HTTP target with CONNECT instead of sending it in absolute form.
    #[serde(default)]
    tunnel: bool,
}

/// A `fetch` failure: the `cause` JS attaches to `TypeError: fetch failed`/`terminated`.
fn cause(name: &str, message: String, code: Option<&str>) -> Error {
    Error::Js(JsException { name: name.into(), message, stack: None, code: code.map(Into::into) })
}

fn type_error(message: String) -> Error {
    Error::Js(JsException { name: "TypeError".into(), message, stack: None, code: None })
}

fn abort_error() -> Error {
    Error::Js(JsException {
        name: "AbortError".into(),
        message: "This operation was aborted".into(),
        stack: None,
        code: Some("ABORT_ERR".into()),
    })
}

fn token(id: u64) -> CancellationToken {
    TOKENS.lock().entry(id).or_default().clone()
}

fn timeout(ms: f64) -> Option<Duration> {
    (ms.is_finite() && ms > 0.0).then(|| Duration::from_millis(ms as u64))
}

/// `host:port` of the URL a connection goes to.
fn address(url: &reqwest::Url) -> String {
    let host = url.host_str().unwrap_or("localhost").trim_start_matches('[').trim_end_matches(']');
    match url.port_or_known_default() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    }
}

/// Describe a reqwest failure the way undici's `cause` does (`connect ECONNREFUSED ...`,
/// `getaddrinfo ENOTFOUND ...`, OpenSSL certificate codes, ...).
fn network_cause(error: &reqwest::Error, spec: &ClientSpec, auth: &AuthSlot) -> Error {
    let url = error.url();
    let host = url.and_then(|u| u.host_str()).unwrap_or("localhost").trim_start_matches('[').trim_end_matches(']').to_string();
    // A configured proxy is where the connection went.
    let proxy = spec.proxy.as_ref().and_then(|p| reqwest::Url::parse(&p.uri).ok());
    let connect_url = proxy.as_ref().or(url);
    let connect_host = connect_url.and_then(|u| u.host_str()).map(|h| h.trim_start_matches('[').trim_end_matches(']').to_string()).unwrap_or(host);
    let addr = connect_url.map(address).unwrap_or_else(|| connect_host.clone());
    let mut texts = Vec::new();
    let mut io_kind = None;
    let mut tls = false;
    let mut source: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(error);
    while let Some(current) = source {
        if let Some(io) = current.downcast_ref::<std::io::Error>() {
            io_kind.get_or_insert(io.kind());
        }
        tls |= current.downcast_ref::<rustls::Error>().is_some();
        texts.push(current.to_string());
        source = current.source();
    }
    let all = texts.join(": ").to_ascii_lowercase();
    if (tls || all.contains("certificate")) && let Some((code, message)) = auth.lock().expect("lock poisoned").take() {
        return cause("Error", message, Some(&code));
    }
    if all.contains("dns error") || all.contains("failed to lookup address") {
        return cause("Error", format!("getaddrinfo ENOTFOUND {connect_host}"), Some("ENOTFOUND"));
    }
    if spec.connect_timeout > 0 && error.is_connect() && error.is_timeout() {
        return cause(
            "ConnectTimeoutError",
            format!("Connect Timeout Error (attempted address: {addr}, timeout: {}ms)", spec.connect_timeout),
            Some("UND_ERR_CONNECT_TIMEOUT"),
        );
    }
    use std::io::ErrorKind;
    let errno = |code: &str, message: String| cause("Error", message, Some(code));
    match io_kind {
        Some(ErrorKind::ConnectionRefused) => return errno("ECONNREFUSED", format!("connect ECONNREFUSED {addr}")),
        Some(ErrorKind::TimedOut) => return errno("ETIMEDOUT", format!("connect ETIMEDOUT {addr}")),
        Some(ErrorKind::ConnectionReset) => return errno("ECONNRESET", "read ECONNRESET".into()),
        Some(ErrorKind::HostUnreachable) => return errno("EHOSTUNREACH", format!("connect EHOSTUNREACH {addr}")),
        Some(ErrorKind::NetworkUnreachable) => return errno("ENETUNREACH", format!("connect ENETUNREACH {addr}")),
        Some(ErrorKind::BrokenPipe) => return errno("EPIPE", "write EPIPE".into()),
        _ => {}
    }
    if all.contains("connection closed") || all.contains("incomplete message") || all.contains("unexpected eof") {
        return cause("SocketError", "other side closed".into(), Some("UND_ERR_SOCKET"));
    }
    cause("Error", texts.pop().unwrap_or_else(|| error.to_string()), None)
}

/// A reqwest client for `spec`, built once per distinct settings.
fn client(key: &str, spec: &ClientSpec, follow: bool) -> Result<(reqwest::Client, AuthSlot), Error> {
    if let Some(c) = CLIENTS.lock().get(key) {
        return Ok(c.clone());
    }
    let tls = FetchTls {
        ca: spec.ca.clone(),
        cert: spec.cert.clone(),
        key: spec.key.clone(),
        passphrase: spec.passphrase.clone().map(String::into_bytes),
        reject_unauthorized: spec.reject_unauthorized.unwrap_or(true),
        alpn: if spec.allow_h2 { vec![b"h2".to_vec(), b"http/1.1".to_vec()] } else { vec![b"http/1.1".to_vec()] },
    };
    let (tls, auth) = fetch_client_config(&tls).map_err(|e| cause("Error", e, Some("ERR_TLS_INVALID_CONTEXT")))?;
    let policy = if follow { reqwest::redirect::Policy::limited(20) } else { reqwest::redirect::Policy::none() };
    let mut builder = reqwest::Client::builder().redirect(policy).use_preconfigured_tls(tls);
    // undici speaks HTTP/1.1 unless the dispatcher sets `allowH2`.
    if !spec.allow_h2 {
        builder = builder.http1_only();
    }
    if spec.connect_timeout > 0 {
        builder = builder.connect_timeout(Duration::from_millis(spec.connect_timeout));
    }
    let bad_proxy = |e: reqwest::Error| type_error(format!("Invalid URL: {e}"));
    match &spec.proxy {
        None if spec.env_proxy => {}
        None => builder = builder.no_proxy(),
        Some(p) => {
            let mut headers = reqwest::header::HeaderMap::new();
            let mut auth_header = None;
            for (k, v) in &p.headers {
                let (Ok(name), Ok(value)) = (reqwest::header::HeaderName::from_bytes(k.as_bytes()), reqwest::header::HeaderValue::from_str(v)) else {
                    continue;
                };
                if name == reqwest::header::PROXY_AUTHORIZATION {
                    auth_header = Some(value);
                } else {
                    headers.append(name, value);
                }
            }
            let configure = |mut proxy: reqwest::Proxy| {
                if let Some(a) = &auth_header {
                    proxy = proxy.custom_http_auth(a.clone());
                }
                if !headers.is_empty() {
                    proxy = proxy.headers(headers.clone());
                }
                proxy
            };
            let https = configure(reqwest::Proxy::https(p.uri.as_str()).map_err(bad_proxy)?);
            let http = if p.tunnel && p.uri.starts_with("http:") {
                // reqwest tunnels only HTTPS targets; a loopback forwarder turns its
                // absolute-form request into a CONNECT tunnel through the real proxy.
                let port = tunnel_forwarder(p).map_err(|e| cause("Error", e.to_string(), None))?;
                reqwest::Proxy::http(format!("http://127.0.0.1:{port}")).map_err(bad_proxy)?
            } else {
                configure(reqwest::Proxy::http(p.uri.as_str()).map_err(bad_proxy)?)
            };
            builder = builder.proxy(https).proxy(http);
            if p.tunnel {
                // Every forwarded connection carries exactly one request.
                builder = builder.pool_max_idle_per_host(0);
            }
        }
    }
    let client = builder.build().map_err(|e| cause("Error", e.to_string(), None))?;
    CLIENTS.lock().insert(key.to_string(), (client.clone(), auth.clone()));
    Ok((client, auth))
}

/// Start (once per proxy) a loopback HTTP proxy that forwards each absolute-form request through
/// `proxy` inside a CONNECT tunnel, as undici's `ProxyAgent` does for plain-HTTP origins. It runs
/// on its own threads so it outlives any one runtime.
fn tunnel_forwarder(proxy: &ProxySpec) -> std::io::Result<u16> {
    let key = format!("{}|{:?}", proxy.uri, proxy.headers);
    if let Some(port) = TUNNELS.lock().get(&key) {
        return Ok(*port);
    }
    let upstream = reqwest::Url::parse(&proxy.uri).map_err(std::io::Error::other)?;
    let upstream = address(&upstream);
    let headers: String = proxy.headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    std::thread::Builder::new().name("fetch-tunnel".into()).spawn(move || {
        for conn in listener.incoming().flatten() {
            let upstream = upstream.clone();
            let headers = headers.clone();
            std::thread::spawn(move || {
                let _ = forward_through_tunnel(conn, &upstream, &headers);
            });
        }
    })?;
    TUNNELS.lock().insert(key, port);
    Ok(port)
}

fn forward_through_tunnel(client: TcpStream, upstream: &str, headers: &str) -> std::io::Result<()> {
    let mut reader = BufReader::new(client.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    // `GET http://host:port/path HTTP/1.1`
    let mut parts = line.trim_end().splitn(3, ' ');
    let (Some(method), Some(target), Some(version)) = (parts.next(), parts.next(), parts.next()) else {
        return Ok(());
    };
    let url = reqwest::Url::parse(target).map_err(std::io::Error::other)?;
    let authority = address(&url);
    let path = match url.query() {
        Some(q) => format!("{}?{q}", url.path()),
        None => url.path().to_string(),
    };
    let mut proxy = TcpStream::connect(upstream)?;
    write!(proxy, "CONNECT {authority} HTTP/1.1\r\nhost: {authority}\r\n{headers}\r\n")?;
    let mut proxy_reader = BufReader::new(proxy.try_clone()?);
    let mut status = String::new();
    proxy_reader.read_line(&mut status)?;
    loop {
        let mut h = String::new();
        if proxy_reader.read_line(&mut h)? == 0 || h == "\r\n" || h == "\n" {
            break;
        }
    }
    let code = status.split(' ').nth(1).unwrap_or("");
    if code != "200" {
        let mut client = client;
        write!(client, "HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")?;
        return Ok(());
    }
    write!(proxy, "{method} {path} {version}\r\n")?;
    let buffered = reader.buffer().to_vec();
    proxy.write_all(&buffered)?;
    let mut to_proxy = proxy.try_clone()?;
    let mut from_client = reader.into_inner();
    let up = std::thread::spawn(move || {
        let _ = std::io::copy(&mut from_client, &mut to_proxy);
        let _ = to_proxy.shutdown(std::net::Shutdown::Write);
    });
    let mut client = client;
    let pending = proxy_reader.buffer().to_vec();
    client.write_all(&pending)?;
    let mut from_proxy = proxy_reader.into_inner();
    let _ = std::io::copy(&mut from_proxy, &mut client);
    let _ = client.shutdown(std::net::Shutdown::Both);
    let _ = up.join();
    Ok(())
}

struct Head {
    status: u16,
    status_text: String,
    url: String,
    headers: Vec<(String, String)>,
}

/// `ops.fetchStart(id, method, url, headerPairs, body, redirect, settingsJson, headersTimeout,
/// bodyTimeout)`; `body` is `null`, bytes, or `true` for a body streamed with `fetchWrite`.
fn op_fetch_start(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = arg_f64(args, 0, ctx)? as u64;
    let method = arg_string(args, 1, ctx)?;
    let url = arg_string(args, 2, ctx)?;
    let mut headers = Vec::new();
    if let Some(obj) = args.get_or_undefined(3).as_object() {
        let arr = JsArray::from_object(obj)?;
        let len = arr.length(ctx)?;
        let mut i = 0;
        while i + 1 < len {
            let k = arr.get(i, ctx)?.to_string(ctx)?.to_std_string_lossy();
            let v = arr.get(i + 1, ctx)?.to_string(ctx)?.to_std_string_lossy();
            headers.push((k, v));
            i += 2;
        }
    }
    let body_v = args.get_or_undefined(4);
    let body = if body_v.is_null_or_undefined() {
        None
    } else if body_v.as_boolean() == Some(true) {
        let (tx, mut rx) = mpsc::channel(UPLOAD_QUEUE);
        UPLOADS.lock().insert(id, tx);
        let stream = futures::stream::poll_fn(move |cx| rx.poll_recv(cx));
        Some(reqwest::Body::wrap_stream(stream))
    } else {
        Some(reqwest::Body::from(bytes_arg(body_v, ctx)?))
    };
    let redirect = arg_string(args, 5, ctx).unwrap_or_else(|_| "follow".into());
    let settings = match args.get_or_undefined(6) {
        v if v.is_null_or_undefined() => String::new(),
        v => v.to_string(ctx)?.to_std_string_lossy(),
    };
    let headers_timeout = timeout(args.get_or_undefined(7).to_number(ctx)?);
    let body_timeout = timeout(args.get_or_undefined(8).to_number(ctx)?);
    let cancel = token(id);
    let fut = async move {
        let spec: ClientSpec = if settings.is_empty() {
            ClientSpec { env_proxy: true, ..ClientSpec::default() }
        } else {
            serde_json::from_str(&settings).map_err(|e| type_error(format!("invalid dispatcher settings: {e}")))?
        };
        let (client, auth) = client(&format!("{redirect}|{settings}"), &spec, redirect == "follow")?;
        let method = reqwest::Method::from_bytes(method.to_ascii_uppercase().as_bytes())
            .map_err(|_| type_error(format!("'{method}' is not a valid HTTP method.")))?;
        let mut req = client.request(method, &url);
        for (k, v) in headers {
            req = req.header(k, v);
        }
        if let Some(b) = body {
            req = req.body(b);
        }
        let send = async {
            match headers_timeout {
                Some(t) => tokio::time::timeout(t, req.send())
                    .await
                    .map_err(|_| cause("HeadersTimeoutError", "Headers Timeout Error".into(), Some("UND_ERR_HEADERS_TIMEOUT")))?,
                None => req.send().await,
            }
            .map_err(|e| network_cause(&e, &spec, &auth))
        };
        let resp = tokio::select! {
            r = send => r,
            _ = cancel.cancelled() => Err(abort_error()),
        };
        UPLOADS.lock().remove(&id);
        let resp = resp?;
        if redirect == "error" && resp.status().is_redirection() {
            return Err(cause("Error", "unexpected redirect".into(), None));
        }
        // HTTP/1 keeps the server's reason phrase when it is not the canonical one (an empty
        // phrase included); undici reports it verbatim, and the canonical text for HTTP/2.
        let status_text = match resp.extensions().get::<hyper::ext::ReasonPhrase>() {
            Some(reason) => String::from_utf8_lossy(reason.as_bytes()).into_owned(),
            None => resp.status().canonical_reason().unwrap_or("").to_string(),
        };
        let head = Head {
            status: resp.status().as_u16(),
            status_text,
            url: resp.url().to_string(),
            headers: resp
                .headers()
                .iter()
                .map(|(k, v)| (k.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
                .collect(),
        };
        BODIES.lock().insert(id, (Arc::new(tokio::sync::Mutex::new(Some(resp))), body_timeout));
        Ok(head)
    };
    Ok(engine::spawn_result(ctx, fut, move |head: Head, ctx| {
        let mut flat = Vec::new();
        for (k, v) in &head.headers {
            flat.push(js_str(k));
            flat.push(js_str(v));
        }
        let headers = array(ctx, flat);
        object(
            ctx,
            vec![
                ("status", JsValue::from(head.status)),
                ("statusText", js_str(&head.status_text)),
                ("url", js_str(&head.url)),
                ("headers", headers),
            ],
        )
    }))
}

/// `ops.fetchWrite(id, chunk | null) -> Promise<boolean>`: queue a chunk of a streamed request
/// body (`null` ends it). Resolves `false` once the request no longer takes a body.
fn op_fetch_write(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = arg_f64(args, 0, ctx)? as u64;
    let chunk = args.get_or_undefined(1);
    if chunk.is_null_or_undefined() {
        let open = UPLOADS.lock().remove(&id).is_some();
        return Ok(engine::spawn_result(ctx, async move { Ok(open) }, |open, _| Ok(JsValue::from(open))));
    }
    let chunk = bytes_arg(chunk, ctx)?;
    let tx = UPLOADS.lock().get(&id).cloned();
    let fut = async move {
        let Some(tx) = tx else { return Ok(false) };
        Ok(tx.send(Ok(chunk)).await.is_ok())
    };
    Ok(engine::spawn_result(ctx, fut, |ok: bool, _| Ok(JsValue::from(ok))))
}

/// `ops.fetchWriteError(id, message)`: fail a streamed request body (the request errors).
fn op_fetch_write_error(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = arg_f64(args, 0, ctx)? as u64;
    let message = arg_string(args, 1, ctx).unwrap_or_default();
    if let Some(tx) = UPLOADS.lock().remove(&id) {
        let _ = tx.try_send(Err(std::io::Error::other(message)));
    }
    Ok(JsValue::undefined())
}

/// `ops.fetchRead(id) -> Promise<Uint8Array | null>`
fn op_fetch_read(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = arg_f64(args, 0, ctx)? as u64;
    let body = BODIES.lock().get(&id).cloned();
    let cancel = token(id);
    let fut = async move {
        let Some((body, idle)) = body else { return Ok(None) };
        let mut guard = body.lock().await;
        let Some(resp) = guard.as_mut() else { return Ok(None) };
        let read = async {
            let chunk = match idle {
                Some(t) => tokio::time::timeout(t, resp.chunk())
                    .await
                    .map_err(|_| cause("BodyTimeoutError", "Body Timeout Error".into(), Some("UND_ERR_BODY_TIMEOUT")))?,
                None => resp.chunk().await,
            };
            chunk.map_err(|e| {
                let text = std::error::Error::source(&e).map(|s| s.to_string()).unwrap_or_else(|| e.to_string());
                cause("SocketError", if text.is_empty() { "other side closed".into() } else { text }, Some("UND_ERR_SOCKET"))
            })
        };
        let chunk = tokio::select! {
            c = read => c?,
            _ = cancel.cancelled() => return Err(abort_error()),
        };
        match chunk {
            Some(c) => Ok(Some(c.to_vec())),
            None => {
                *guard = None;
                drop(guard);
                BODIES.lock().remove(&id);
                TOKENS.lock().remove(&id);
                Ok(None)
            }
        }
    };
    Ok(engine::spawn_result(ctx, fut, |chunk: Option<Vec<u8>>, ctx| match chunk {
        Some(c) => bytes_to_js(&c, ctx),
        None => Ok(JsValue::null()),
    }))
}

/// `ops.fetchCancel(id)`: abort an in-flight request / body and free it.
fn op_fetch_cancel(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = arg_f64(args, 0, ctx)? as u64;
    if let Some(t) = TOKENS.lock().remove(&id) {
        t.cancel();
    }
    BODIES.lock().remove(&id);
    UPLOADS.lock().remove(&id);
    Ok(JsValue::undefined())
}

fn op_fetch_id(_: &JsValue, _: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::from(engine::state(ctx).next_id() as f64))
}

pub(crate) const OPS: &[(&str, usize, OpFn)] = &[
    ("fetchId", 0, op_fetch_id),
    ("fetchStart", 9, op_fetch_start),
    ("fetchWrite", 2, op_fetch_write),
    ("fetchWriteError", 2, op_fetch_write_error),
    ("fetchRead", 1, op_fetch_read),
    ("fetchCancel", 1, op_fetch_cancel),
];
