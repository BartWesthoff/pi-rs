//! Cloudflare endpoint constants (port of `api/cloudflare.ts`) and the AI Gateway binding
//! transport notes (port of `api/cloudflare-ai-binding.ts`).
//!
//! The TypeScript `createAiBindingFetch(env.AI)` adapts a Cloudflare Worker's AI binding into a
//! `fetch`. Workers bindings exist only inside the Workers runtime, so the Rust port keeps the
//! shared auth sentinel and the binding route helper; a caller that has a binding-backed HTTP
//! transport passes it as `StreamOptions.http_transport`.

/// Workers AI direct endpoint.
pub const CLOUDFLARE_WORKERS_AI_BASE_URL: &str = "https://api.cloudflare.com/client/v4/accounts/{CLOUDFLARE_ACCOUNT_ID}/ai/v1";

/// Workers AI REST endpoint root; model runs are posted to `run`.
pub const CLOUDFLARE_WORKERS_AI_REST_BASE_URL: &str = "https://api.cloudflare.com/client/v4/accounts/{CLOUDFLARE_ACCOUNT_ID}/ai";

/// AI Gateway Unified API. <https://developers.cloudflare.com/ai-gateway/usage/unified-api/>
pub const CLOUDFLARE_AI_GATEWAY_COMPAT_BASE_URL: &str =
    "https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/compat";

/// AI Gateway → OpenAI passthrough. Used until /compat supports /v1/responses.
pub const CLOUDFLARE_AI_GATEWAY_OPENAI_BASE_URL: &str =
    "https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/openai";

/// AI Gateway → Anthropic passthrough.
pub const CLOUDFLARE_AI_GATEWAY_ANTHROPIC_BASE_URL: &str =
    "https://gateway.ai.cloudflare.com/v1/{CLOUDFLARE_ACCOUNT_ID}/{CLOUDFLARE_GATEWAY_ID}/anthropic";

/// Placeholder value for auth headers on binding-routed requests. API implementations require
/// an API key or a recognized auth header (`authorization`, `x-api-key`, `cf-aig-authorization`)
/// before dispatch; binding calls are pre-authenticated, so pass
/// `cf-aig-authorization: Bearer {CLOUDFLARE_GATEWAY_BINDING_AUTH_SENTINEL}` together with
/// `Authorization: null` / `x-api-key: null`.
pub const CLOUDFLARE_GATEWAY_BINDING_AUTH_SENTINEL: &str = "cloudflare-gateway-binding";

/// Base URL of the AI Gateway provider passthrough served by the Workers AI binding:
/// `https://workers-binding.ai/ai-gateway/gateways/{gateway}/{provider}`.
pub fn ai_binding_gateway_base_url(gateway: &str, provider: &str) -> String {
    format!("https://workers-binding.ai/ai-gateway/gateways/{gateway}/{provider}")
}

/// Headers that satisfy the API implementations' auth check on binding-routed requests and keep
/// SDK-style placeholder auth headers away from the gateway.
pub fn ai_binding_auth_headers() -> crate::types::ProviderHeaders {
    let mut headers = crate::types::ProviderHeaders::new();
    headers.insert("cf-aig-authorization".into(), Some(format!("Bearer {CLOUDFLARE_GATEWAY_BINDING_AUTH_SENTINEL}")));
    headers.insert("Authorization".into(), None);
    headers.insert("x-api-key".into(), None);
    headers
}
