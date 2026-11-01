//! Port of `identity.test.ts`: client identity (`User-Agent` and provider attribution headers).

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use common::*;
use indexmap::IndexMap;
use pi_ai::api::openai_completions;
use pi_ai::api::registry::builtin_api;
use pi_ai::auth::*;
use pi_ai::utils::identity::{
    ClientAttribution, ClientIdentityTarget, UserAgentMode, UserAgentNamed, format_user_agent,
    get_attribution_headers, get_identity_headers, merge_client_identity, pi_attribution,
};
use pi_ai::*;
use serde_json::json;

fn acme(categories: Option<&[&str]>) -> ClientAttribution {
    ClientAttribution {
        name: "acme".into(),
        url: "https://acme.example".into(),
        categories: categories.map(|c| c.iter().map(|s| s.to_string()).collect()),
    }
}

fn target<'a>(provider: &'a str, base_url: &'a str) -> ClientIdentityTarget<'a> {
    ClientIdentityTarget { provider, base_url }
}

/// An identity whose `attribution` is set, or explicitly `null` for `None`.
fn attributed(attribution: Option<ClientAttribution>) -> ClientIdentity {
    ClientIdentity {
        attribution: Some(attribution),
        ..Default::default()
    }
}

fn short() -> Option<UserAgentMode> {
    Some(UserAgentMode::Named(UserAgentNamed::Short))
}

fn headers(pairs: &[(&str, &str)]) -> IndexMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// `process.platform` and `process.arch` names, as the detailed `User-Agent` reports them.
fn platform_and_arch() -> (&'static str, &'static str) {
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    };
    (platform, arch)
}

#[test]
fn keeps_the_version_constant_in_sync_with_the_package_version() {
    assert_eq!(PI_AI_VERSION, env!("CARGO_PKG_VERSION"));
}

#[test]
fn formats_user_agents() {
    let (platform, arch) = platform_and_arch();
    // TS reports the JS runtime (`node/<version>`); the port reports `rust/<version>`.
    let detailed = |version: &str| {
        let agent = format_user_agent(Some(&ClientIdentity {
            version: Some(version.into()),
            ..Default::default()
        }));
        let inner = agent
            .strip_prefix(&format!("pi/{version} ("))
            .and_then(|rest| rest.strip_suffix(")"))
            .unwrap_or_else(|| panic!("unexpected detailed User-Agent {agent:?}"));
        let parts: Vec<&str> = inner.split("; ").collect();
        assert_eq!(parts.len(), 3, "{agent:?}");
        assert_eq!(parts[0], platform, "{agent:?}");
        assert!(parts[1].starts_with("rust/"), "{agent:?}");
        assert_eq!(parts[2], arch, "{agent:?}");
    };
    detailed("9.9.9");
    assert!(format_user_agent(None).starts_with(&format!("pi/{PI_AI_VERSION} ({platform}; ")));
    let short_identity = ClientIdentity {
        user_agent: short(),
        version: Some("9.9.9".into()),
        ..Default::default()
    };
    assert_eq!(format_user_agent(Some(&short_identity)), "pi");
    let custom = ClientIdentity {
        user_agent: Some(UserAgentMode::Custom {
            custom: "acme/1.0".into(),
        }),
        ..Default::default()
    };
    assert_eq!(format_user_agent(Some(&custom)), "acme/1.0");
}

#[test]
fn merges_identities_field_by_field() {
    assert_eq!(merge_client_identity(&[None, None]), None);
    let base = ClientIdentity {
        attribution: Some(Some(pi_attribution())),
        user_agent: short(),
        ..Default::default()
    };
    // An absent field does not override; TS `attribution: undefined` is absent here.
    let over = ClientIdentity {
        version: Some("1.0.0".into()),
        ..Default::default()
    };
    assert_eq!(
        merge_client_identity(&[Some(&base), Some(&over)]),
        Some(ClientIdentity {
            attribution: Some(Some(pi_attribution())),
            user_agent: short(),
            version: Some("1.0.0".into())
        })
    );
    let pi_only = attributed(Some(pi_attribution()));
    assert_eq!(
        merge_client_identity(&[Some(&pi_only), Some(&attributed(None))]),
        Some(attributed(None))
    );
}

