//! Provider HTTP and WebSocket transports: the seam where TypeScript's `fetch` and `WebSocket`
//! sit.
//!
//! Every provider request goes through an [`HttpTransport`] (and the Codex WebSocket through a
//! [`WsTransport`]). The defaults are [`ReqwestTransport`] and, on native targets,
//! [`TungsteniteTransport`]; [`StreamOptions::http_transport`](crate::types::StreamOptions::http_transport)
//! and [`StreamOptions::ws_transport`](crate::types::StreamOptions::ws_transport) replace them.
//!
//! A transport only moves bytes. Retries, timeouts, abort handling, request compression,
//! keep-alive tuning, proxy resolution of the default clients and WebSocket session caching all
//! stay in the provider code above it, so every transport sees the exact request the provider
//! built.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use futures::{Sink, Stream, StreamExt};
use tokio_util::sync::CancellationToken;

use crate::types::{ProviderEnv, StreamOptions};
use crate::utils::http::NetworkError;

pub use reqwest::Method;
pub use reqwest::header::HeaderMap;

/// A failure inside a transport: the request could not be sent, or its body could not be read.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TransportError {
    /// A failure of the reqwest client behind [`ReqwestTransport`].
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
    /// A network failure reported by another transport, worded like Node's network stack.
    #[error(transparent)]
    Network(NetworkError),
    /// The request or body read exceeded its deadline ([`HttpRequest::timeout`] or an idle
    /// timeout).
    #[error(transparent)]
    Timeout(NetworkError),
    /// The body is not valid JSON ([`HttpResponse::json`]).
    #[error("error decoding response body")]
    Decode(#[source] serde_json::Error),
}

impl TransportError {
    /// `true` when a deadline elapsed rather than the connection failing.
    pub fn is_timeout(&self) -> bool {
        match self {
            Self::Reqwest(error) => error.is_timeout(),
            Self::Timeout(_) => true,
            Self::Network(_) | Self::Decode(_) => false,
        }
    }

    /// The failure as Node's network stack words it (the `cause` of `TypeError: fetch failed`).
    pub fn network_error(&self) -> NetworkError {
        match self {
            Self::Reqwest(error) => NetworkError::from_reqwest(error),
            Self::Network(error) | Self::Timeout(error) => error.clone(),
            Self::Decode(error) => NetworkError { code: None, message: error.to_string() },
        }
    }
}

/// The body of an [`HttpRequest`].
#[derive(Default)]
pub enum HttpBody {
    /// No body.
    #[default]
    Empty,
    /// A complete body.
    Bytes(Bytes),
    /// A body streamed in chunks.
    Stream(BoxStream<'static, Result<Bytes, TransportError>>),
}

impl fmt::Debug for HttpBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("Empty"),
            Self::Bytes(bytes) => f.debug_tuple("Bytes").field(&bytes.len()).finish(),
            Self::Stream(_) => f.write_str("Stream"),
        }
    }
}

impl From<Vec<u8>> for HttpBody {
    fn from(bytes: Vec<u8>) -> Self {
        Self::Bytes(bytes.into())
    }
}

impl From<String> for HttpBody {
    fn from(text: String) -> Self {
        Self::Bytes(text.into())
    }
}

impl From<Bytes> for HttpBody {
    fn from(bytes: Bytes) -> Self {
        Self::Bytes(bytes)
    }
}

/// One outgoing HTTP request, exactly as the provider built it.
#[derive(Debug)]
pub struct HttpRequest {
    pub method: Method,
    pub url: String,
    /// Header lines in send order. Names are matched case-insensitively by the server; a
    /// transport sends them as given, and an invalid name or value fails the send.
    pub headers: Vec<(String, String)>,
    pub body: HttpBody,
    /// Deadline for the whole exchange, body included (reqwest's per-request `timeout`). An
    /// elapsed deadline fails with an error whose [`TransportError::is_timeout`] is `true`.
    pub timeout: Option<Duration>,
    /// The caller's abort signal. Callers still race it themselves; a transport may use it to
    /// stop underlying work early.
    pub signal: Option<CancellationToken>,
}

