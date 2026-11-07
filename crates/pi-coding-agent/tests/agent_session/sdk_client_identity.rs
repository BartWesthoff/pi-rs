//! Port of `test/sdk-client-identity.test.ts`.

use std::sync::Arc;

use indexmap::IndexMap;
use parking_lot::Mutex;
use pi_ai::auth::types::{ApiKeyCredential, Credential};
use pi_ai::types::{AssistantMessage, Context, Model, SimpleStreamOptions};
use pi_ai::utils::identity::{ClientIdentityTarget, get_attribution_headers, pi_attribution};
use pi_coding_agent::config::VERSION;
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::extensions::types::ProviderConfig;
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
use pi_coding_agent::core::session_manager::{NewSessionOptions, SessionManager};
use pi_coding_agent::core::settings_manager::SettingsManager;
use serde_json::{Value, json};

/// `PI_TELEMETRY` is process-global; these tests run one at a time.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn create_model(provider: &str, base_url: &str) -> Model {
    serde_json::from_value(json!({
        "id": format!("{provider}-test-model"),
        "name": format!("{provider} Test Model"),
        "api": "openai-completions",
        "provider": provider,
        "baseUrl": base_url,
        "reasoning": false,
        "input": ["text"],
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
        "contextWindow": 128000,
        "maxTokens": 4096
    }))
    .unwrap()
}

fn done_stream() -> pi_ai::AssistantMessageEventStream {
    let message: AssistantMessage = serde_json::from_value(json!({
        "role": "assistant", "content": [{"type": "text", "text": "ok"}], "api": "openai-completions",
        "provider": "capture-provider", "model": "capture-model",
        "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
            "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}},
        "stopReason": "stop", "timestamp": 1
    }))
    .unwrap();
    let (sender, stream) = pi_ai::AssistantMessageEventStream::new();
    sender.end(Some(message));
    stream
}

#[derive(Default)]
struct CaptureOptions {
    telemetry_enabled: Option<bool>,
    provider_headers: Option<IndexMap<String, String>>,
    session_id: Option<String>,
}