#[test]
fn sends_no_attribution_unless_enabled() {
    let open_router = target("openrouter", "https://openrouter.ai/api/v1");
    assert!(get_attribution_headers(open_router, None).is_empty());
    assert!(get_attribution_headers(open_router, Some(&attributed(None))).is_empty());
    assert_eq!(
        get_identity_headers(open_router, None),
        headers(&[("User-Agent", &format_user_agent(None))])
    );
}

#[test]
fn attributes_open_router_requests() {
    let open_router = target("openrouter", "https://openrouter.ai/api/v1");
    assert_eq!(
        get_attribution_headers(open_router, Some(&attributed(Some(pi_attribution())))),
        headers(&[
            ("HTTP-Referer", "https://pi.dev"),
            ("X-OpenRouter-Title", "pi"),
            ("X-OpenRouter-Categories", "cli-agent")
        ])
    );
    assert_eq!(
        get_attribution_headers(
            open_router,
            Some(&attributed(Some(acme(Some(&["cli-agent", "cloud-agent"])))))
        ),
        headers(&[
            ("HTTP-Referer", "https://acme.example"),
            ("X-OpenRouter-Title", "acme"),
            ("X-OpenRouter-Categories", "cli-agent,cloud-agent")
        ])
    );
    assert_eq!(
        get_attribution_headers(open_router, Some(&attributed(Some(acme(Some(&[])))))),
        headers(&[
            ("HTTP-Referer", "https://acme.example"),
            ("X-OpenRouter-Title", "acme")
        ])
    );
    // Custom providers routed through OpenRouter, including legacy base URLs that are not valid URLs.
    let legacy = get_attribution_headers(
        target("custom", "not-a-url-openrouter.ai"),
        Some(&attributed(Some(pi_attribution()))),
    );
    assert_eq!(
        legacy.get("X-OpenRouter-Title").map(String::as_str),
        Some("pi")
    );
}

#[test]
fn attributes_nvidia_nim_with_a_capitalized_name_and_open_code_with_the_name() {
    let pi = attributed(Some(pi_attribution()));
    let acme_identity = attributed(Some(acme(None)));
    assert_eq!(
        get_attribution_headers(target("nvidia", "https://example.test/v1"), Some(&pi)),
        headers(&[("X-BILLING-INVOKE-ORIGIN", "Pi")])
    );
    assert_eq!(
        get_attribution_headers(
            target("custom-nim", "https://integrate.api.nvidia.com/v1"),
            Some(&acme_identity)
        ),
        headers(&[("X-BILLING-INVOKE-ORIGIN", "Acme")])
    );
    assert_eq!(
        get_attribution_headers(target("opencode", "https://opencode.ai/zen/v1"), Some(&pi)),
        headers(&[("x-opencode-client", "pi")])
    );
    assert_eq!(
        get_attribution_headers(
            target("custom", "https://opencode.ai/zen/v1"),
            Some(&acme_identity)
        ),
        headers(&[("x-opencode-client", "acme")])
    );
}

#[test]
fn does_not_attribute_nvidia_models_routed_through_gateways_to_nvidia() {
    let pi = attributed(Some(pi_attribution()));
    assert!(
        !get_attribution_headers(
            target("openrouter", "https://openrouter.ai/api/v1"),
            Some(&pi)
        )
        .contains_key("X-BILLING-INVOKE-ORIGIN")
    );
    assert!(
        get_attribution_headers(
            target("vercel-ai-gateway", "https://ai-gateway.vercel.sh/v1"),
            Some(&pi)
        )
        .is_empty()
    );
}

// ---------------------------------------------------------------------------
// client identity in requests
// ---------------------------------------------------------------------------