impl HttpRequest {
    /// A request without headers or body.
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Self { method, url: url.into(), headers: Vec::new(), body: HttpBody::Empty, timeout: None, signal: None }
    }

    /// A `GET` request.
    pub fn get(url: impl Into<String>) -> Self {
        Self::new(Method::GET, url)
    }

    /// A `POST` request.
    pub fn post(url: impl Into<String>) -> Self {
        Self::new(Method::POST, url)
    }

    /// Append one header line.
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Append header lines in order.
    #[must_use]
    pub fn headers<N: Into<String>, V: Into<String>>(mut self, headers: impl IntoIterator<Item = (N, V)>) -> Self {
        self.headers.extend(headers.into_iter().map(|(n, v)| (n.into(), v.into())));
        self
    }

    /// Set the body.
    #[must_use]
    pub fn body(mut self, body: impl Into<HttpBody>) -> Self {
        self.body = body.into();
        self
    }

    /// Set the whole-exchange deadline.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Attach the caller's abort signal.
    #[must_use]
    pub fn signal(mut self, signal: Option<CancellationToken>) -> Self {
        self.signal = signal;
        self
    }
}

/// A streamed response body.
pub type ResponseBody = BoxStream<'static, Result<Bytes, TransportError>>;

/// The response head plus a streamed body.
pub struct HttpResponse {
    pub status: u16,
    pub status_text: String,
    pub headers: HeaderMap,
    pub body: ResponseBody,
}

impl fmt::Debug for HttpResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("status_text", &self.status_text)
            .field("headers", &self.headers)
            .finish_non_exhaustive()
    }
}

/// The IANA reason phrase of `status` (`""` when unknown), as Node reports `statusText`.
pub fn canonical_status_text(status: u16) -> String {
    reqwest::StatusCode::from_u16(status).ok().and_then(|s| s.canonical_reason()).unwrap_or_default().to_string()
}

impl HttpResponse {
    /// A response with a complete body (handy for mock transports).
    pub fn from_bytes(status: u16, headers: HeaderMap, body: impl Into<Bytes>) -> Self {
        let body: Bytes = body.into();
        Self {
            status,
            status_text: canonical_status_text(status),
            headers,
            body: futures::stream::once(async move { Ok(body) }).boxed(),
        }
    }

    /// `response.ok`: a 2xx status.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// The next body chunk; `Ok(None)` at the end.
    ///
    /// # Errors
    /// Fails when the body read fails.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, TransportError> {
        self.body.next().await.transpose()
    }

    /// The whole body.
    ///
    /// # Errors
    /// Fails when the body read fails.
    pub async fn bytes(mut self) -> Result<Bytes, TransportError> {
        let Some(first) = self.chunk().await? else { return Ok(Bytes::new()) };
        let Some(second) = self.chunk().await? else { return Ok(first) };
        let mut all = BytesMut::with_capacity(first.len() + second.len());
        all.extend_from_slice(&first);
        all.extend_from_slice(&second);
        while let Some(chunk) = self.chunk().await? {
            all.extend_from_slice(&chunk);
        }
        Ok(all.freeze())
    }

    /// The whole body decoded as UTF-8 (a leading byte-order mark dropped, invalid sequences
    /// replaced), like `response.text()`.
    ///
    /// # Errors
    /// Fails when the body read fails.
    pub async fn text(self) -> Result<String, TransportError> {
        let bytes = self.bytes().await?;
        let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
        Ok(String::from_utf8_lossy(bytes).into_owned())
    }

    /// The whole body parsed as JSON.
    ///
    /// # Errors
    /// Fails when the body read fails or the body is not valid JSON.
    pub async fn json<T: serde::de::DeserializeOwned>(self) -> Result<T, TransportError> {
        let bytes = self.bytes().await?;
        serde_json::from_slice(&bytes).map_err(TransportError::Decode)
    }
}

/// Sends HTTP requests for the providers (TypeScript's `fetch`).
pub trait HttpTransport: Send + Sync {
    /// Send `request` and resolve once the response head arrives.
    fn send(&self, request: HttpRequest) -> BoxFuture<'static, Result<HttpResponse, TransportError>>;
}

