//! Provider HTTP error normalization (port of `utils/error-body.ts`).
//!
//! The TypeScript version probes SDK error objects. The Rust port has one HTTP error type,
//! [`ProviderHttpError`], produced by the HTTP layer of every API implementation.

use reqwest::header::HeaderMap;
use serde_json::Value;

pub const MAX_PROVIDER_ERROR_BODY_CHARS: usize = 4000;

/// Which SDK's error-message convention to reproduce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdkErrorStyle {
    /// `@anthropic-ai/sdk`: the whole parsed JSON body is the error object.
    Anthropic,
    /// `openai`: the body's `error` field is the error object.
    OpenAI,
}

/// An HTTP-level provider error (the equivalent of the SDKs' `APIError`).
/// `status: None` is a connection error (`APIConnectionError`).
#[derive(Debug, Clone)]
pub struct ProviderHttpError {
    pub status: Option<u16>,
    pub headers: Option<HeaderMap>,
    /// Display message, formatted like the SDK error message.
    pub message: String,
    /// Raw response body text, when one was read.
    pub body: Option<String>,
    /// The SDK's `error` field: the parsed error object.
    pub error: Option<Value>,
    pub name: String,
}

impl std::fmt::Display for ProviderHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ProviderHttpError {}

/// JavaScript truthiness of a JSON value.
fn js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// `APIError.makeMessage` of the `openai`/`@anthropic-ai/sdk` SDKs.
fn sdk_make_message(status: Option<u16>, error: Option<&Value>, message: Option<&str>) -> String {
    use crate::utils::js_json::js_json_stringify;
    let msg: Option<String> = match error.filter(|e| js_truthy(e)) {
        Some(err) => match err.get("message").filter(|m| js_truthy(m)) {
            Some(Value::String(s)) => Some(s.clone()),
            Some(m) => Some(js_json_stringify(m)),
            None => Some(js_json_stringify(err)),
        },
        None => message.map(str::to_string),
    };
    let msg = msg.filter(|m| !m.is_empty());
    match (status, msg) {
        (Some(status), Some(msg)) => format!("{status} {msg}"),
        (Some(status), None) => format!("{status} status code (no body)"),
        (None, Some(msg)) => msg,
        (None, None) => "(no status code or body)".to_string(),
    }
}

impl ProviderHttpError {
    /// Build an HTTP status error the way the SDK would from a response body.
    pub fn from_response(status: u16, headers: HeaderMap, body_text: String, style: SdkErrorStyle) -> Self {
        let parsed: Option<Value> = serde_json::from_str(&body_text).ok();
        let (error, message) = match style {
            SdkErrorStyle::Anthropic => match &parsed {
                Some(json) => (Some(json.clone()), None),
                None => (None, Some(body_text.clone())),
            },
            SdkErrorStyle::OpenAI => match &parsed {
                Some(json) => (json.get("error").cloned(), None),
                None => (None, Some(body_text.clone())),
            },
        };
        let display = sdk_make_message(Some(status), error.as_ref(), message.as_deref());
        ProviderHttpError {
            status: Some(status),
            headers: Some(headers),
            message: display,
            body: if body_text.is_empty() { None } else { Some(body_text) },
            error,
            name: status_error_name(status).to_string(),
        }
    }

    /// A connection-level failure (no HTTP status).
    pub fn connection(message: impl Into<String>) -> Self {
        ProviderHttpError {
            status: None,
            headers: None,
            message: message.into(),
            body: None,
            error: None,
            name: "APIConnectionError".to_string(),
        }
    }

    /// The SDKs' `APIConnectionTimeoutError`: no response arrived within the request timeout.
    pub fn timed_out() -> Self {
        let mut e = Self::connection("Request timed out.");
        e.name = "APIConnectionTimeoutError".to_string();
        e
    }

    /// Map a reqwest transport error to the SDK's connection error messages.
    pub fn from_reqwest(error: &reqwest::Error) -> Self {
        if error.is_timeout() {
            Self::timed_out()
        } else if let Some(message) = local_connection_refused_message(
            error.url(),
            &super::http::NetworkError::from_reqwest(error),
        ) {
            // pi-rs divergence (LM-04): a loopback server that refuses the connection is
            // simply not running, so name its address instead of the SDKs' bare message.
            Self::connection(message)
        } else {
            Self::connection("Connection error.")
        }
    }

