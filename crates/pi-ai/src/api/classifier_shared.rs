//! Shared HTTP plumbing for classifier APIs (port of `api/classifier-shared.ts`).

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::models::calculate_cost;
use crate::transport::{HttpRequest, HttpTransport, TransportError};
use crate::types::{ClassifierModel, ImageModel, Model, ProviderHeaders, ProviderResponse, StreamOptions, Usage};
use crate::utils::error_body::{
    MAX_PROVIDER_ERROR_BODY_CHARS, NormalizedProviderError, ProviderHttpError, normalize_provider_error,
    truncate_error_text,
};
use crate::utils::headers::{headers_to_record, provider_headers_to_record, to_provider_headers};
use crate::utils::identity::{ClientIdentityTarget, get_identity_headers};
use crate::utils::provider_retry::{AbortError, ProviderRetryOptions, retry_provider_request};

/// `name` of HTTP errors that carry the raw response text as their body (the TS `body: string`
/// field, as the classifier and Mistral HTTP errors do).
pub const BODY_TEXT_HTTP_ERROR: &str = "BodyTextHttpError";

/// An HTTP failure in the shape `retry_provider_request` understands, with the raw body text
/// as its body: message `"{label} returned {status}"`.
pub fn body_text_http_error(label: &str, status: u16, headers: reqwest::header::HeaderMap, body: String) -> ProviderHttpError {
    ProviderHttpError {
        status: Some(status),
        headers: Some(headers),
        message: format!("{label} returned {status}"),
        body: Some(body),
        error: None,
        name: BODY_TEXT_HTTP_ERROR.to_string(),
    }
}

/// A per-attempt timeout (`TimeoutError`): no status, so it is retried like a connection error.
pub fn timeout_http_error(timeout_ms: u64) -> ProviderHttpError {
    let mut error = ProviderHttpError::connection(format!("Request timed out after {timeout_ms}ms"));
    error.name = "TimeoutError".to_string();
    error
}

/// [`normalize_provider_error`] that also reads the raw body text of [`body_text_http_error`]s
/// (TS: a `body` string field wins over the parsed `error` object).
pub fn normalize_classifier_error(error: &anyhow::Error) -> NormalizedProviderError {
    if let Some(http) = error.downcast_ref::<ProviderHttpError>()
        && http.name == BODY_TEXT_HTTP_ERROR {
            let body = http
                .body
                .as_deref()
                .map(str::trim)
                .filter(|b| !b.is_empty())
                .map(|b| truncate_error_text(b, MAX_PROVIDER_ERROR_BODY_CHARS));
            let message_carries_body = match &body {
                None => true,
                Some(b) => http.message.contains(b.as_str()),
            };
            return NormalizedProviderError { status: http.status, body, message: http.message.clone(), message_carries_body };
        }
    normalize_provider_error(error)
}

/// `typeof value === "object" && value !== null && !Array.isArray(value)`.
pub fn is_record(value: &Value) -> bool {
    value.is_object()
}

/// A finite number from a provider response, or an error naming the field.
pub fn required_number(label: &str, value: Option<&Value>, field: &str) -> anyhow::Result<f64> {
    match value.and_then(Value::as_f64) {
        Some(n) if n.is_finite() => Ok(n),
        _ => anyhow::bail!("{label} returned an invalid {field}"),
    }
}