/// The default transport: a reqwest client.
#[derive(Debug, Clone)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    /// A transport sending through `client`.
    pub fn new(client: reqwest::Client) -> Self {
        Self { client }
    }

    /// The shared, proxy-aware client for requests to `target_url`
    /// ([`client_for`](crate::utils::http::client_for)).
    ///
    /// # Errors
    /// Fails when the configured proxy URL is invalid.
    pub fn for_target(target_url: &str, env: Option<&ProviderEnv>) -> anyhow::Result<Self> {
        Ok(Self::new(crate::utils::http::client_for(target_url, env)?))
    }

    /// The underlying client.
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    fn build(&self, request: HttpRequest) -> Result<reqwest::RequestBuilder, TransportError> {
        let mut builder = self.client.request(request.method, &request.url);
        for (name, value) in &request.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        builder = match request.body {
            HttpBody::Empty => builder,
            HttpBody::Bytes(bytes) => builder.body(bytes),
            #[cfg(not(target_arch = "wasm32"))]
            HttpBody::Stream(stream) => builder.body(reqwest::Body::wrap_stream(stream)),
            #[cfg(target_arch = "wasm32")]
            HttpBody::Stream(_) => {
                return Err(TransportError::Network(NetworkError {
                    code: None,
                    message: "streaming request bodies are not supported on wasm32".into(),
                }));
            }
        };
        if let Some(timeout) = request.timeout {
            builder = builder.timeout(timeout);
        }
        Ok(builder)
    }
}

impl From<reqwest::Client> for ReqwestTransport {
    fn from(client: reqwest::Client) -> Self {
        Self::new(client)
    }
}

impl HttpTransport for ReqwestTransport {
    fn send(&self, request: HttpRequest) -> BoxFuture<'static, Result<HttpResponse, TransportError>> {
        let builder = self.build(request);
        crate::utils::wasm::boxed(async move {
            let response = builder?.send().await?;
            let status = response.status();
            Ok(HttpResponse {
                status: status.as_u16(),
                status_text: status.canonical_reason().unwrap_or_default().to_string(),
                headers: response.headers().clone(),
                body: Box::pin(crate::utils::wasm::assert_send(
                    response.bytes_stream().map(|chunk| chunk.map_err(TransportError::from)),
                )),
            })
        })
    }
}

/// The transport for an HTTP request to `url`: the caller's
/// [`StreamOptions::http_transport`], else the shared proxy-aware reqwest client.
///
/// # Errors
/// Fails when the configured proxy URL is invalid.
pub fn http_transport_for(url: &str, options: &StreamOptions) -> anyhow::Result<Arc<dyn HttpTransport>> {
    match &options.http_transport {
        Some(transport) => Ok(transport.clone()),
        None => Ok(Arc::new(ReqwestTransport::for_target(url, options.env.as_ref())?)),
    }
}

// ============================================================================
// WebSocket
// ============================================================================

/// A WebSocket close frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WsCloseFrame {
    pub code: u16,
    pub reason: String,
}

/// One WebSocket message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsMessage {
    Text(String),
    Binary(Bytes),
    Ping(Bytes),
    Pong(Bytes),
    Close(Option<WsCloseFrame>),
}

/// A WebSocket failure, carrying the message the `ws` package's `error` event reports.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct WsError {
    pub message: String,
}

impl WsError {
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }
}

/// An open WebSocket: a stream of received messages and a sink for outgoing ones.
pub trait WsConnection:
    Stream<Item = Result<WsMessage, WsError>> + Sink<WsMessage, Error = WsError> + Send + Unpin
{
}

impl<T> WsConnection for T where
    T: Stream<Item = Result<WsMessage, WsError>> + Sink<WsMessage, Error = WsError> + Send + Unpin
{
}

/// A boxed [`WsConnection`].
pub type BoxWsConnection = Box<dyn WsConnection>;

/// An outgoing WebSocket handshake.
#[derive(Debug, Clone)]
pub struct WsConnectRequest {
    pub url: String,
    /// Handshake header lines in order; a later line replaces an earlier one with the same name.
    pub headers: Vec<(String, String)>,
    /// The caller's abort signal. Callers still race it (and their connect timeout) themselves.
    pub signal: Option<CancellationToken>,
}