    /// Map a [`TransportError`](crate::transport::TransportError) to the SDK's connection error
    /// messages.
    pub fn from_transport(error: &crate::transport::TransportError) -> Self {
        if let crate::transport::TransportError::Reqwest(error) = error {
            return Self::from_reqwest(error);
        }
        if error.is_timeout() {
            return Self::timed_out();
        }
        // A rejected JavaScript `fetch` (an extension's patch) reports its own message, as the
        // SDK does; a network failure keeps the bare SDK wording.
        if let crate::transport::TransportError::Network(e) = error
            && let Some(message) = e.js_fetch_message()
        {
            return Self::connection(message);
        }
        // Like the SDKs' APIConnectionError: the cause is not part of the message.
        Self::connection("Connection error.")
    }

    /// `true` for the loopback "connection refused" error of [`Self::from_reqwest`]: no local
    /// server is listening, so retrying within seconds cannot succeed.
    pub fn is_local_connection_refused(&self) -> bool {
        self.status.is_none() && is_local_connection_refused_message(&self.message)
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.as_ref().and_then(|h| h.get(name)).and_then(|v| v.to_str().ok())
    }
}

/// The part of the loopback "connection refused" message that marks it (pi-rs divergence LM-04).
pub const LOCAL_CONNECTION_REFUSED_HINT: &str = "no server is listening; is the local server running?";

/// `true` when `message` is (or contains) the loopback "connection refused" error text.
pub fn is_local_connection_refused_message(message: &str) -> bool {
    message.contains(LOCAL_CONNECTION_REFUSED_HINT)
}

/// `true` when `url` points at this machine (`localhost`, `*.localhost`, `127.0.0.0/8`, `::1`).
pub fn is_loopback_url(url: &reqwest::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(domain)) => {
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            domain == "localhost" || domain.ends_with(".localhost")
        }
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// `Connection refused: <address> (...)` when a request to a loopback URL was refused; `None`
/// for every other failure and for non-loopback hosts, which keep the SDKs' message.
fn local_connection_refused_message(url: Option<&reqwest::Url>, cause: &super::http::NetworkError) -> Option<String> {
    if cause.code != Some("ECONNREFUSED") || !url.is_some_and(is_loopback_url) {
        return None;
    }
    let address = cause.message.strip_prefix("connect ECONNREFUSED ").unwrap_or(&cause.message);
    Some(format!("Connection refused: {address} ({LOCAL_CONNECTION_REFUSED_HINT})"))
}

fn status_error_name(status: u16) -> &'static str {
    match status {
        400 => "BadRequestError",
        401 => "AuthenticationError",
        403 => "PermissionDeniedError",
        404 => "NotFoundError",
        409 => "ConflictError",
        422 => "UnprocessableEntityError",
        429 => "RateLimitError",
        s if s >= 500 => "InternalServerError",
        _ => "APIError",
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedProviderError {
    pub status: Option<u16>,
    pub body: Option<String>,
    pub message: String,
    pub message_carries_body: bool,
}

/// Normalize any error into status/body/message.
pub fn normalize_provider_error(error: &anyhow::Error) -> NormalizedProviderError {
    if let Some(http) = error.downcast_ref::<ProviderHttpError>() {
        // Like the SDK error objects: the parsed error body (`error.error`) is the body when it is
        // a non-empty object.
        let body = match &http.error {
            Some(Value::Object(map)) if !map.is_empty() => Some(Value::Object(map.clone()).to_string()),
            _ => None,
        }
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty())
        .map(|b| truncate_error_text(&b, MAX_PROVIDER_ERROR_BODY_CHARS));
        let message_carries_body = match &body {
            None => true,
            Some(b) => http.message.contains(b.as_str()),
        };
        return NormalizedProviderError { status: http.status, body, message: http.message.clone(), message_carries_body };
    }
    NormalizedProviderError { status: None, body: None, message: error.to_string(), message_carries_body: true }
}

/// Compose a display string from a normalized error.
pub fn format_provider_error(norm: &NormalizedProviderError, prefix: Option<&str>) -> String {
    match (norm.message_carries_body, norm.status, &norm.body) {
        (false, Some(status), Some(body)) => match prefix {
            Some(prefix) => format!("{prefix} ({status}): {body}"),
            None => format!("{status}: {body}"),
        },
        _ => match (prefix, norm.status) {
            (Some(prefix), Some(status)) => format!("{prefix} ({status}): {}", norm.message),
            _ => norm.message.clone(),
        },
    }
}