/// The chat-model view of a classifier model, for `onPayload`/`onResponse` hooks (which receive
/// the request model as JSON in TypeScript).
pub fn classifier_hook_model(model: &ClassifierModel) -> Model {
    serde_json::to_value(model).ok().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_else(|| Model {
        id: model.id.clone(),
        name: model.name.clone(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        base_url: model.base_url.clone(),
        ..Default::default()
    })
}

/// The chat-model view of an image model, for `onPayload`/`onResponse` hooks.
pub fn image_hook_model(model: &ImageModel) -> Model {
    serde_json::to_value(model).ok().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_else(|| Model {
        id: model.id.clone(),
        name: model.name.clone(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        base_url: model.base_url.clone(),
        ..Default::default()
    })
}

fn request_headers(model: &ClassifierModel, api_key: Option<&str>, options: &StreamOptions) -> ProviderHeaders {
    let identity = to_provider_headers(&get_identity_headers(
        ClientIdentityTarget { provider: &model.provider, base_url: &model.base_url },
        options.identity.as_ref(),
    ));
    let mut defaults = ProviderHeaders::new();
    if let Some(key) = api_key {
        defaults.insert("authorization".into(), Some(format!("Bearer {key}")));
    }
    defaults.insert("content-type".into(), Some("application/json".into()));
    let model_headers = model.headers.as_ref().map(to_provider_headers);
    let merged = provider_headers_to_record(&[Some(&identity), Some(&defaults), model_headers.as_ref(), options.headers.as_ref()]);
    merged.map(|m| to_provider_headers(&m)).unwrap_or_default()
}

/// One JSON POST with a fresh per-attempt timeout. Non-2xx responses become
/// [`body_text_http_error`]s.
pub(crate) async fn post_json_attempt(
    client: &dyn HttpTransport,
    label: &str,
    url: &str,
    headers: &ProviderHeaders,
    payload: &Value,
    options: &StreamOptions,
) -> anyhow::Result<(ProviderResponse, Value)> {
    let mut request = HttpRequest::post(url).body(serde_json::to_vec(payload)?).signal(options.signal.clone());
    for (name, value) in headers {
        if let Some(value) = value {
            request = request.header(name.as_str(), value.as_str());
        }
    }
    if let Some(timeout) = options.timeout_ms {
        request = request.timeout(Duration::from_millis(timeout));
    }
    let attempt = async {
        let response = client.send(request).await.map_err(|e| map_send_error(e, options.timeout_ms))?;
        let status = response.status;
        let response_headers = response.headers.clone();
        if !response.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(body_text_http_error(label, status, response_headers, text).into());
        }
        let provider_response = ProviderResponse { status, headers: headers_to_record(&response_headers) };
        let bytes = response.bytes().await.map_err(|e| map_send_error(e, options.timeout_ms))?;
        let json: Value = serde_json::from_slice(&bytes)
            .map_err(|e| anyhow::anyhow!("Unexpected token in JSON response: {e}"))?;
        anyhow::Ok((provider_response, json))
    };
    match &options.signal {
        Some(signal) => {
            if signal.is_cancelled() {
                anyhow::bail!(AbortError);
            }
            tokio::select! {
                result = attempt => result,
                _ = signal.cancelled() => Err(AbortError.into()),
            }
        }
        None => attempt.await,
    }
}

fn map_send_error(error: TransportError, timeout_ms: Option<u64>) -> anyhow::Error {
    if error.is_timeout()
        && let Some(timeout) = timeout_ms {
            return timeout_http_error(timeout).into();
        }
    // A fetch failure (not an HTTP error): not retried, like a `TypeError` from `fetch`.
    anyhow::Error::new(crate::utils::http::FetchFailed { cause: error.network_error() })
}

/// The HTTP transport for a request: the caller's `http_transport`, else a proxy-aware shared
/// client.
pub(crate) fn http_client(url: &str, options: &StreamOptions) -> anyhow::Result<Arc<dyn HttpTransport>> {
    crate::transport::http_transport_for(url, options)
}

/// Post one JSON classifier request with bearer auth, `onPayload`/`onResponse` hooks, a fresh
/// timeout per attempt, and provider retries (default 2). Returns the parsed response body.
/// `no_retry_statuses` lists HTTP statuses that fail at once although they are normally retried.
pub async fn post_classifier_request(
    label: &str,
    url: &str,
    model: &ClassifierModel,
    body: Value,
    options: &StreamOptions,
    no_retry_statuses: &[u16],
) -> anyhow::Result<Value> {
    let Some(api_key) = options.api_key.clone().filter(|k| !k.is_empty()) else {
        anyhow::bail!("No API key for provider: {}", model.provider);
    };
    let mut payload = body;
    if let Some(on_payload) = &options.on_payload
        && let Some(next) = on_payload(payload.clone(), classifier_hook_model(model)).await {
            payload = next;
        }
    let headers = request_headers(model, Some(&api_key), options);
    let client = http_client(url, options)?;
    let retry = ProviderRetryOptions {
        max_retries: Some(options.max_retries.unwrap_or(2)),
        max_retry_delay_ms: options.max_retry_delay_ms,
        signal: options.signal.clone(),
        no_retry_statuses: no_retry_statuses.to_vec(),
    };
    let (response, json) =
        retry_provider_request(|| post_json_attempt(&*client, label, url, &headers, &payload, options), &retry).await?;
    if let Some(on_response) = &options.on_response {
        on_response(response, classifier_hook_model(model)).await;
    }
    Ok(json)
}

/// Headers for a classifier request with optional bearer auth (used by APIs that call several
/// endpoints, such as llama.cpp).
pub(crate) fn classifier_request_headers(model: &ClassifierModel, api_key: Option<&str>, options: &StreamOptions) -> ProviderHeaders {
    request_headers(model, api_key, options)
}

fn token_count(value: Option<&Value>) -> u64 {
    match value.and_then(Value::as_f64) {
        Some(n) if n.is_finite() && n > 0.0 => n as u64,
        _ => 0,
    }
}

/// Usage from a `{ input_tokens, output_tokens }` object, priced from the model catalog. A
/// missing or malformed usage object yields `None` instead of failing.
pub fn parse_classifier_usage(value: Option<&Value>, model: &ClassifierModel) -> Option<Usage> {
    let value = value?.as_object()?;
    let input_raw = value.get("input_tokens");
    let output_raw = value.get("output_tokens");
    if input_raw.is_none() && output_raw.is_none() {
        return None;
    }
    let input = token_count(input_raw);
    let output = token_count(output_raw);
    let mut usage = Usage { input, output, total_tokens: input + output, ..Default::default() };
    calculate_cost(&model.cost, &mut usage);
    Some(usage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_usage_leniently() {
        let model = ClassifierModel { id: "m".into(), ..Default::default() };
        assert!(parse_classifier_usage(None, &model).is_none());
        assert!(parse_classifier_usage(Some(&json!({})), &model).is_none());
        assert!(parse_classifier_usage(Some(&json!([1])), &model).is_none());
        let usage = parse_classifier_usage(Some(&json!({ "input_tokens": 3, "output_tokens": "x" })), &model).unwrap();
        assert_eq!((usage.input, usage.output, usage.total_tokens), (3, 0, 3));
    }

    #[test]
    fn normalizes_body_text_errors() {
        let error = anyhow::Error::new(body_text_http_error("X", 400, Default::default(), " bad input ".into()));
        let norm = normalize_classifier_error(&error);
        assert_eq!(norm.status, Some(400));
        assert_eq!(norm.body.as_deref(), Some("bad input"));
        assert!(!norm.message_carries_body);
        assert_eq!(
            crate::utils::error_body::format_provider_error(&norm, Some("X error")),
            "X error (400): bad input"
        );
    }
}
