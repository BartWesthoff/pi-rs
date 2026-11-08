//! Provider HTTP through a JavaScript `fetch`, so an extension's `fetch` patch (or its
//! `options.fetch`) applies to the requests pi makes on its behalf.
//!
//! The request is marshalled to the extension JS thread, which calls the *current*
//! `globalThis.fetch` (or the extension's own `fetch`) and streams `Response.body` back one chunk
//! per call, so a slow consumer stops the JavaScript side too (backpressure). A response whose
//! body is an untouched built-in `fetch` stream hands its Rust stream over instead
//! ([`pi_jsrt::take_native_body`]), which keeps a pass-through patch at native speed. Abort maps
//! to the mirrored `AbortSignal`, and a JavaScript failure becomes a [`TransportError::Network`]
//! carrying the thrown message.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use tokio_util::bytes::Bytes;
use futures::future::BoxFuture;
use futures::StreamExt;
use pi_ai::transport::{
    HttpBody, HttpRequest, HttpResponse, HttpTransport, ResponseBody, TransportError,
};
use pi_ai::utils::http::NetworkError;
use pi_jsrt::JsHandle;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::Bridge;

/// Chunks the JavaScript side is asked for, one at a time; each read is one host call.
pub(crate) struct JsFetchTransport {
    bridge: Arc<Bridge>,
    /// The extension's `options.fetch`; `None` calls the current `globalThis.fetch`.
    fetch: Option<JsHandle>,
}

impl JsFetchTransport {
    pub(crate) fn new(bridge: Arc<Bridge>, fetch: Option<JsHandle>) -> Self {
        Self { bridge, fetch }
    }
}

impl HttpTransport for JsFetchTransport {
    fn send(&self, request: HttpRequest) -> BoxFuture<'static, Result<HttpResponse, TransportError>> {
        let bridge = self.bridge.clone();
        let fetch = self.fetch.clone();
        Box::pin(async move {
            // Mirrors the caller's token into an AbortSignal the JavaScript side aborts on drop.
            let (signal, guard) = match &request.signal {
                Some(token) => {
                    let (value, guard) = bridge.signal_for(token);
                    (value, Some(guard))
                }
                None => (Value::Null, None),
            };
            let body = match request.body {
                HttpBody::Empty => Value::Null,
                HttpBody::Bytes(bytes) => Value::String(BASE64.encode(&bytes)),
                // No provider streams request bodies today; buffering keeps one code path.
                HttpBody::Stream(mut stream) => {
                    let mut bytes = Vec::new();
                    while let Some(chunk) = stream.next().await {
                        bytes.extend_from_slice(&chunk?);
                    }
                    Value::String(BASE64.encode(&bytes))
                }
            };
            let headers: Vec<Value> = request
                .headers
                .iter()
                .flat_map(|(k, v)| [Value::String(k.clone()), Value::String(v.clone())])
                .collect();
            let payload = json!({
                "url": request.url,
                "method": request.method.as_str(),
                "headers": headers,
                "body": body,
                "signal": signal,
                "fetch": fetch.as_ref().map(|h| h.to_value()),
            });
            let send = bridge.glue_call("hostFetchStart", vec![payload]);
            let head = match request.timeout {
                Some(limit) => match tokio::time::timeout(limit, send).await {
                    Ok(result) => result,
                    Err(_) => {
                        return Err(TransportError::Timeout(NetworkError {
                            code: Some("ETIMEDOUT"),
                            message: "headers timeout".into(),
                        }));
                    }
                },
                None => send.await,
            };
            let head = head.map_err(js_failure)?;
            let id = head["id"].as_u64().unwrap_or_default();
            let status = head["status"].as_u64().unwrap_or(0) as u16;
            let status_text = head["statusText"].as_str().unwrap_or_default().to_string();
            let mut headers = reqwest::header::HeaderMap::new();
            if let Some(pairs) = head["headers"].as_array() {
                for pair in pairs.chunks(2) {
                    let (Some(name), Some(value)) = (pair.first().and_then(Value::as_str), pair.get(1).and_then(Value::as_str))
                    else {
                        continue;
                    };
                    if let (Ok(name), Ok(value)) = (
                        reqwest::header::HeaderName::from_bytes(name.as_bytes()),
                        reqwest::header::HeaderValue::from_str(value),
                    ) {
                        headers.append(name, value);
                    }
                }
            }
            // An untouched built-in body is handed over as a Rust stream: no copying through JS.
            let body: ResponseBody = match head["nativeId"].as_u64().and_then(pi_jsrt::take_native_body) {
                Some(native) => Box::pin(native_stream(native, guard)),
                None => Box::pin(pull_stream(bridge, id, CancellationToken::new(), guard)),
            };
            Ok(HttpResponse { status, status_text, headers, body })
        })
    }
}