fn completions_model(provider: &str, base_url: &str, model_headers: &[(&str, &str)]) -> Model {
    Model {
        id: "test-model".into(),
        name: "Test Model".into(),
        api: "openai-completions".into(),
        provider: provider.into(),
        base_url: base_url.into(),
        reasoning: false,
        input: vec![ModelInputModality::Text],
        context_window: 128000,
        max_tokens: 4096,
        headers: (!model_headers.is_empty()).then(|| headers(model_headers)),
        ..Default::default()
    }
}

fn ok_body() -> String {
    openai_sse_body(&[
        json!({ "id": "chatcmpl-identity", "choices": [{ "delta": { "content": "ok" }, "finish_reason": null, "index": 0 }] }),
        json!({
            "id": "chatcmpl-identity",
            "choices": [{ "delta": {}, "finish_reason": "stop", "index": 0 }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1 }
        }),
    ])
}

fn user_context() -> Context {
    Context {
        messages: vec![Message::User(UserMessage::new("hello", 1))],
        ..Default::default()
    }
}

/// Serves one completion and returns the request it received.
async fn send_completion(model: &Model, options: StreamOptions) -> CapturedRequest {
    let server = serve(vec![MockResponse::sse(ok_body())]).await;
    let model = Model {
        base_url: server.base_url.clone(),
        ..model.clone()
    };
    let result = openai_completions::stream(&model, &normalize_context(&user_context()), options)
        .result()
        .await;
    assert_eq!(
        result.stop_reason,
        StopReason::Stop,
        "{:?}",
        result.error_message
    );
    server.requests.lock()[0].clone()
}

#[tokio::test]
async fn applies_the_identity_below_model_and_request_headers() {
    let model = completions_model(
        "openrouter",
        "https://openrouter.ai/api/v1",
        &[("X-OpenRouter-Title", "model-title")],
    );
    let mut request_headers = ProviderHeaders::new();
    request_headers.insert("HTTP-Referer".into(), None);
    let options = StreamOptions {
        api_key: Some("key".into()),
        identity: Some(ClientIdentity {
            attribution: Some(Some(pi_attribution())),
            user_agent: short(),
            ..Default::default()
        }),
        headers: Some(request_headers),
        ..Default::default()
    };
    let request = send_completion(&model, options).await;
    assert_eq!(request.header("user-agent"), Some("pi"));
    assert_eq!(request.header("x-openrouter-title"), Some("model-title"));
    assert!(request.header("http-referer").is_none());
}

#[tokio::test]
async fn keeps_impersonating_model_headers_over_the_identity_user_agent() {
    let model = completions_model(
        "github-copilot",
        "https://api.githubcopilot.com",
        &[("User-Agent", "GitHubCopilotChat/0.35.0")],
    );
    let options = StreamOptions {
        api_key: Some("key".into()),
        identity: Some(ClientIdentity {
            user_agent: Some(UserAgentMode::Custom {
                custom: "acme".into(),
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let request = send_completion(&model, options).await;
    assert_eq!(
        request.header("user-agent"),
        Some("GitHubCopilotChat/0.35.0")
    );
}

struct StaticKey;

#[async_trait]
impl ApiKeyAuth for StaticKey {
    fn name(&self) -> &str {
        "Test"
    }
    async fn resolve(&self, _input: ApiKeyResolveInput) -> anyhow::Result<Option<AuthResult>> {
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: Some("key".into()),
                ..Default::default()
            },
            ..Default::default()
        }))
    }
}

fn completions_provider(id: &str, model: &Model) -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: id.into(),
        auth: ProviderAuth {
            api_key: Some(Arc::new(StaticKey)),
            oauth: None,
        },
        models: vec![AnyModel::Chat(model.clone())],
        api: Some(ProviderApi::Single(builtin_api("openai-completions"))),
        ..Default::default()
    })
    .unwrap()
}