/// Opens WebSockets for the providers (TypeScript's `WebSocket`).
pub trait WsTransport: Send + Sync {
    /// Open a WebSocket and resolve once the handshake completes.
    fn connect(&self, request: WsConnectRequest) -> BoxFuture<'static, anyhow::Result<BoxWsConnection>>;
}

/// The transport for a WebSocket: the caller's [`StreamOptions::ws_transport`], else
/// [`TungsteniteTransport`] tunneling through the environment's proxy.
#[cfg(not(target_arch = "wasm32"))]
pub fn ws_transport_for(options: &StreamOptions) -> Arc<dyn WsTransport> {
    match &options.ws_transport {
        Some(transport) => transport.clone(),
        None => Arc::new(TungsteniteTransport::new(options.env.clone())),
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use tungstenite_transport::TungsteniteTransport;

#[cfg(not(target_arch = "wasm32"))]
mod tungstenite_transport {
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use base64::Engine;
    use futures::future::BoxFuture;
    use futures::{Sink, Stream};
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::protocol::CloseFrame;
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
    use tokio_tungstenite::tungstenite::{Error as TungsteniteError, Message};
    use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

    use super::{BoxWsConnection, WsCloseFrame, WsConnectRequest, WsError, WsMessage, WsTransport};
    use crate::types::ProviderEnv;
    use crate::utils::node_http_proxy::get_proxy_url_for_target;

    trait AsyncIo: AsyncRead + AsyncWrite + Send + Unpin {}
    impl<T: AsyncRead + AsyncWrite + Send + Unpin> AsyncIo for T {}

    type Socket = WebSocketStream<MaybeTlsStream<Box<dyn AsyncIo>>>;

    /// The default WebSocket transport: tokio-tungstenite over TCP (or an HTTP `CONNECT` tunnel
    /// when the environment configures a proxy for the target).
    #[derive(Debug, Clone, Default)]
    pub struct TungsteniteTransport {
        env: Option<ProviderEnv>,
    }

    impl TungsteniteTransport {
        /// A transport resolving proxies from `env` (then the process environment).
        pub fn new(env: Option<ProviderEnv>) -> Self {
            Self { env }
        }
    }

    impl WsTransport for TungsteniteTransport {
        fn connect(&self, request: WsConnectRequest) -> BoxFuture<'static, anyhow::Result<BoxWsConnection>> {
            let env = self.env.clone();
            Box::pin(async move {
                let parsed = url::Url::parse(&request.url)?;
                let mut handshake = request.url.as_str().into_client_request()?;
                for (name, value) in &request.headers {
                    let name = reqwest::header::HeaderName::from_bytes(name.to_lowercase().as_bytes())?;
                    handshake.headers_mut().insert(name, reqwest::header::HeaderValue::from_str(value)?);
                }
                let stream = open_tcp(&parsed, env.as_ref()).await?;
                let (socket, _) = tokio_tungstenite::client_async_tls_with_config(handshake, stream, None, None)
                    .await
                    .map_err(|e| anyhow::anyhow!(websocket_error_message(&e)))?;
                Ok(Box::new(TungsteniteConnection(socket)) as BoxWsConnection)
            })
        }
    }

    /// The `ws` package's wording for a tungstenite failure.
    fn websocket_error_message(error: &TungsteniteError) -> String {
        match error {
            TungsteniteError::Http(response) => {
                format!("Unexpected server response: {}", response.status().as_u16())
            }
            other => {
                let text = other.to_string();
                if text.is_empty() { "WebSocket error".to_string() } else { text }
            }
        }
    }

    fn ws_error(error: TungsteniteError) -> WsError {
        WsError::new(websocket_error_message(&error))
    }

    struct TungsteniteConnection(Socket);

    impl Stream for TungsteniteConnection {
        type Item = Result<WsMessage, WsError>;

        fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            loop {
                let message = match Pin::new(&mut self.0).poll_next(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(None) => return Poll::Ready(None),
                    Poll::Ready(Some(Err(error))) => return Poll::Ready(Some(Err(ws_error(error)))),
                    Poll::Ready(Some(Ok(message))) => message,
                };
                let message = match message {
                    Message::Text(text) => WsMessage::Text(text.to_string()),
                    Message::Binary(bytes) => WsMessage::Binary(bytes),
                    Message::Ping(bytes) => WsMessage::Ping(bytes),
                    Message::Pong(bytes) => WsMessage::Pong(bytes),
                    Message::Close(frame) => WsMessage::Close(
                        frame.map(|f| WsCloseFrame { code: u16::from(f.code), reason: f.reason.to_string() }),
                    ),
                    // Raw frames are never yielded while reading.
                    Message::Frame(_) => continue,
                };
                return Poll::Ready(Some(Ok(message)));
            }
        }
    }

    impl Sink<WsMessage> for TungsteniteConnection {
        type Error = WsError;

        fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), WsError>> {
            Pin::new(&mut self.0).poll_ready(cx).map_err(ws_error)
        }

        fn start_send(mut self: Pin<&mut Self>, item: WsMessage) -> Result<(), WsError> {
            let message = match item {
                WsMessage::Text(text) => Message::text(text),
                WsMessage::Binary(bytes) => Message::Binary(bytes),
                WsMessage::Ping(bytes) => Message::Ping(bytes),
                WsMessage::Pong(bytes) => Message::Pong(bytes),
                WsMessage::Close(frame) => Message::Close(
                    frame.map(|f| CloseFrame { code: CloseCode::from(f.code), reason: f.reason.into() }),
                ),
            };
            Pin::new(&mut self.0).start_send(message).map_err(ws_error)
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), WsError>> {
            Pin::new(&mut self.0).poll_flush(cx).map_err(ws_error)
        }

        fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), WsError>> {
            Pin::new(&mut self.0).poll_close(cx).map_err(ws_error)
        }
    }

    /// Open a TCP connection to `host:port`, tunneling through an HTTP proxy when configured.
    async fn open_tcp(target_url: &url::Url, env: Option<&ProviderEnv>) -> anyhow::Result<Box<dyn AsyncIo>> {
        let host = target_url.host_str().ok_or_else(|| anyhow::anyhow!("Invalid WebSocket URL"))?.to_string();
        let port = target_url.port_or_known_default().unwrap_or(443);
        let http_target = target_url.as_str().replacen("wss:", "https:", 1).replacen("ws:", "http:", 1);
        if let Some(proxy) = get_proxy_url_for_target(&http_target, env).and_then(|p| url::Url::parse(&p).ok())
            && matches!(proxy.scheme(), "http" | "https")
        {
            let proxy_host = proxy.host_str().unwrap_or_default().to_string();
            let proxy_port = proxy.port_or_known_default().unwrap_or(80);
            let mut stream = tokio::net::TcpStream::connect((proxy_host.as_str(), proxy_port)).await?;
            let host_port = if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") };
            let mut request = format!("CONNECT {host_port} HTTP/1.1\r\nHost: {host_port}\r\n");
            if !proxy.username().is_empty() {
                let user = percent_decode(proxy.username());
                let pass = percent_decode(proxy.password().unwrap_or_default());
                let token = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
                request.push_str(&format!("Proxy-Authorization: Basic {token}\r\n"));
            }
            request.push_str("\r\n");
            stream.write_all(request.as_bytes()).await?;
            let mut response = Vec::new();
            let mut byte = [0u8; 1];
            while !response.ends_with(b"\r\n\r\n") {
                if stream.read(&mut byte).await? == 0 {
                    anyhow::bail!("Proxy closed the connection during CONNECT");
                }
                response.push(byte[0]);
                if response.len() > 64 * 1024 {
                    anyhow::bail!("Proxy CONNECT response too large");
                }
            }
            let head = String::from_utf8_lossy(&response);
            let status = head.split_whitespace().nth(1).unwrap_or_default();
            if status != "200" {
                anyhow::bail!("Proxy CONNECT failed: {}", head.lines().next().unwrap_or_default());
            }
            return Ok(Box::new(stream));
        }
        let stream = tokio::net::TcpStream::connect((host.as_str(), port)).await?;
        let _ = stream.set_nodelay(true);
        Ok(Box::new(stream))
    }

    fn percent_decode(value: &str) -> String {
        url::form_urlencoded::parse(format!("x={value}").as_bytes())
            .next()
            .map(|(_, v)| v.into_owned())
            .unwrap_or_else(|| value.to_string())
    }
}
