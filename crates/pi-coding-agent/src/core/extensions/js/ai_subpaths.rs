//! The deep `@earendil-works/pi-ai/<subpath>` entry points of pi-ai's `package.json` `exports`
//! (`./providers/*`, `./api/*`, `./utils/*`, `./bedrock-provider`, `./bun-oauth`).
//!
//! Every subpath is a small generated ES module over the `pi-rs:ai-subpaths` glue
//! (`js/pi-ai/subpaths.js`), which re-exports the existing pi-ai glue and adds what the root
//! entry point does not export. [`helper`] serves the glue's `sub.*` calls into the Rust pi-ai
//! crate.

use std::sync::Arc;

use pi_ai::types::{
    ClassifierContext, ClassifierModel, ClassifierQuestion, Message, Model, ModelThinkingLevel,
    SamplingParams, ThinkingBudgets, ThinkingLevel, Tool, ToolResultMessage, UserContent,
};
use serde_json::{Map, Value, json};

use super::Bridge;

/// One part of a subpath module.
enum Part {
    /// Re-exports from `pi-rs:ai-subpaths`: space-separated `name` or `name=glueName`.
    Glue(&'static str),
    /// `export function <factory>()` constructing the built-in provider `<id>`.
    Provider {
        factory: &'static str,
        id: &'static str,
    },
    /// `<PREFIX>_MODELS`, `<PREFIX>_IMAGE_MODELS`, `<PREFIX>_CLASSIFIER_MODELS` of provider `<id>`.
    Catalog {
        prefix: &'static str,
        id: &'static str,
    },
    /// A side effect of importing the module: calls the glue function of that name.
    Run(&'static str),
}

use Part::{Catalog, Glue, Provider, Run};

/// Specifier prefix of the subpaths.
const PACKAGE: &str = "@earendil-works/pi-ai";

/// Built-in providers with a `providers/<file>.ts` factory and a `providers/<file>.models.ts`
/// catalog: (file, factory, provider id, catalog constant prefix).
const BUILTIN_PROVIDERS: &[(&str, &str, &str, &str)] = &[
    (
        "amazon-bedrock",
        "amazonBedrockProvider",
        "amazon-bedrock",
        "AMAZON_BEDROCK",
    ),
    ("ant-ling", "antLingProvider", "ant-ling", "ANT_LING"),
    ("anthropic", "anthropicProvider", "anthropic", "ANTHROPIC"),
    ("azure", "azureProvider", "azure", "AZURE"),
    ("baseten", "basetenProvider", "baseten", "BASETEN"),
    ("cerebras", "cerebrasProvider", "cerebras", "CEREBRAS"),
    (
        "cloudflare-ai-gateway",
        "cloudflareAIGatewayProvider",
        "cloudflare-ai-gateway",
        "CLOUDFLARE_AI_GATEWAY",
    ),
    (
        "cloudflare-workers-ai",
        "cloudflareWorkersAIProvider",
        "cloudflare-workers-ai",
        "CLOUDFLARE_WORKERS_AI",
    ),
    ("deepseek", "deepseekProvider", "deepseek", "DEEPSEEK"),
    ("fireworks", "fireworksProvider", "fireworks", "FIREWORKS"),
    (
        "github-copilot",
        "githubCopilotProvider",
        "github-copilot",
        "GITHUB_COPILOT",
    ),
    ("google", "googleProvider", "google", "GOOGLE"),
    (
        "google-vertex",
        "googleVertexProvider",
        "google-vertex",
        "GOOGLE_VERTEX",
    ),
    ("groq", "groqProvider", "groq", "GROQ"),
    (
        "huggingface",
        "huggingfaceProvider",
        "huggingface",
        "HUGGINGFACE",
    ),
    (
        "kimi-coding",
        "kimiCodingProvider",
        "kimi-coding",
        "KIMI_CODING",
    ),
    ("meta", "metaProvider", "meta", "META"),
    ("minimax", "minimaxProvider", "minimax", "MINIMAX"),
    (
        "minimax-cn",
        "minimaxCnProvider",
        "minimax-cn",
        "MINIMAX_CN",
    ),
    ("mistral", "mistralProvider", "mistral", "MISTRAL"),
    (
        "moonshotai",
        "moonshotaiProvider",
        "moonshotai",
        "MOONSHOTAI",
    ),
    (
        "moonshotai-cn",
        "moonshotaiCnProvider",
        "moonshotai-cn",
        "MOONSHOTAI_CN",
    ),
    ("nvidia", "nvidiaProvider", "nvidia", "NVIDIA"),
    ("openai", "openaiProvider", "openai", "OPENAI"),
    (
        "openai-codex",
        "openaiCodexProvider",
        "openai-codex",
        "OPENAI_CODEX",
    ),
    ("opencode", "opencodeProvider", "opencode", "OPENCODE"),
    (
        "opencode-go",
        "opencodeGoProvider",
        "opencode-go",
        "OPENCODE_GO",
    ),
    (
        "openrouter",
        "openrouterProvider",
        "openrouter",
        "OPENROUTER",
    ),
    (
        "qwen-token-plan",
        "qwenTokenPlanProvider",
        "qwen-token-plan",
        "QWEN_TOKEN_PLAN",
    ),
    (
        "qwen-token-plan-cn",
        "qwenTokenPlanCnProvider",
        "qwen-token-plan-cn",
        "QWEN_TOKEN_PLAN_CN",
    ),
    (
        "qwen-token-plan-individual",
        "qwenTokenPlanIndividualProvider",
        "qwen-token-plan-individual",
        "QWEN_TOKEN_PLAN_INDIVIDUAL",
    ),
    ("together", "togetherProvider", "together", "TOGETHER"),
    ("typesafe", "typesafeProvider", "typesafe", "TYPESAFE"),
    (
        "vercel-ai-gateway",
        "vercelAIGatewayProvider",
        "vercel-ai-gateway",
        "VERCEL_AI_GATEWAY",
    ),
    ("xai", "xaiProvider", "xai", "XAI"),
    ("xiaomi", "xiaomiProvider", "xiaomi", "XIAOMI"),
    (
        "xiaomi-token-plan-ams",
        "xiaomiTokenPlanAmsProvider",
        "xiaomi-token-plan-ams",
        "XIAOMI_TOKEN_PLAN_AMS",
    ),
    (
        "xiaomi-token-plan-cn",
        "xiaomiTokenPlanCnProvider",
        "xiaomi-token-plan-cn",
        "XIAOMI_TOKEN_PLAN_CN",
    ),
    (
        "xiaomi-token-plan-sgp",
        "xiaomiTokenPlanSgpProvider",
        "xiaomi-token-plan-sgp",
        "XIAOMI_TOKEN_PLAN_SGP",
    ),
    ("zai", "zaiProvider", "zai", "ZAI"),
    (
        "zai-coding-cn",
        "zaiCodingCnProvider",
        "zai-coding-cn",
        "ZAI_CODING_CN",
    ),
];

/// Exports of `providers/<file>.ts` besides the factory.
const PROVIDER_EXTRAS: &[(&str, &str)] = &[("openrouter", "personalizeOpenRouterCatalog")];

/// Subpaths other than the built-in provider factories and catalogs.
const SUBPATHS: &[(&str, &[Part])] = &[
    // providers/*
    (
        "providers/faux",
        &[Glue(
            "createFauxCore fauxAssistantMessage fauxProvider fauxText fauxThinking fauxToolCall",
        )],
    ),
    ("providers/radius", &[Glue("radiusProvider")]),
    (
        "providers/radius.models",
        &[Catalog {
            prefix: "RADIUS",
            id: "radius",
        }],
    ),
    (
        "providers/cloudflare-auth",
        &[Glue("cloudflareAIGatewayAuth cloudflareWorkersAIAuth")],
    ),
    (
        "providers/cloudflare-stream",
        &[Glue(
            "cloudflareClassifier cloudflareStreams resolveCloudflareModel",
        )],
    ),
    (
        "providers/compat-schema",
        &[Glue(
            "AnthropicAllowedFallbackModelSchema AnthropicMessagesCompatSchema BedrockCompatSchema \
             ChatTemplateKwargValueSchema CompatSchemasByApi MistralConversationsCompatSchema \
             OpenAICompletionsCompatSchema OpenAIResponsesCompatSchema OpenRouterRoutingSchema ProviderCompatSchema \
             SessionAffinityFormatSchema SystemOneCompatSchema ThinkingTokenBudgetFieldSchema \
             VercelGatewayRoutingSchema",
        )],
    ),
    (
        "providers/model-schema",
        &[Glue(
            "CACHE_RETENTIONS CacheRetentionSchema MODEL_INPUT_MODALITIES MODEL_PROMPT_CACHE_RETENTIONS \
             MODEL_THINKING_LEVELS ModelCostRatesSchema ModelCostSchema ModelCostTierSchema \
             ModelImageInputLimitsSchema ModelImageResizeOptionsSchema ModelInputLimitsSchema \
             ModelInputModalitySchema ModelPromptCacheSchema ModelThinkingLevelSchema THINKING_LEVELS \
             ThinkingLevelMapSchema ThinkingLevelSchema",
        )],
    ),
    (
        "providers/images/register-builtins",
        &[
            Glue("generateImagesOpenRouter registerBuiltInImagesApiProviders"),
            Run("registerBuiltInImagesApiProviders"),
        ],
    ),
    (
        "providers/opencode-headers",
        &[Glue("withOpenCodeSessionHeader")],
    ),
    ("providers/openrouter-api", &[Glue("openRouterCost")]),
    (
        "providers/radius-config",
        &[Glue(
            "DEFAULT_RADIUS_GATEWAY getRadiusCredentialConfig getRadiusModels getRadiusModelsFromConfig \
             loadRadiusGatewayConfig normalizeRadiusGatewayUrl",
        )],
    ),
    // api/*
    (
        "api/anthropic-messages",
        &[Glue(
            "stream=anthropicMessagesStream streamSimple=anthropicMessagesStreamSimple",
        )],
    ),
    (
        "api/anthropic-messages.lazy",
        &[Glue("anthropicMessagesApi")],
    ),
    (
        "api/azure-openai-config",
        &[Glue(
            "resolveAzureBaseUrl resolveAzureConfig resolveDeploymentName",
        )],
    ),
    (
        "api/azure-openai-responses",
        &[Glue(
            "stream=azureOpenAIResponsesStream streamSimple=azureOpenAIResponsesStreamSimple",
        )],
    ),
    (
        "api/azure-openai-responses.lazy",
        &[Glue("azureOpenAIResponsesApi")],
    ),
    (
        "api/bedrock-converse-stream",
        &[Glue(
            "stream=bedrockConverseStreamStream streamSimple=bedrockConverseStreamStreamSimple",
        )],
    ),
    (
        "api/bedrock-converse-stream.lazy",
        &[Glue("bedrockConverseStreamApi setBedrockProviderModule")],
    ),
    (
        "api/classifier-shared",
        &[Glue(
            "isRecord parseClassifierUsage postClassifierRequest requiredNumber",
        )],
    ),
    (
        "api/cloudflare",
        &[Glue(
            "CLOUDFLARE_AI_GATEWAY_ANTHROPIC_BASE_URL CLOUDFLARE_AI_GATEWAY_COMPAT_BASE_URL \
             CLOUDFLARE_AI_GATEWAY_OPENAI_BASE_URL CLOUDFLARE_WORKERS_AI_BASE_URL CLOUDFLARE_WORKERS_AI_REST_BASE_URL",
        )],
    ),
    (
        "api/cloudflare-ai-binding",
        &[Glue(
            "CLOUDFLARE_GATEWAY_BINDING_AUTH_SENTINEL createAiBindingFetch",
        )],
    ),
    (
        "api/cloudflare-workers-ai-system-one",
        &[Glue("classify=cloudflareWorkersAISystemOneClassify")],
    ),
    (
        "api/cloudflare-workers-ai-system-one.lazy",
        &[Glue("cloudflareWorkersAISystemOneApi")],
    ),
    (
        "api/constrained-sampling",
        &[Glue(
            "appendGrammarToolInputJsonDelta createGrammarToolInputProperties getGrammarToolInput \
             getJsonSchemaToolParameters makeStrictJsonSchema resolveGrammarConstrainedSampling \
             resolveJsonSchemaStrictSampling",
        )],
    ),
    (
        "api/github-copilot-headers",
        &[Glue(
            "buildCopilotDynamicHeaders hasCopilotVisionInput inferCopilotInitiator",
        )],
    ),
    (
        "api/google-generative-ai",
        &[Glue(
            "stream=googleGenerativeAIStream streamSimple=googleGenerativeAIStreamSimple",
        )],
    ),
    (
        "api/google-generative-ai.lazy",
        &[Glue("googleGenerativeAIApi")],
    ),
    (
        "api/google-shared",
        &[Glue(
            "convertMessages=googleConvertMessages convertTools=googleConvertTools getDisabledGoogleThinkingConfig \
             isThinkingPart mapStopReason=googleMapStopReason mapStopReasonString=googleMapStopReasonString \
             mapToolChoice=googleMapToolChoice requiresToolCallId resolveGoogleFunctionCallingMode \
             resolveGoogleThinkingLevel retainThoughtSignature retryGoogleRequest supportsGoogleStrictToolSampling \
             toGoogleSdkThinkingLevel toGoogleThinkingLevel usesGoogleThinkingLevel",
        )],
    ),
    (
        "api/google-vertex",
        &[Glue(
            "stream=googleVertexStream streamSimple=googleVertexStreamSimple",
        )],
    ),
    ("api/google-vertex.lazy", &[Glue("googleVertexApi")]),
    ("api/lazy", &[Glue("lazyApi lazyStream")]),
    (
        "api/llama-cpp-classify",
        &[Glue(
            "answerFromProbabilities classify=llamaCppClassify labelProbabilities llamaServerRoot peakConfidence \
             renderQuestion",
        )],
    ),
    (
        "api/llama-cpp-classify.lazy",
        &[Glue("llamaCppClassifyApi")],
    ),
    (
        "api/mistral-conversations",
        &[Glue(
            "stream=mistralConversationsStream streamSimple=mistralConversationsStreamSimple",
        )],
    ),
    (
        "api/mistral-conversations.lazy",
        &[Glue("mistralConversationsApi")],
    ),
    (
        "api/openai-codex-responses",
        &[Glue(
            "closeOpenAICodexWebSocketSessions getOpenAICodexWebSocketDebugStats resetOpenAICodexWebSocketDebugStats \
             stream=openAICodexResponsesStream streamSimple=openAICodexResponsesStreamSimple",
        )],
    ),
    (
        "api/openai-codex-responses.lazy",
        &[Glue("openAICodexResponsesApi")],
    ),
    (
        "api/openai-completions",
        &[Glue(
            "convertMessages=openAICompletionsConvertMessages stream=openAICompletionsStream \
             streamSimple=openAICompletionsStreamSimple",
        )],
    ),
    (
        "api/openai-completions.lazy",
        &[Glue("openAICompletionsApi")],
    ),
    (
        "api/openai-decisions",
        &[Glue("classify=openAIDecisionsClassify")],
    ),
    ("api/openai-decisions.lazy", &[Glue("openAIDecisionsApi")]),
    (
        "api/openai-prompt-cache",
        &[Glue(
            "OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH clampOpenAIPromptCacheKey",
        )],
    ),
    (
        "api/openai-responses",
        &[Glue(
            "stream=openAIResponsesStream streamSimple=openAIResponsesStreamSimple",
        )],
    ),
    (
        "api/openai-responses-shared",
        &[Glue(
            "convertResponsesMessages convertResponsesTools getResponsesToolSupport processResponsesStream",
        )],
    ),
    ("api/openai-responses.lazy", &[Glue("openAIResponsesApi")]),
    (
        "api/openrouter-images",
        &[Glue("generateImages=generateImagesOpenRouter")],
    ),
    ("api/openrouter-images.lazy", &[Glue("openrouterImagesApi")]),
    (
        "api/pi-messages",
        &[Glue(
            "PiMessagesResponseError stream=piMessagesStream streamSimple=piMessagesStreamSimple",
        )],
    ),
    ("api/pi-messages.lazy", &[Glue("piMessagesApi")]),
    (
        "api/simple-options",
        &[Glue(
            "DEFAULT_THINKING_BUDGETS MIN_ANSWER_TOKENS adjustMaxTokensForThinking buildBaseOptions \
             clampMaxTokensToContext clampReasoning clampThinkingBudgetToAnswerRoom resolveSamplingParams \
             thinkingBudgetForLevel",
        )],
    ),
    ("api/system-one-shared", &[Glue("classifySystemOne")]),
    (
        "api/transform-messages",
        &[Glue("splitErrorToolResultImages transformMessages")],
    ),
    (
        "api/typesafe-system-one",
        &[Glue("classify=typesafeSystemOneClassify")],
    ),
    (
        "api/typesafe-system-one.lazy",
        &[Glue("typesafeSystemOneApi")],
    ),
    // utils/*
    (
        "utils/abort",
        &[Glue("operationSignal raceWithAbortSignal")],
    ),
    ("utils/abort-signals", &[Glue("combineAbortSignals")]),
    (
        "utils/diagnostics",
        &[Glue(
            "appendAssistantMessageDiagnostic createAssistantMessageDiagnostic extractDiagnosticError formatThrownValue",
        )],
    ),
    (
        "utils/error-body",
        &[Glue(
            "MAX_PROVIDER_ERROR_BODY_CHARS formatProviderError normalizeProviderError safeJsonStringify \
             truncateErrorText",
        )],
    ),
    (
        "utils/estimate",
        &[Glue(
            "calculateContextTokens estimateContextTokens estimateMessageTokens estimateTextAndImageContentTokens \
             estimateTextTokens",
        )],
    ),
    (
        "utils/event-stream",
        &[Glue(
            "AssistantMessageEventStream EventStream createAssistantMessageEventStream",
        )],
    ),
    ("utils/hash", &[Glue("shortHash")]),
    (
        "utils/headers",
        &[Glue("headersToRecord providerHeadersToRecord")],
    ),
    (
        "utils/identity",
        &[Glue(
            "PI_ATTRIBUTION formatUserAgent getAttributionHeaders getClientVersion getIdentityHeaders \
             getOpenCodeSessionHeaders mergeClientIdentity withClientIdentity",
        )],
    ),
    (
        "utils/json-parse",
        &[Glue(
            "parseJsonWithRepair parseStreamingJson parseToolCallArguments repairJson",
        )],
    ),
    (
        "utils/model-operations",
        &[Glue(
            "assertChatModel assertClassifierInputSupported assertClassifierModel assertImageModel \
             classifierErrorResult getModelType imageErrorResult isModelType",
        )],
    ),
    ("utils/models-error", &[Glue("ModelsError")]),
    (
        "utils/node-http-proxy",
        &[Glue(
            "UNSUPPORTED_PROXY_PROTOCOL_MESSAGE resolveHttpProxyUrlForTarget",
        )],
    ),
    (
        "utils/oauth-page",
        &[Glue("oauthErrorHtml oauthSuccessHtml")],
    ),
    (
        "utils/overflow",
        &[Glue(
            "getOverflowPatterns isContextOverflow isRecoverableLength",
        )],
    ),
    ("utils/provider-env", &[Glue("getProviderEnvValue")]),
    ("utils/provider-retry", &[Glue("retryProviderRequest")]),
    (
        "utils/retry",
        &[Glue(
            "DEFAULT_MAX_AGENT_RETRY_DELAY_MS isRetryableAssistantError retryAssistantCall retryDelayMs",
        )],
    ),
    ("utils/sanitize-unicode", &[Glue("sanitizeSurrogates")]),
    ("utils/sleep", &[Glue("sleep")]),
    (
        "utils/text",
        &[Glue(
            "contentText getSystemMessageText renderSystemMessageUpdate",
        )],
    ),
    (
        "utils/transcript",
        &[Glue(
            "collapseSystemMessages createInitialSystemMessage declarationsEqual getCurrentSystemMessage \
             getCurrentSystemPrompt getCurrentTools getDeclaredTools getInitialSystemMessage getToolStateChanges \
             hasNonAdditiveToolChanges hasToolRedefinitions normalizeContext resolveTranscript resolveTranscriptTools \
             toToolDeclaration withoutInitialSystemMessage",
        )],
    ),
    ("utils/typebox-helpers", &[Glue("StringEnum")]),
    ("utils/uuid", &[Glue("uuidv7")]),
    (
        "utils/validation",
        &[Glue("validateToolArguments validateToolCall")],
    ),
    ("utils/version", &[Glue("PI_AI_VERSION")]),
    // Top-level entry points.
    ("bedrock-provider", &[Glue("bedrockProviderModule")]),
    ("bun-oauth", &[Glue("registerBunOAuthFlows")]),
];

/// ES module source of one subpath.
fn module_source(parts: &[Part]) -> String {
    let mut reexports: Vec<String> = Vec::new();
    let mut imports: Vec<&str> = Vec::new();
    let mut body = String::new();
    for part in parts {
        match part {
            Glue(names) => {
                reexports.extend(
                    names
                        .split_whitespace()
                        .map(|spec| match spec.split_once('=') {
                            Some((name, internal)) => format!("{internal} as {name}"),
                            None => spec.to_string(),
                        }),
                )
            }
            Provider { factory, id } => {
                imports.push("builtinProvider");
                body.push_str(&format!(
                    "export function {factory}() {{\n\treturn builtinProvider({id:?});\n}}\n"
                ));
            }
            Catalog { prefix, id } => {
                imports.push("builtinCatalog");
                for (suffix, kind) in [
                    ("MODELS", "chat"),
                    ("IMAGE_MODELS", "image"),
                    ("CLASSIFIER_MODELS", "classifier"),
                ] {
                    body.push_str(&format!(
                        "export const {prefix}_{suffix} = builtinCatalog({id:?}, {kind:?});\n"
                    ));
                }
            }
            Run(function) => {
                imports.push(function);
                body.push_str(&format!("{function}();\n"));
            }
        }
    }
    let mut source = String::new();
    if !imports.is_empty() {
        source.push_str(&format!(
            "import {{ {} }} from \"pi-rs:ai-subpaths\";\n",
            imports.join(", ")
        ));
    }
    if !reexports.is_empty() {
        source.push_str(&format!(
            "export {{ {} }} from \"pi-rs:ai-subpaths\";\n",
            reexports.join(", ")
        ));
    }
    source.push_str(&body);
    source
}

/// Every deep subpath module: (`@earendil-works/pi-ai/<subpath>`, ES module source).
pub(super) fn modules() -> Vec<(String, String)> {
    let mut modules = Vec::new();
    for (file, factory, id, prefix) in BUILTIN_PROVIDERS {
        let extra = PROVIDER_EXTRAS
            .iter()
            .find(|(f, _)| f == file)
            .map(|(_, names)| *names)
            .unwrap_or_default();
        modules.push((
            format!("{PACKAGE}/providers/{file}"),
            module_source(&[Provider { factory, id }, Glue(extra)]),
        ));
        modules.push((
            format!("{PACKAGE}/providers/{file}.models"),
            module_source(&[Catalog { prefix, id }]),
        ));
    }
    for (subpath, parts) in SUBPATHS {
        modules.push((format!("{PACKAGE}/{subpath}"), module_source(parts)));
    }
    modules
}

/// Model fields TS helpers do not read but the Rust `Model` requires: callers may pass a
/// partial model (`Pick<Model, "id">`), as in TS.
fn model_arg(value: &Value) -> anyhow::Result<Model> {
    let mut model = json!({
        "id": "", "name": "", "api": "", "provider": "", "baseUrl": "", "input": [],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
    });
    if let (Some(target), Some(source)) = (model.as_object_mut(), value.as_object()) {
        for (key, v) in source {
            target.insert(key.clone(), v.clone());
        }
    }
    Ok(serde_json::from_value(model)?)
}

fn from<T: serde::de::DeserializeOwned>(value: &Value) -> anyhow::Result<T> {
    Ok(serde_json::from_value(value.clone())?)
}

/// `undefined`/`null` arguments as `None`.
fn optional<T: serde::de::DeserializeOwned>(value: &Value) -> anyhow::Result<Option<T>> {
    if value.is_null() {
        Ok(None)
    } else {
        from(value).map(Some)
    }
}

/// Messages of a `TranscriptContext` or a message array (`estimateContextTokens`).
fn messages_of(value: &Value) -> anyhow::Result<Vec<Message>> {
    from(value.get("messages").unwrap_or(value))
}

fn tools_arg(value: &Value) -> anyhow::Result<Vec<Tool>> {
    Ok(optional(value)?.unwrap_or_default())
}

/// The `sub.*` calls of `js/pi-ai/subpaths.js`; `None` for other names.
pub(super) fn helper(
    bridge: &Arc<Bridge>,
    name: &str,
    args: &[Value],
) -> Option<anyhow::Result<Value>> {
    let name = name.strip_prefix("sub.")?;
    Some(call_helper(bridge, name, args))
}

fn call_helper(bridge: &Arc<Bridge>, name: &str, args: &[Value]) -> anyhow::Result<Value> {
    use pi_ai::api::{
        azure_openai_config as azure, classifier_shared, constrained_sampling as constrained,
        github_copilot_headers, google_shared as google, llama_cpp_classify as llama,
        openai_codex_responses as codex, simple_options, transform_messages,
    };
    use pi_ai::utils::{estimate, node_http_proxy, oauth_page};

    let null = Value::Null;
    let a = |i: usize| args.get(i).unwrap_or(&null);
    let s = |i: usize| a(i).as_str().unwrap_or_default().to_string();
    let options = |i: usize| super::provider::stream_options_from_json(bridge, a(i));
    Ok(match name {
        "constants" => constants(),
        "schemas" => {
            let names = a(0).as_array().cloned().unwrap_or_default();
            let mut out = Map::new();
            for name in names.iter().filter_map(Value::as_str) {
                let schema = pi_ai::providers::schemas::schema(name)
                    .ok_or_else(|| anyhow::anyhow!("pi-ai schema {name} is not embedded"))?;
                out.insert(name.to_string(), schema.clone());
            }
            Value::Object(out)
        }
        // utils/estimate.ts
        "estimateTextAndImageContentTokens" => {
            json!(estimate::estimate_text_and_image_content_tokens(&from::<
                UserContent,
            >(
                a(0)
            )?))
        }
        "estimateMessageTokens" => {
            json!(estimate::estimate_message_tokens(&from::<Message>(a(0))?))
        }
        "estimateContextTokens" => {
            let e = estimate::estimate_context_tokens(&messages_of(a(0))?);
            json!({
                "tokens": e.tokens,
                "usageTokens": e.usage_tokens,
                "trailingTokens": e.trailing_tokens,
                "lastUsageIndex": e.last_usage_index,
            })
        }
        // utils/oauth-page.ts, utils/node-http-proxy.ts
        "oauthSuccessHtml" => json!(oauth_page::oauth_success_html(&s(0))),
        "oauthErrorHtml" => json!(oauth_page::oauth_error_html(&s(0), a(1).as_str())),
        "resolveHttpProxyUrlForTarget" => {
            let env = optional(a(1))?;
            json!(
                node_http_proxy::resolve_http_proxy_url_for_target(&s(0), env.as_ref())?
                    .map(|url| url.to_string())
            )
        }
        // api/github-copilot-headers.ts
        "buildCopilotDynamicHeaders" => {
            json!(github_copilot_headers::build_copilot_dynamic_headers(
                &from::<Vec<Message>>(&a(0)["messages"])?,
                a(0)["hasImages"].as_bool().unwrap_or(false),
            ))
        }
        // api/simple-options.ts
        "clampMaxTokensToContext" => json!(simple_options::clamp_max_tokens_to_context(
            &model_arg(a(0))?,
            &super::native::transcript_from_js(a(1))?,
            a(2).as_u64().unwrap_or_default(),
        )),
        "resolveSamplingParams" => json!(simple_options::resolve_sampling_params(
            &model_arg(a(0))?,
            from::<ModelThinkingLevel>(a(1))?,
            optional::<SamplingParams>(a(2))?.as_ref(),
        )),
        "thinkingBudgetForLevel" => json!(simple_options::thinking_budget_for_level(
            from::<ThinkingLevel>(a(0))?,
            optional::<ThinkingBudgets>(a(1))?.as_ref(),
        )),
        "adjustMaxTokensForThinking" => {
            let (max_tokens, thinking_budget) = simple_options::adjust_max_tokens_for_thinking(
                a(0).as_u64(),
                a(1).as_u64().unwrap_or_default(),
                from::<ThinkingLevel>(a(2))?,
                optional::<ThinkingBudgets>(a(3))?.as_ref(),
            );
            json!({ "maxTokens": max_tokens, "thinkingBudget": thinking_budget })
        }
        // api/transform-messages.ts
        "transformMessages" => serde_json::to_value(transform_messages::transform_messages(
            &from::<Vec<Message>>(a(0))?,
            &model_arg(a(1))?,
            None,
        ))?,
        "splitErrorToolResultImages" => {
            match transform_messages::split_error_tool_result_images(&from::<ToolResultMessage>(
                a(0),
            )?) {
                Some(split) => json!({
                    "content": serde_json::to_value(split.content)?,
                    "caption": split.caption,
                    "images": serde_json::to_value(split.images)?,
                }),
                None => Value::Null,
            }
        }
        // api/llama-cpp-classify.ts
        "renderQuestion" => {
            let q = llama::render_question(&from::<ClassifierContext>(a(0))?, &s(1))?;
            json!({ "content": q.content, "labels": q.labels, "keys": q.keys })
        }
        "answerFromProbabilities" => serde_json::to_value(llama::answer_from_probabilities(
            &from::<ClassifierQuestion>(a(0))?,
            &from::<Vec<String>>(a(1))?,
            &from::<Vec<f64>>(a(2))?,
        ))?,
        // api/classifier-shared.ts
        "parseClassifierUsage" => serde_json::to_value(classifier_shared::parse_classifier_usage(
            Some(a(0)),
            &from::<ClassifierModel>(a(1))?,
        ))?,
        // api/google-shared.ts
        "resolveGoogleThinkingLevel" => {
            json!(
                google::resolve_google_thinking_level(
                    &model_arg(a(0))?,
                    from::<ModelThinkingLevel>(a(1))?
                )?
                .as_str()
            )
        }
        "usesGoogleThinkingLevel" => json!(google::uses_google_thinking_level(&model_arg(a(0))?)),
        "getDisabledGoogleThinkingConfig" => {
            google::get_disabled_google_thinking_config(&model_arg(a(0))?)?
        }
        "requiresToolCallId" => json!(google::requires_tool_call_id(&s(0))),
        "googleConvertMessages" => {
            json!(google::convert_messages(
                &model_arg(a(0))?,
                &super::native::transcript_from_js(a(1))?
            ))
        }
        "googleConvertTools" => json!(google::convert_tools(
            &tools_arg(a(0))?,
            a(1).as_bool().unwrap_or(false),
            a(2).as_bool().unwrap_or(true),
        )?),
        "supportsGoogleStrictToolSampling" => {
            json!(google::supports_google_strict_tool_sampling(&s(0)))
        }
        "resolveGoogleFunctionCallingMode" => json!(google::resolve_google_function_calling_mode(
            &tools_arg(a(0))?,
            a(1).as_str(),
            a(2).as_bool().unwrap_or(false),
        )?),
        "googleMapStopReason" => serde_json::to_value(google::map_stop_reason(&s(0))?)?,
        "googleMapStopReasonString" => serde_json::to_value(google::map_stop_reason_string(&s(0)))?,
        // api/constrained-sampling.ts
        "makeStrictJsonSchema" => {
            constrained::make_strict_json_schema(a(0), None).map_err(anyhow::Error::msg)?
        }
        "getJsonSchemaToolParameters" => {
            constrained::get_json_schema_tool_parameters(&from::<Tool>(a(0))?, a(1).as_bool())
        }
        "resolveJsonSchemaStrictSampling" => {
            json!(constrained::resolve_json_schema_strict_sampling(
                &from::<Tool>(a(0))?,
                a(1).as_bool().unwrap_or(false),
                None,
            )?)
        }
        "resolveGrammarConstrainedSampling" => {
            match constrained::resolve_grammar_constrained_sampling(
                &from::<Tool>(a(0))?,
                a(1).as_bool().unwrap_or(false),
            )? {
                Some(g) => {
                    json!({ "format": g.format, "definition": g.definition, "inputProperty": g.input_property })
                }
                None => Value::Null,
            }
        }
        // api/azure-openai-config.ts
        "resolveDeploymentName" => json!(azure::resolve_deployment_name(
            &model_arg(a(0))?,
            Some(&options(1))
        )),
        "resolveAzureBaseUrl" => json!(azure::resolve_azure_base_url(
            &model_arg(a(0))?,
            Some(&options(1))
        )?),
        "resolveAzureConfig" => {
            let (base_url, api_version) =
                azure::resolve_azure_config(&model_arg(a(0))?, Some(&options(1)))?;
            json!({ "baseUrl": base_url, "apiVersion": api_version })
        }
        // api/openai-codex-responses.ts
        "getOpenAICodexWebSocketDebugStats" => {
            match codex::get_openai_codex_websocket_debug_stats(&s(0)) {
                Some(st) => json!({
                    "requests": st.requests,
                    "connectionsCreated": st.connections_created,
                    "connectionsReused": st.connections_reused,
                    "cachedContextRequests": st.cached_context_requests,
                    "storeTrueRequests": st.store_true_requests,
                    "fullContextRequests": st.full_context_requests,
                    "deltaRequests": st.delta_requests,
                    "lastInputItems": st.last_input_items,
                    "lastDeltaInputItems": st.last_delta_input_items,
                    "lastPreviousResponseId": st.last_previous_response_id,
                    "websocketFailures": st.websocket_failures,
                    "sseFallbacks": st.sse_fallbacks,
                    "websocketFallbackActive": st.websocket_fallback_active,
                    "lastWebsocketError": st.last_websocket_error,
                }),
                None => Value::Null,
            }
        }
        "resetOpenAICodexWebSocketDebugStats" => {
            codex::reset_openai_codex_websocket_debug_stats(a(0).as_str());
            Value::Null
        }
        "closeOpenAICodexWebSocketSessions" => {
            codex::close_openai_codex_websocket_sessions(a(0).as_str());
            Value::Null
        }
        // providers/openrouter-api.ts, providers/radius-config.ts
        "openRouterCost" => serde_json::to_value(
            pi_ai::providers::openrouter_api::open_router_cost(optional(a(0))?.as_ref()),
        )?,
        "getRadiusCredentialConfig" => serde_json::to_value(
            pi_ai::providers::radius_config::get_radius_credential_config(optional(a(0))?.as_ref()),
        )?,
        "getRadiusModelsFromConfig" => serde_json::to_value(
            pi_ai::providers::radius_config::get_radius_models_from_config(&s(0), &from(a(1))?),
        )?,
        "getRadiusModels" => serde_json::to_value(
            pi_ai::providers::radius_config::get_radius_models(&s(0), optional(a(1))?.as_ref()),
        )?,
        _ => anyhow::bail!("pi-ai sub.{name} is not available to extensions in pi-rs"),
    })
}

/// Runtime constants of the subpaths, from the Rust pi-ai crate.
fn constants() -> Value {
    use pi_ai::api::{cloudflare, openai_prompt_cache, simple_options};
    json!({
        "CLOUDFLARE_WORKERS_AI_BASE_URL": cloudflare::CLOUDFLARE_WORKERS_AI_BASE_URL,
        "CLOUDFLARE_WORKERS_AI_REST_BASE_URL": cloudflare::CLOUDFLARE_WORKERS_AI_REST_BASE_URL,
        "CLOUDFLARE_AI_GATEWAY_COMPAT_BASE_URL": cloudflare::CLOUDFLARE_AI_GATEWAY_COMPAT_BASE_URL,
        "CLOUDFLARE_AI_GATEWAY_OPENAI_BASE_URL": cloudflare::CLOUDFLARE_AI_GATEWAY_OPENAI_BASE_URL,
        "CLOUDFLARE_AI_GATEWAY_ANTHROPIC_BASE_URL": cloudflare::CLOUDFLARE_AI_GATEWAY_ANTHROPIC_BASE_URL,
        "CLOUDFLARE_GATEWAY_BINDING_AUTH_SENTINEL": cloudflare::CLOUDFLARE_GATEWAY_BINDING_AUTH_SENTINEL,
        "DEFAULT_RADIUS_GATEWAY": pi_ai::providers::radius_config::DEFAULT_RADIUS_GATEWAY,
        "MIN_ANSWER_TOKENS": simple_options::MIN_ANSWER_TOKENS,
        "DEFAULT_THINKING_BUDGETS": simple_options::DEFAULT_THINKING_BUDGETS,
        "OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH": openai_prompt_cache::OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH,
        "MAX_PROVIDER_ERROR_BODY_CHARS": pi_ai::utils::error_body::MAX_PROVIDER_ERROR_BODY_CHARS,
        "UNSUPPORTED_PROXY_PROTOCOL_MESSAGE": pi_ai::utils::node_http_proxy::UNSUPPORTED_PROXY_PROTOCOL_MESSAGE,
    })
}