/// Truncate to `max_chars` UTF-16-ish characters (counted as chars here).
pub fn truncate_error_text(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    let head: String = text.chars().take(max_chars).collect();
    format!("{head}... [truncated {} chars]", count - max_chars)
}

pub fn safe_json_stringify(value: &Value) -> String {
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderMap;

    #[test]
    fn sdk_messages_and_normalization() {
        let anthropic = ProviderHttpError::from_response(
            400,
            HeaderMap::new(),
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad"}}"#.into(),
            SdkErrorStyle::Anthropic,
        );
        assert_eq!(anthropic.message, r#"400 {"type":"error","error":{"type":"invalid_request_error","message":"bad"}}"#);
        let norm = normalize_provider_error(&anyhow::Error::new(anthropic));
        assert!(norm.message_carries_body);
        assert_eq!(format_provider_error(&norm, None), norm.message);

        let openai = ProviderHttpError::from_response(
            400,
            HeaderMap::new(),
            r#"{"error":{"message":"Bad things","code":null}}"#.into(),
            SdkErrorStyle::OpenAI,
        );
        assert_eq!(openai.message, "400 Bad things");
        let norm = normalize_provider_error(&anyhow::Error::new(openai));
        assert_eq!(format_provider_error(&norm, None), r#"400: {"message":"Bad things","code":null}"#);

        let no_body = ProviderHttpError::from_response(403, HeaderMap::new(), String::new(), SdkErrorStyle::OpenAI);
        assert_eq!(no_body.message, "403 status code (no body)");
        let detail = ProviderHttpError::from_response(502, HeaderMap::new(), r#"{"detail":"x"}"#.into(), SdkErrorStyle::OpenAI);
        assert_eq!(detail.message, "502 status code (no body)");
        assert_eq!(truncate_error_text("abcdef", 3), "abc... [truncated 3 chars]");
    }

    #[tokio::test]
    async fn connection_failures_read_like_the_sdks() {
        // Loopback refusals name the address (pi-rs divergence LM-04); the name stays the SDK's.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let error = reqwest::Client::new().get(format!("http://{address}/v1")).send().await.unwrap_err();
        let http = ProviderHttpError::from_reqwest(&error);
        assert_eq!(
            http.message,
            format!("Connection refused: {address} (no server is listening; is the local server running?)")
        );
        assert_eq!(http.name, "APIConnectionError");
        assert!(http.status.is_none());
        assert!(http.is_local_connection_refused());

        // Any other connection failure keeps the SDKs' message.
        let other = ProviderHttpError::connection("Connection error.");
        assert!(!other.is_local_connection_refused());
    }

    #[test]
    fn loopback_connection_refused_names_the_address() {
        use super::super::http::NetworkError;
        let refused = |address: &str| NetworkError {
            code: Some("ECONNREFUSED"),
            message: format!("connect ECONNREFUSED {address}"),
        };
        let url = |text: &str| reqwest::Url::parse(text).unwrap();
        for (base, address) in [
            ("http://127.0.0.1:8084/v1", "127.0.0.1:8084"),
            ("http://localhost:8080/v1", "localhost:8080"),
            ("http://[::1]:11434/v1", "::1:11434"),
            ("http://127.1.2.3:9/v1", "127.1.2.3:9"),
        ] {
            let message = local_connection_refused_message(Some(&url(base)), &refused(address)).unwrap();
            assert_eq!(
                message,
                format!("Connection refused: {address} (no server is listening; is the local server running?)")
            );
            assert!(ProviderHttpError::connection(message).is_local_connection_refused());
        }
        // Remote hosts keep the SDK message; other loopback failures too.
        assert_eq!(
            local_connection_refused_message(Some(&url("http://10.0.0.5:8084/v1")), &refused("10.0.0.5:8084")),
            None
        );
        assert_eq!(
            local_connection_refused_message(Some(&url("https://api.openai.com/v1")), &refused("api.openai.com:443")),
            None
        );
        let reset = NetworkError { code: Some("ECONNRESET"), message: "read ECONNRESET".into() };
        assert_eq!(local_connection_refused_message(Some(&url("http://127.0.0.1:8084")), &reset), None);
        assert_eq!(local_connection_refused_message(None, &refused("127.0.0.1:8084")), None);
    }
}