async fn capture_options(model: &Model, options: CaptureOptions) -> SimpleStreamOptions {
    let temp_dir = std::env::temp_dir().join(format!("pi-sdk-identity-test-{}", uuid::Uuid::new_v4()));
    let cwd = temp_dir.join("project").to_string_lossy().into_owned();
    let agent_dir = temp_dir.join("agent").to_string_lossy().into_owned();
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&agent_dir).unwrap();

    let settings_manager = SettingsManager::create(&cwd, Some(&agent_dir), None);
    if let Some(enabled) = options.telemetry_enabled {
        settings_manager.set_enable_install_telemetry(enabled);
    }
    let mut credentials: IndexMap<String, Credential> = IndexMap::new();
    credentials.insert(
        model.provider.clone(),
        Credential::ApiKey(ApiKeyCredential {
            key: Some("test-api-key".into()),
            env: None,
        }),
    );
    let runtime = ModelRuntime::create(Some(&CreateModelRuntimeOptions {
        credentials: Some(AuthStorage::in_memory(Some(&credentials))),
        models_path: None,
        allow_model_network: Some(false),
        ..Default::default()
    }))
    .await
    .unwrap();
    let captured = Arc::new(Mutex::new(None::<SimpleStreamOptions>));
    let c = captured.clone();
    runtime
        .register_provider(
            &model.provider,
            &ProviderConfig {
                api: Some(model.api.clone()),
                headers: options.provider_headers.clone(),
                stream_simple: Some(Arc::new(move |_model, _context, options| {
                    *c.lock() = options;
                    done_stream()
                })),
                ..Default::default()
            },
        )
        .unwrap();
    let session_manager = SessionManager::in_memory(Some(&cwd), None, None).unwrap();
    if let Some(id) = &options.session_id {
        session_manager
            .new_session(Some(&NewSessionOptions {
                id: Some(id.clone()),
                parent_session: None,
            }))
            .unwrap();
    }
    let session = create_agent_session(Some(&CreateAgentSessionOptions {
        cwd: Some(cwd),
        agent_dir: Some(agent_dir),
        model: Some(model.clone()),
        model_runtime: Some(runtime.clone()),
        settings_manager: Some(settings_manager),
        session_manager: Some(session_manager),
        ..Default::default()
    }))
    .await
    .unwrap()
    .session;

    let stream_fn = session.agent().stream_fn().unwrap();
    let context = pi_ai::normalize_context(&Context {
        system_prompt: None,
        messages: Vec::new(),
        tools: None,
    });
    let stream = stream_fn(
        model.clone(),
        context,
        SimpleStreamOptions {
            session_id: Some(session.session_id()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let _ = stream.result().await;
    session.dispose();
    runtime.unregister_provider(&model.provider);
    let _ = std::fs::remove_dir_all(&temp_dir);
    
    captured.lock().clone().expect("provider was called")
}

fn identity_json(options: &SimpleStreamOptions) -> Value {
    serde_json::to_value(&options.identity).unwrap()
}

fn attribution_headers(model: &Model, options: &SimpleStreamOptions) -> Value {
    serde_json::to_value(get_attribution_headers(
        ClientIdentityTarget {
            provider: &model.provider,
            base_url: &model.base_url,
        },
        options.identity.as_ref(),
    ))
    .unwrap()
}

/// Runs `body` with `PI_TELEMETRY` set to `value` (or unset), restoring it afterwards.
async fn with_telemetry_env<F: std::future::Future<Output = ()>>(value: Option<&str>, body: F) {
    let _guard = ENV_LOCK.lock().await;
    let original = std::env::var("PI_TELEMETRY").ok();
    // SAFETY: guarded by ENV_LOCK within this test binary.
    unsafe {
        match value {
            Some(v) => std::env::set_var("PI_TELEMETRY", v),
            None => std::env::remove_var("PI_TELEMETRY"),
        }
    }
    body.await;
    unsafe {
        match original {
            Some(v) => std::env::set_var("PI_TELEMETRY", v),
            None => std::env::remove_var("PI_TELEMETRY"),
        }
    }
}

// pi-rs divergence from TS Pi: install telemetry defaults to off, so attribution now needs the
// setting enabled instead of being on by default.
#[tokio::test]
async fn identifies_pi_with_a_detailed_user_agent_and_attribution_as_a_cli_agent() {
    with_telemetry_env(None, async {
        let model = create_model("openrouter", "https://openrouter.ai/api/v1");
        let options = capture_options(
            &model,
            CaptureOptions {
                telemetry_enabled: Some(true),
                ..Default::default()
            },
        )
        .await;

        assert_eq!(
            identity_json(&options),
            json!({"userAgent": "detailed", "version": VERSION, "attribution": serde_json::to_value(pi_attribution()).unwrap()})
        );
        assert_eq!(
            attribution_headers(&model, &options),
            json!({
                "HTTP-Referer": "https://pi.dev",
                "X-OpenRouter-Title": "pi",
                "X-OpenRouter-Categories": "cli-agent",
            })
        );
    })
    .await;
}

// pi-rs divergence from TS Pi: the default setting is off, so a default session sends no attribution.
#[tokio::test]
async fn default_settings_send_no_attribution() {
    with_telemetry_env(None, async {
        let model = create_model("openrouter", "https://openrouter.ai/api/v1");
        let options = capture_options(&model, CaptureOptions::default()).await;
        assert_eq!(identity_json(&options)["attribution"], Value::Null);
        assert_eq!(attribution_headers(&model, &options), json!({}));
    })
    .await;
}

#[tokio::test]
async fn turns_attribution_off_when_telemetry_is_disabled() {
    with_telemetry_env(None, async {
        let model = create_model("nvidia", "https://integrate.api.nvidia.com/v1");
        let options = capture_options(
            &model,
            CaptureOptions {
                telemetry_enabled: Some(false),
                ..Default::default()
            },
        )
        .await;

        assert_eq!(
            identity_json(&options),
            json!({"userAgent": "detailed", "version": VERSION, "attribution": null})
        );
        assert_eq!(attribution_headers(&model, &options), json!({}));
    })
    .await;
}

#[tokio::test]
async fn turns_attribution_off_with_pi_telemetry_0() {
    with_telemetry_env(Some("0"), async {
        let options =
            capture_options(&create_model("openrouter", "https://openrouter.ai/api/v1"), CaptureOptions::default()).await;
        assert_eq!(identity_json(&options)["attribution"], Value::Null);
        assert!(matches!(options.identity.as_ref().unwrap().attribution, Some(None)));
    })
    .await;
}

#[tokio::test]
async fn adds_opencode_session_headers_for_custom_opencode_providers() {
    with_telemetry_env(None, async {
        let options = capture_options(
            &create_model("custom-opencode", "https://opencode.ai/zen/v1"),
            CaptureOptions {
                session_id: Some("opencode-session".into()),
                ..Default::default()
            },
        )
        .await;
        let headers = serde_json::to_value(options.headers.unwrap_or_default()).unwrap();
        assert_eq!(headers["x-opencode-session"], json!("opencode-session"));
    })
    .await;
}

#[tokio::test]
async fn lets_configured_opencode_headers_override_the_session_header() {
    with_telemetry_env(None, async {
        let mut provider_headers = IndexMap::new();
        provider_headers.insert("x-opencode-session".to_string(), "configured-session".to_string());
        let options = capture_options(
            &create_model("opencode", "https://opencode.ai/zen/v1"),
            CaptureOptions {
                session_id: Some("opencode-session".into()),
                provider_headers: Some(provider_headers),
                ..Default::default()
            },
        )
        .await;
        let headers = serde_json::to_value(options.headers.unwrap_or_default()).unwrap();
        assert_eq!(headers["x-opencode-session"], json!("configured-session"));
    })
    .await;
}