/// Chunks of a built-in `fetch` response, read straight from its Rust stream.
fn native_stream(
    body: pi_jsrt::NativeBody,
    signal_guard: Option<super::SignalGuard>,
) -> impl futures::Stream<Item = Result<Bytes, TransportError>> {
    futures::stream::unfold((body, signal_guard), |(mut body, signal_guard)| async move {
        body.next_chunk().await.map(|chunk| {
            let item = chunk.map_err(|e| TransportError::Network(NetworkError { code: None, message: e.message }));
            (item, (body, signal_guard))
        })
    })
}

/// Chunks of a JavaScript `Response`, one host call per read (so the JavaScript side is paced by
/// this consumer). Cancelling the stream cancels the JavaScript request.
fn pull_stream(
    bridge: Arc<Bridge>,
    id: u64,
    done: CancellationToken,
    signal_guard: Option<super::SignalGuard>,
) -> impl futures::Stream<Item = Result<Bytes, TransportError>> {
    let guard = CancelOnDrop { bridge: bridge.clone(), id, done: done.clone() };
    futures::stream::unfold((bridge, id, done, guard, signal_guard), |(bridge, id, done, guard, signal_guard)| async move {
        if done.is_cancelled() {
            return None;
        }
        let chunk = bridge.glue_call("hostFetchRead", vec![json!(id)]).await;
        match chunk {
            Ok(Value::Null) => None,
            Ok(Value::String(text)) => match BASE64.decode(text) {
                Ok(bytes) => Some((Ok(Bytes::from(bytes)), (bridge, id, done, guard, signal_guard))),
                Err(e) => Some((Err(TransportError::Network(NetworkError { code: None, message: e.to_string() })), (bridge, id, done, guard, signal_guard))),
            },
            Ok(other) => Some((
                Err(TransportError::Network(NetworkError { code: None, message: format!("unexpected fetch chunk: {other}") })),
                (bridge, id, done, guard, signal_guard),
            )),
            Err(e) => Some((Err(js_failure(e)), (bridge, id, done, guard, signal_guard))),
        }
    })
}

/// Cancels the JavaScript request when the stream is dropped before its end.
struct CancelOnDrop {
    bridge: Arc<Bridge>,
    id: u64,
    done: CancellationToken,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.done.is_cancelled() {
            return;
        }
        let bridge = self.bridge.clone();
        let id = self.id;
        super::tokio_spawn(async move {
            let _ = bridge.glue_call("hostFetchCancel", vec![json!(id)]).await;
        });
    }
}

/// A rejection from the JavaScript `fetch`: its message becomes the provider error, as the TS SDK
/// reports a thrown `fetch` error.
fn js_failure(error: anyhow::Error) -> TransportError {
    let text = error.to_string();
    let code = if text.contains("AbortError") || text.contains("aborted") {
        "ABORT_ERR"
    } else {
        pi_ai::utils::http::JS_FETCH_CODE
    };
    TransportError::Network(NetworkError { code: Some(code), message: text })
}