/// Streams through `models` against a fresh mock and returns the request it received.
async fn send_through(models: &Models, model: &Model, options: StreamOptions) -> CapturedRequest {
    let server = serve(vec![MockResponse::sse(ok_body())]).await;
    // The registered model keeps its base URL; the request goes to the mock through it.
    let local = Model {
        base_url: server.base_url.clone(),
        ..model.clone()
    };
    let result = models
        .stream_simple(&local, &user_context(), options)
        .result()
        .await;
    assert_eq!(
        result.stop_reason,
        StopReason::Stop,
        "{:?}",
        result.error_message
    );
    server.requests.lock()[0].clone()
}

#[tokio::test]
async fn merges_request_identity_over_the_models_and_wrapper_identities() {
    let model = completions_model("openrouter", "https://openrouter.ai/api/v1", &[]);
    let named = |name: &str| ClientAttribution {
        name: name.into(),
        url: "https://acme.example".into(),
        categories: None,
    };
    let models = create_models(CreateModelsOptions {
        identity: Some(ClientIdentity {
            attribution: Some(Some(named("models"))),
            user_agent: short(),
            ..Default::default()
        }),
        ..Default::default()
    });
    models.set_provider(completions_provider("openrouter", &model));

    let from_models = send_through(&models, &model, StreamOptions::default()).await;
    assert_eq!(from_models.header("user-agent"), Some("pi"));
    assert_eq!(from_models.header("x-openrouter-title"), Some("models"));

    let wrapped = models.with_client_identity(attributed(Some(named("wrapper"))));
    let from_wrapper = send_through(&wrapped, &model, StreamOptions::default()).await;
    assert_eq!(from_wrapper.header("user-agent"), Some("pi"));
    assert_eq!(from_wrapper.header("x-openrouter-title"), Some("wrapper"));
    assert_eq!(
        wrapped.get_model("openrouter", "test-model"),
        Some(model.clone())
    );
    // A mutable collection stays mutable through the wrapper.
    wrapped.delete_provider("openrouter");
    assert!(models.get_provider("openrouter").is_none());
    models.set_provider(completions_provider("openrouter", &model));

    let from_request = send_through(
        &wrapped,
        &model,
        StreamOptions {
            identity: Some(attributed(None)),
            ..Default::default()
        },
    )
    .await;
    assert!(from_request.header("x-openrouter-title").is_none());
    assert_eq!(from_request.header("user-agent"), Some("pi"));
}

#[tokio::test]
async fn sends_the_open_code_session_header_to_custom_providers_pointing_at_open_code() {
    let send = |configured: Option<&'static str>| async move {
        let server = serve(vec![MockResponse::sse(ok_body())]).await;
        // Resolve opencode.ai to the mock, like the TS test's `fetch` stub, so the model keeps
        // an OpenCode base URL.
        let address: std::net::SocketAddr = server
            .base_url
            .trim_start_matches("http://")
            .parse()
            .unwrap();
        let client = reqwest::Client::builder()
            .resolve("opencode.ai", address)
            .build()
            .unwrap();
        let model = completions_model(
            "custom-opencode",
            &format!("http://opencode.ai:{}/zen/v1", address.port()),
            &[],
        );
        let models = create_models(CreateModelsOptions::default());
        models.set_provider(completions_provider("custom-opencode", &model));
        let request_headers = configured.map(|value| {
            let mut headers = ProviderHeaders::new();
            headers.insert("X-OpenCode-Session".into(), Some(value.into()));
            headers
        });
        let options = StreamOptions {
            http_client: Some(client),
            session_id: Some("session-1".into()),
            headers: request_headers,
            ..Default::default()
        };
        let result = models
            .stream_simple(&model, &user_context(), options)
            .result()
            .await;
        assert_eq!(
            result.stop_reason,
            StopReason::Stop,
            "{:?}",
            result.error_message
        );
        server.requests.lock()[0].clone()
    };

    assert_eq!(
        send(None).await.header("x-opencode-session"),
        Some("session-1")
    );
    assert_eq!(
        send(Some("configured")).await.header("x-opencode-session"),
        Some("configured")
    );
}
