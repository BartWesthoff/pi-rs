//! Provider error handling: `error-body.test.ts`, `provider-error-body-passthrough.test.ts`,
//! `provider-error-body-regression.test.ts`, `pre-generation-error.test.ts`,
//! `github-copilot-request-size-overflow.test.ts` and `github-copilot-anthropic.test.ts`.

use pi_ai::api::{anthropic_messages, azure_openai_responses, bedrock_converse_stream, google_generative_ai, mistral_conversations, openai_codex_responses, openai_completions, openai_responses};
use pi_ai::providers::all::get_builtin_model;
use pi_ai::utils::error_body::*;
use pi_ai::*;
use serde_json::{Value, json};

use crate::mock_http::{MockServer, Response};
use crate::support::*;

fn http_error(status: u16, message: &str, error: Option<Value>) -> anyhow::Error {
    anyhow::Error::new(ProviderHttpError { status: Some(status), headers: None, message: message.into(), body: None, error, name: "APIError".into() })
}

// ---------------------------------------------------------------------------
// Port of error-body.test.ts
// ---------------------------------------------------------------------------

#[test]
fn normalize_and_format_provider_errors() {
    // Opaque SDK message with the parsed body on `error`.
    let norm = normalize_provider_error(&http_error(403, "403 status code (no body)", Some(json!({"error": "blocked by gateway WAF"}))));
    assert_eq!(norm.status, Some(403));
    assert_eq!(norm.body.as_deref(), Some(r#"{"error":"blocked by gateway WAF"}"#));
    assert!(!norm.message_carries_body);
    let formatted = format_provider_error(&norm, None);
    assert!(formatted.contains("403") && formatted.contains("blocked by gateway WAF") && formatted != "403 status code (no body)");
    assert_eq!(format_provider_error(&norm, Some("OpenAI API error")), r#"OpenAI API error (403): {"error":"blocked by gateway WAF"}"#);

    // A message that already folds the body in is kept.
    let body = json!({"error": {"code": 403, "message": "Permission denied"}}).to_string();
    let norm = normalize_provider_error(&http_error(403, &body, None));
    assert_eq!(norm.status, Some(403));
    assert!(norm.message_carries_body);
    assert_eq!(norm.message, body);
    assert_eq!(format_provider_error(&norm, Some("OpenAI API error")), format!("OpenAI API error (403): {body}"));

    // Plain parsed JSON bodies surface; empty ones do not.
    let norm = normalize_provider_error(&http_error(400, "400 status code (no body)", Some(json!({"message": "schema validation failed", "field": "tools[0]"}))));
    assert_eq!(norm.body.as_deref(), Some(r#"{"message":"schema validation failed","field":"tools[0]"}"#));
    assert!(!norm.message_carries_body);
    let norm = normalize_provider_error(&http_error(403, "403 status code (no body)", Some(json!({}))));
    assert_eq!(norm.body, None);
    assert!(norm.message_carries_body);

    // Bodies are truncated at the cap.
    let long = "x".repeat(MAX_PROVIDER_ERROR_BODY_CHARS + 50);
    let norm = normalize_provider_error(&http_error(500, "failed", Some(json!({"detail": long}))));
    let body = norm.body.unwrap();
    assert!(body.contains("... [truncated"), "{}", &body[body.len() - 40..]);
    assert!(body.len() < long.len() + 20);

    // A message containing the body.
    let norm = normalize_provider_error(&http_error(500, r#"500: {"detail":"upstream exploded"}"#, Some(json!({"detail": "upstream exploded"}))));
    assert!(norm.message_carries_body);

    // Non-HTTP errors keep their message.
    let norm = normalize_provider_error(&anyhow::anyhow!(r#"{{"reason":"boom"}}"#));
    assert_eq!((norm.status, norm.body.clone()), (None, None));
    assert_eq!(norm.message, r#"{"reason":"boom"}"#);
    assert_eq!(format_provider_error(&norm, None), r#"{"reason":"boom"}"#);
}

// ---------------------------------------------------------------------------
// Port of provider-error-body-passthrough.test.ts and provider-error-body-regression.test.ts
// ---------------------------------------------------------------------------

fn waf_server(body: Value) -> impl Fn(&crate::mock_http::Request, usize) -> Response + Send + Sync + 'static {
    move |_, _| Response::json(403, json!({"error": body}))
}

fn hi() -> TranscriptContext {
    normalize_context(&ctx(json!({"systemPrompt": "", "messages": [{"role": "user", "content": [{"type": "text", "text": "hi"}]}], "tools": []})))
}

fn key() -> StreamOptions {
    StreamOptions { api_key: Some("test".into()), max_retries: Some(0), ..Default::default() }
}

#[tokio::test]
async fn openrouter_images_surface_the_http_body() {
    let server = MockServer::start(waf_server(json!("blocked by gateway WAF"))).await;
    let mut model = pi_ai::providers::all::get_builtin_image_model("openrouter", "black-forest-labs/flux.2-pro").unwrap();
    model.base_url = server.url.clone();
    let output = pi_ai::images::generate_images(&model, &ImagesContext { input: vec![UserContentBlock::text("Generate a dog")] }, key()).await.unwrap();
    assert_eq!(output.stop_reason, ImagesStopReason::Error);
    let message = output.error_message.unwrap();
    assert!(message.contains("403") && message.contains("blocked by gateway WAF") && message != "403 status code (no body)", "{message}");
}

#[tokio::test]
async fn completions_and_responses_surface_the_http_body() {
    let server = MockServer::start(waf_server(json!("blocked by gateway WAF"))).await;
    let completions = model(json!({"id": "test-model", "name": "Test Model", "api": "openai-completions", "provider": "openrouter", "baseUrl": server.url,
        "reasoning": false, "input": ["text"], "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0}, "contextWindow": 1000, "maxTokens": 100}));
    let output = openai_completions::stream(&completions, &hi(), key()).result().await;
    assert_eq!(output.stop_reason, StopReason::Error);
    let message = output.error_message.unwrap();
    assert!(message.contains("403") && message.contains("blocked by gateway WAF") && message != "403 status code (no body)", "{message}");

    // OpenRouter's metadata.raw is part of the surfaced body and is not appended twice.
    let server = MockServer::start(waf_server(json!({"message": "Provider returned error", "code": 403, "metadata": {"raw": "upstream WAF blocked policy XYZ"}}))).await;
    let mut completions = completions.clone();
    completions.base_url = server.url.clone();
    let message = openai_completions::stream(&completions, &hi(), key()).result().await.error_message.unwrap();
    assert_eq!(message.matches("upstream WAF blocked policy XYZ").count(), 1, "{message}");

    let server = MockServer::start(waf_server(json!("blocked by gateway WAF"))).await;
    let responses = model(json!({"id": "gpt-test", "name": "GPT Test", "api": "openai-responses", "provider": "openai", "baseUrl": server.url,
        "reasoning": false, "input": ["text"], "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0}, "contextWindow": 1000, "maxTokens": 100}));
    let message = openai_responses::stream(&responses, &hi(), key()).result().await.error_message.unwrap();
    assert!(message.contains("OpenAI API error (403)") && message.contains("blocked by gateway WAF"), "{message}");
}

#[tokio::test]
async fn bedrock_surfaces_gateway_bodies_and_validation_messages() {
    let options = || StreamOptions {
        cache_retention: Some(CacheRetention::None),
        env: Some([("AWS_BEDROCK_SKIP_AUTH".to_string(), "1".to_string()), ("AWS_REGION".to_string(), "us-east-1".to_string()), ("AWS_MAX_ATTEMPTS".to_string(), "1".to_string())].into_iter().collect()),
        ..Default::default()
    };
    let server = MockServer::start(|_, _| Response::json(403, json!({"message": "blocked by gateway WAF"}))).await;
    let mut model = get_builtin_model("amazon-bedrock", "us.anthropic.claude-opus-4-8").unwrap();
    model.base_url = server.url.clone();
    let message = bedrock_converse_stream::stream_simple(&model, &hi(), options()).result().await.error_message.unwrap();
    assert!(message.contains("403") && message.contains("blocked by gateway WAF") && !message.contains("Unknown: UnknownError"), "{message}");

    let text = "Invocation of model ID anthropic.claude-opus-5 with on-demand throughput isn't supported. Retry with an inference profile.";
    let server = MockServer::start(move |_, _| Response::json(400, json!({"message": text})).with_header("x-amzn-errortype", "ValidationException")).await;
    let mut model = get_builtin_model("amazon-bedrock", "global.anthropic.claude-opus-5").unwrap();
    model.base_url = server.url.clone();
    let output = bedrock_converse_stream::stream_simple(&model, &hi(), options()).result().await;
    assert_eq!(output.stop_reason, StopReason::Error);
    let message = output.error_message.unwrap();
    assert!(message.contains("on-demand throughput isn't supported") && message.contains("inference profile"), "{message}");
}

// ---------------------------------------------------------------------------
// Port of pre-generation-error.test.ts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn missing_auth_fails_before_generation() {
    // TS throws synchronously from streamSimple; the Rust entry points return a stream that
    // ends with the same error before any request.
    let test_model = |api: &str| {
        model(json!({"id": "test-model", "name": "Test", "api": api, "provider": "test-provider", "baseUrl": "https://example.invalid", "reasoning": false,
            "input": ["text"], "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0}, "contextWindow": 1000, "maxTokens": 100}))
    };
    let context = normalize_context(&Context::default());
    let results = vec![
        anthropic_messages::stream_simple(&test_model("anthropic-messages"), &context, StreamOptions::default()).result().await,
        azure_openai_responses::stream_simple(&test_model("azure-openai-responses"), &context, StreamOptions::default()).result().await,
        google_generative_ai::stream_simple(&test_model("google-generative-ai"), &context, StreamOptions::default()).result().await,
        mistral_conversations::stream_simple(&test_model("mistral-conversations"), &context, StreamOptions::default()).result().await,
        openai_codex_responses::stream_simple(&test_model("openai-codex-responses"), &context, StreamOptions::default()).result().await,
        openai_completions::stream_simple(&test_model("openai-completions"), &context, StreamOptions::default()).result().await,
        openai_responses::stream_simple(&test_model("openai-responses"), &context, StreamOptions::default()).result().await,
    ];
    for result in results {
        assert_eq!(result.stop_reason, StopReason::Error, "{}", result.api);
        assert_eq!(result.error_message.as_deref(), Some("No API key for provider: test-provider"), "{}", result.api);
    }
}

// ---------------------------------------------------------------------------
// Port of github-copilot-request-size-overflow.test.ts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn copilot_request_size_errors_classify_as_overflow() {
    for (status, nested, overflow) in [(413u16, true, true), (413, false, true), (400, true, false)] {
        let error = json!({"message": "failed to parse request", "code": ""});
        let body = if nested { json!({"error": error}) } else { error };
        let server = MockServer::start(move |_, _| Response::json(status, body.clone())).await;
        let copilot = model(json!({"id": "copilot-test-model", "name": "Copilot test model", "api": "openai-responses", "provider": "github-copilot",
            "baseUrl": server.url, "reasoning": true, "input": ["text", "image"], "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
            "contextWindow": 1000000, "maxTokens": 128000}));
        let result = openai_responses::stream(&copilot, &normalize_context(&user_ctx("Continue")), StreamOptions { api_key: Some("test-key".into()), max_retries: Some(0), ..Default::default() })
            .result()
            .await;
        assert_eq!(server.requests().len(), 1);
        assert_eq!(result.provider, "github-copilot");
        assert_eq!(result.stop_reason, StopReason::Error);
        assert_eq!(result.usage.total_tokens, 0);
        let message = result.error_message.clone().unwrap();
        assert!(message.contains(&format!("({status})")), "{message}");
        // The openai SDK (7.19) reads the reason from the body's `error` field only; a bare error
        // object yields "<status> status code (no body)" (the TS expectation of the reason text
        // for the non-nested body does not match the SDK it runs against).
        if nested {
            assert!(message.contains("failed to parse request"), "{message}");
        } else {
            assert!(message.ends_with(&format!("{status} status code (no body)")), "{message}");
        }
        assert_eq!(is_context_overflow(&result, Some(copilot.context_window)), overflow, "{status} nested={nested}: {message}");
    }
}

// ---------------------------------------------------------------------------
// Port of github-copilot-anthropic.test.ts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn copilot_claude_through_anthropic_messages() {
    let level = |m: &Model| get_supported_thinking_levels(m).into_iter().map(|l| serde_json::to_value(l).unwrap().as_str().unwrap().to_string()).collect::<Vec<_>>();
    let opus5 = get_builtin_model("github-copilot", "claude-opus-5").unwrap();
    assert_eq!(opus5.api, "anthropic-messages");
    assert_eq!(opus5.context_window, 1000000);
    let map = serde_json::to_value(&opus5.thinking_level_map).unwrap();
    assert_eq!((map["minimal"].clone(), map["xhigh"].clone(), map["max"].clone()), (json!("low"), json!("xhigh"), json!("max")));
    assert!(level(&opus5).contains(&"xhigh".to_string()) && level(&opus5).contains(&"max".to_string()));
    let opus55 = get_builtin_model("github-copilot", "claude-opus-5.5").unwrap();
    assert_eq!(opus55.api, "anthropic-messages");
    assert_eq!(opus55.context_window, 1000000);
    assert_eq!(level(&opus55), ["low", "medium", "high", "xhigh", "max"]);
    let sonnet46 = get_builtin_model("github-copilot", "claude-sonnet-4.6").unwrap();
    let map = serde_json::to_value(&sonnet46.thinking_level_map).unwrap();
    assert_eq!((map["minimal"].clone(), map["max"].clone()), (json!("low"), json!("max")));
    assert!(level(&sonnet46).contains(&"max".to_string()) && !level(&sonnet46).contains(&"xhigh".to_string()));

    let server = MockServer::start(|_, _| Response::sse(&anthropic_ok_sse())).await;
    let mut model = sonnet46.clone();
    model.base_url = server.url.clone();
    let context = normalize_context(&ctx(json!({"systemPrompt": "You are a helpful assistant.", "messages": [{"role": "user", "content": "Hello"}]})));
    anthropic_messages::stream(&model, &context, StreamOptions { api_key: Some("tid_copilot_session_test_token".into()), ..Default::default() }).result().await;
    let request = server.request(0);
    assert_eq!(request.header("authorization"), Some("Bearer tid_copilot_session_test_token"));
    assert_eq!(request.header("x-api-key"), None);
    assert!(request.header("user-agent").unwrap().contains("GitHubCopilotChat"), "{:?}", request.header("user-agent"));
    assert_eq!(request.header("copilot-integration-id"), Some("vscode-chat"));
    assert_eq!(request.header("x-initiator"), Some("user"));
    assert_eq!(request.header("openai-intent"), Some("conversation-edits"));
    let betas = request.header("anthropic-beta").unwrap_or_default().to_string();
    assert!(!betas.contains("fine-grained-tool-streaming-2025-05-14"));
    let body = request.json();
    assert_eq!(body["model"], "claude-sonnet-4.6");
    assert_eq!(body["stream"], true);
    assert_eq!(body["max_tokens"], model.max_tokens);
    assert!(body["messages"].is_array());

    // Adaptive-thinking models omit the interleaved-thinking beta even when requested.
    anthropic_messages::stream(&model, &context, StreamOptions { api_key: Some("tid_copilot_session_test_token".into()), ..Default::default() }.with_extra("interleavedThinking", true))
        .result()
        .await;
    assert!(!server.request(1).header("anthropic-beta").unwrap_or_default().contains("interleaved-thinking-2025-05-14"));
}

// ---------------------------------------------------------------------------
// Port of overflow.test.ts
// ---------------------------------------------------------------------------

fn overflow_error(error_message: &str, provider: &str) -> AssistantMessage {
    AssistantMessage {
        api: "openai-completions".into(),
        provider: provider.into(),
        model: "qwen3.5:35b".into(),
        stop_reason: StopReason::Error,
        error_message: Some(error_message.into()),
        timestamp: 1,
        ..Default::default()
    }
}

fn length_stop(input: u64, cache_read: u64, cache_write: u64, output: u64, provider: &str) -> AssistantMessage {
    let mut message = AssistantMessage { provider: provider.into(), stop_reason: StopReason::Length, timestamp: 1, ..Default::default() };
    message.usage.input = input;
    message.usage.cache_read = cache_read;
    message.usage.cache_write = cache_write;
    message.usage.output = output;
    message.usage.total_tokens = input + cache_read + cache_write + output;
    message
}

#[test]
fn context_overflow_detection() {
    use pi_ai::utils::overflow::{is_context_overflow, is_recoverable_length};
    // The 413 pattern is provider-agnostic and holds even with zero usage and a large window.
    for message in [
        r#"github-copilot API error (413): {"message":"failed to parse request","code":""}"#,
        "custom-provider API error (413): Payload Too Large",
        r#"413 {"error":{"message":"failed to parse request","code":""}}"#,
        "413",
    ] {
        assert!(is_context_overflow(&overflow_error(message, "ollama"), Some(1_000_000)), "{message}");
        assert!(is_context_overflow(&overflow_error(message, "ollama"), None), "{message}");
    }
    for message in [
        r#"github-copilot API error (400): {"message":"failed to parse request","code":""}"#,
        "custom-provider API error (429): Too many requests",
        r#"github-copilot API error (400): {"message":"failed to parse request: invalid value 413"}"#,
        "custom-provider API error (400): upstream mentioned OpenAI API error (413): invalid input",
        "custom-provider API error (4130): invalid input",
        "4130 failed to parse request",
    ] {
        assert!(!is_context_overflow(&overflow_error(message, "ollama"), None), "{message}");
    }
    // HTTP-status matching is limited to failed responses.
    for stop in [StopReason::Aborted, StopReason::Stop] {
        let mut message = overflow_error(r#"OpenAI API error (413): {"message":"failed to parse request"}"#, "ollama");
        message.stop_reason = stop;
        assert!(!is_context_overflow(&message, None));
    }
    for (message, provider, window) in [
        ("400 `prompt too long; exceeded max context length by 100918 tokens`", "ollama", 32768),
        (r#"400 {"code":"1261","message":"Prompt too long"}"#, "zai", 1048576),
        (r#"400 {"code":"1261","message":"Prompt exceeds max length"}"#, "zai", 1048576),
        ("400 The input (516368 tokens) is longer than the model's context length (262144 tokens).", "ollama", 262144),
        (
            "Error: 503 litellm.ServiceUnavailableError: litellm.MidStreamFallbackError: litellm.APIConnectionError: APIConnectionError: OpenAIException - Requested token count exceeds the model's maximum context length of 131072 tokens.",
            "ollama",
            131072,
        ),
        // LiteLLM pre-call ContextWindowExceededError (#11494).
        (
            "400: {\"message\":\"litellm.ContextWindowExceededError: litellm.BadRequestError: litellm._pre_call_checks: Context Window exceeded for given call. No models have context window large enough for this call.\nModel=Qwen3.8-Flash-Next, Max Input Tokens=262144, Got=265262\nmodel=Qwen3.8-Flash-Next. context_window_fallbacks=None. fallbacks=[].\"}",
            "ollama",
            262144,
        ),
        ("Error: 400 Input length (265330) exceeds model's maximum context length (262144).", "ollama", 262144),
        ("Provider returned error: Input length 131393 exceeds the maximum allowed input length of 131040 tokens.", "ollama", 131072),
        ("400 Prompt has 256468 tokens, but the configured context size is 256000 tokens", "ollama", 256000),
        ("Prompt has 5,958,968 tokens, but the configured context size is 256,000 tokens", "ollama", 256000),
        ("400 status code (no body)", "cerebras", 131072),
    ] {
        assert!(is_context_overflow(&overflow_error(message, provider), Some(window)), "{message}");
    }
    for (message, provider, window) in [
        ("500 `model runner crashed unexpectedly`", "ollama", 32768),
        // A bodyless 400 is only known to indicate overflow for Cerebras (#9482).
        ("400 status code (no body)", "opencode-go", 1_000_000),
        ("Throttling error: Too many tokens, please wait before trying again.", "ollama", 200000),
        ("Service unavailable: The service is temporarily unavailable.", "ollama", 200000),
        ("Rate limit exceeded, please retry after 30 seconds.", "ollama", 200000),
        ("Too many requests. Please slow down.", "ollama", 200000),
    ] {
        assert!(!is_context_overflow(&overflow_error(message, provider), Some(window)), "{message}");
    }

    // Xiaomi-style overflow: length stop with zero output and a filled context.
    assert!(is_context_overflow(&length_stop(58, 1048512, 0, 0, "xiaomi"), Some(1048576)));
    assert!(is_recoverable_length(&length_stop(3, 253584, 25554, 16, "openai"), 128000));
    assert!(!is_recoverable_length(&length_stop(4062, 0, 0, 1024, "test-provider"), 1024));
    assert!(is_recoverable_length(&length_stop(100, 0, 0, 0, "test-provider"), 128000));
    assert!(!is_context_overflow(&length_stop(1000, 0, 0, 4096, "test-provider"), Some(200000)));
    assert!(!is_context_overflow(&length_stop(100, 0, 0, 0, "test-provider"), Some(200000)));
}
