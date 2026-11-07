//! pi-ai's deep entry points (`@earendil-works/pi-ai/providers/*`, `/api/*`, `/utils/*`,
//! `/bedrock-provider`, `/bun-oauth`, ...) for JavaScript extensions: export-name parity with
//! pi-ai 1.4.0's `package.json` `exports`, behavior of the subpath modules, and a faux provider
//! imported from `@earendil-works/pi-ai/providers/faux` streaming through pi.

mod extensions_support;
mod support;

use std::collections::BTreeMap;
use std::sync::Arc;

use extensions_support::*;
use pi_ai::types::{Context, Message, StopReason, UserContent, UserMessage};
use pi_coding_agent::core::resource_loader::{
    DefaultResourceLoader, DefaultResourceLoaderOptions, ResourceLoader,
};
use serde_json::{Value, json};
use support::*;

/// Runtime export names per subpath, from the TS sources (`scripts/pi-ai-subpath-exports.py`).
const EXPECTED_EXPORTS: &str = include_str!("fixtures/pi-ai-subpath-exports.json");

fn expected_exports() -> BTreeMap<String, Vec<String>> {
    serde_json::from_str(EXPECTED_EXPORTS).expect("valid export fixture")
}

/// Load `source` as an extension and return the JSON it puts in the `out` tool's description.
async fn extension_output(source: &str) -> Value {
    let dir = ExtDir::new();
    dir.write("subpaths.ts", source);
    let result = dir.discover().await;
    assert!(result.errors.is_empty(), "{:#?}", result.errors);
    let extension = result.extensions[0].read();
    serde_json::from_str(&extension.tools["out"].definition.description)
        .expect("extension output is JSON")
}

const REGISTER_OUT: &str = r#"
	pi.registerTool({
		name: "out",
		label: "out",
		description: JSON.stringify(out),
		parameters: { type: "object", properties: {} },
		execute: async () => ({ content: [] }),
	});
"#;

/// The fixture still matches the TS sources when a reference checkout is available (`PI_TS_REF`).
#[test]
fn export_fixture_matches_the_ts_reference() {
    let Some(reference) = std::env::var_os("PI_TS_REF") else {
        eprintln!("PI_TS_REF not set; skipping the TS export extraction");
        return;
    };
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/pi-ai-subpath-exports.py");
    let output = std::process::Command::new("python3")
        .arg("-I")
        .arg(script)
        .arg(reference)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let current: BTreeMap<String, Vec<String>> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        current,
        expected_exports(),
        "regenerate tests/fixtures/pi-ai-subpath-exports.json"
    );
}

/// Every subpath of pi-ai's `exports` map imports, and exports exactly the TS runtime names,
/// none of them `undefined`. The `@mariozechner/pi-ai` aliases resolve to the same modules.
#[tokio::test(flavor = "multi_thread")]
async fn every_pi_ai_subpath_exports_the_ts_names() {
    let expected = expected_exports();
    let mut source = String::new();
    for (index, subpath) in expected.keys().enumerate() {
        let specifier = format!("@earendil-works/pi-ai/{}", subpath.trim_start_matches("./"));
        source.push_str(&format!("import * as m{index} from {specifier:?};\n"));
    }
    source.push_str("import * as legacyFaux from \"@mariozechner/pi-ai/providers/faux\";\n");
    source.push_str("import * as faux from \"@earendil-works/pi-ai/providers/faux\";\n");
    source.push_str(
        "export default function (pi) {\n\tconst out = { names: {}, undefinedExports: [] };\n",
    );
    for (index, subpath) in expected.keys().enumerate() {
        source.push_str(&format!(
            "\tout.names[{subpath:?}] = Object.keys(m{index}).sort();\n\
             \tfor (const [name, value] of Object.entries(m{index})) if (value === undefined) out.undefinedExports.push({subpath:?} + \" \" + name);\n"
        ));
    }
    source.push_str("\tout.legacyAlias = legacyFaux.fauxProvider === faux.fauxProvider;\n");
    source.push_str(REGISTER_OUT);
    source.push_str("}\n");

    let out = extension_output(&source).await;
    let actual: BTreeMap<String, Vec<String>> =
        serde_json::from_value(out["names"].clone()).unwrap();
    let mut mismatches = Vec::new();
    for (subpath, names) in &expected {
        let mut want = names.clone();
        want.sort();
        if actual.get(subpath) != Some(&want) {
            mismatches.push(format!(
                "{subpath}: want {want:?}, got {:?}",
                actual.get(subpath)
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "export mismatches:\n{}",
        mismatches.join("\n")
    );
    assert_eq!(out["undefinedExports"], json!([]));
    assert_eq!(out["legacyAlias"], json!(true));
}

/// Provider factories, catalogs, per-API functions, Rust-backed helpers, JS ports and the
/// named error of the exports that cannot work in pi-rs.
#[tokio::test(flavor = "multi_thread")]
async fn pi_ai_subpath_modules_behave_like_ts() {
    let out = extension_output(&format!(
        r#"
import {{ openaiProvider }} from "@earendil-works/pi-ai/providers/openai";
import {{ OPENAI_MODELS, OPENAI_IMAGE_MODELS }} from "@earendil-works/pi-ai/providers/openai.models";
import {{ anthropicProvider }} from "@earendil-works/pi-ai/providers/anthropic";
import {{ openrouterProvider, personalizeOpenRouterCatalog }} from "@earendil-works/pi-ai/providers/openrouter";
import {{ cloudflareWorkersAIAuth }} from "@earendil-works/pi-ai/providers/cloudflare-auth";
import {{ THINKING_LEVELS, MODEL_PROMPT_CACHE_RETENTIONS, ModelCostSchema }} from "@earendil-works/pi-ai/providers/model-schema";
import {{ CompatSchemasByApi }} from "@earendil-works/pi-ai/providers/compat-schema";
import {{ normalizeRadiusGatewayUrl, DEFAULT_RADIUS_GATEWAY }} from "@earendil-works/pi-ai/providers/radius-config";
import {{ openRouterCost }} from "@earendil-works/pi-ai/providers/openrouter-api";
import * as anthropicMessages from "@earendil-works/pi-ai/api/anthropic-messages";
import {{ streamAnthropic }} from "@earendil-works/pi-ai";
import {{ openAIDecisionsApi }} from "@earendil-works/pi-ai/api/openai-decisions.lazy";
import {{ openrouterImagesApi }} from "@earendil-works/pi-ai/api/openrouter-images.lazy";
import {{ PiMessagesResponseError }} from "@earendil-works/pi-ai/api/pi-messages";
import {{ adjustMaxTokensForThinking, clampReasoning, DEFAULT_THINKING_BUDGETS, thinkingBudgetForLevel }} from "@earendil-works/pi-ai/api/simple-options";
import {{ clampOpenAIPromptCacheKey, OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH }} from "@earendil-works/pi-ai/api/openai-prompt-cache";
import {{ makeStrictJsonSchema, appendGrammarToolInputJsonDelta }} from "@earendil-works/pi-ai/api/constrained-sampling";
import {{ mapStopReasonString, requiresToolCallId, toGoogleThinkingLevel }} from "@earendil-works/pi-ai/api/google-shared";
import {{ labelProbabilities, llamaServerRoot }} from "@earendil-works/pi-ai/api/llama-cpp-classify";
import {{ transformMessages }} from "@earendil-works/pi-ai/api/transform-messages";
import {{ resolveAzureBaseUrl, resolveDeploymentName }} from "@earendil-works/pi-ai/api/azure-openai-config";
import {{ buildCopilotDynamicHeaders, inferCopilotInitiator }} from "@earendil-works/pi-ai/api/github-copilot-headers";
import {{ CLOUDFLARE_WORKERS_AI_BASE_URL }} from "@earendil-works/pi-ai/api/cloudflare";
import {{ convertResponsesTools }} from "@earendil-works/pi-ai/api/openai-responses-shared";
import {{ estimateContextTokens, estimateTextTokens }} from "@earendil-works/pi-ai/utils/estimate";
import {{ shortHash }} from "@earendil-works/pi-ai/utils/hash";
import {{ sanitizeSurrogates }} from "@earendil-works/pi-ai/utils/sanitize-unicode";
import {{ providerHeadersToRecord }} from "@earendil-works/pi-ai/utils/headers";
import {{ formatProviderError, normalizeProviderError }} from "@earendil-works/pi-ai/utils/error-body";
import {{ assertChatModel }} from "@earendil-works/pi-ai/utils/model-operations";
import {{ ModelsError }} from "@earendil-works/pi-ai/utils/models-error";
import {{ oauthSuccessHtml }} from "@earendil-works/pi-ai/utils/oauth-page";
import {{ sleep }} from "@earendil-works/pi-ai/utils/sleep";
import {{ bedrockProviderModule }} from "@earendil-works/pi-ai/bedrock-provider";
import {{ registerBunOAuthFlows }} from "@earendil-works/pi-ai/bun-oauth";

export default async function (pi) {{
	const out = {{}};
	const caught = (fn) => {{ try {{ fn(); return null; }} catch (e) {{ return [e.name, e.message]; }} }};
	const openai = openaiProvider();
	out.openai = [openai.id, typeof openai.getModels, openaiProvider() !== openai];
	out.anthropic = anthropicProvider().id;
	out.openrouter = openrouterProvider().id;
	out.catalog = [Object.keys(OPENAI_MODELS).length > 0, Object.values(OPENAI_MODELS).every((m) => m.provider === "openai" && OPENAI_MODELS[m.id] === m), typeof OPENAI_IMAGE_MODELS];
	out.cloudflareAuth = typeof cloudflareWorkersAIAuth().resolve;
	out.schemas = [THINKING_LEVELS, MODEL_PROMPT_CACHE_RETENTIONS, ModelCostSchema.type, typeof CompatSchemasByApi];
	out.radius = [normalizeRadiusGatewayUrl("radius.example.com//"), DEFAULT_RADIUS_GATEWAY];
	out.openRouterCost = openRouterCost({{ prompt: "0.000001", completion: "0.000002" }});
	out.apiStreams = [anthropicMessages.stream === streamAnthropic, typeof anthropicMessages.streamSimple];
	out.classifier = typeof openAIDecisionsApi().classify;
	out.images = typeof openrouterImagesApi().generateImages;
	const piError = new PiMessagesResponseError("bad", "E1", {{ a: 1 }});
	out.piError = [piError.name, piError.code, piError.diagnosticDetails, piError instanceof Error];
	out.simple = [adjustMaxTokensForThinking(1000, 32000, "medium"), clampReasoning("xhigh"), DEFAULT_THINKING_BUDGETS.high, thinkingBudgetForLevel("low", {{ low: 7 }})];
	out.promptCache = [clampOpenAIPromptCacheKey("x".repeat(70)).length, OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH];
	out.strict = makeStrictJsonSchema({{ type: "object", properties: {{ a: {{ type: "string" }} }} }});
	const buffer = {{ input: "", started: false, closed: false }};
	out.grammar = [appendGrammarToolInputJsonDelta(buffer, "q", "a\"b", false), appendGrammarToolInputJsonDelta(buffer, "q", "a\"bc", true), buffer.closed];
	out.google = [mapStopReasonString("MAX_TOKENS"), requiresToolCallId("claude-x"), toGoogleThinkingLevel("low")];
	out.llama = [labelProbabilities([0, 0], 1), llamaServerRoot("http://h:1/v1/")];
	const model = {{ id: "m", name: "m", api: "openai-completions", provider: "p", baseUrl: "https://example.com", input: ["text"], cost: {{ input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }}, contextWindow: 1000, maxTokens: 100 }};
	out.transform = transformMessages([{{ role: "user", content: "hi", timestamp: 1 }}], model).map((m) => m.role);
	out.azure = [resolveAzureBaseUrl({{ baseUrl: "https://r.openai.azure.com" }}), resolveDeploymentName({{ id: "gpt" }}, {{ azureDeploymentName: "dep" }})];
	out.copilot = [buildCopilotDynamicHeaders({{ messages: [{{ role: "user", content: "x", timestamp: 1 }}], hasImages: false }}), inferCopilotInitiator([])];
	out.cloudflare = CLOUDFLARE_WORKERS_AI_BASE_URL;
	out.estimate = [estimateContextTokens({{ messages: [{{ role: "user", content: "abcdefgh", timestamp: 1 }}] }}), estimateTextTokens("abcdefgh")];
	out.hash = shortHash("hello");
	out.sanitize = sanitizeSurrogates("a\uD83Db");
	out.headers = providerHeadersToRecord({{ "X-A": "1", "x-b": "2" }}, {{ "x-a": "3", "X-B": null }});
	const sdkError = Object.assign(new Error("403 status code (no body)"), {{ status: 403, error: {{ message: "denied" }} }});
	out.errorBody = formatProviderError(normalizeProviderError(sdkError), "OpenAI");
	out.assert = caught(() => assertChatModel({{ ...model, type: "image" }}));
	out.assertIsModelsError = (() => {{ try {{ assertChatModel({{ ...model, type: "image" }}); }} catch (e) {{ return e instanceof ModelsError && e.code === "provider"; }} }})();
	out.oauthPage = oauthSuccessHtml("done").includes("done");
	const aborted = new AbortController();
	aborted.abort(new Error("stop"));
	out.sleep = await sleep(1, aborted.signal).then(() => "resolved", (e) => e.message);
	out.bedrock = [typeof bedrockProviderModule.stream, typeof bedrockProviderModule.streamSimple];
	out.bunOAuth = registerBunOAuthFlows() === undefined;
	out.unsupported = [caught(() => personalizeOpenRouterCatalog([], {{}})), caught(() => convertResponsesTools([]))];
{REGISTER_OUT}
}}
"#
    ))
    .await;

    assert_eq!(out["openai"], json!(["openai", "function", true]));
    assert_eq!(out["anthropic"], json!("anthropic"));
    assert_eq!(out["openrouter"], json!("openrouter"));
    assert_eq!(out["catalog"], json!([true, true, "object"]));
    assert_eq!(out["cloudflareAuth"], json!("function"));
    assert_eq!(
        out["schemas"],
        json!([
            ["minimal", "low", "medium", "high", "xhigh", "max"],
            ["short", "long"],
            "object",
            "object"
        ])
    );
    assert_eq!(
        out["radius"],
        json!(["https://radius.example.com", "https://radius.pi.dev"])
    );
    assert_eq!(out["openRouterCost"]["input"], json!(1));
    assert_eq!(out["openRouterCost"]["output"], json!(2));
    assert_eq!(out["apiStreams"], json!([true, "function"]));
    assert_eq!(out["classifier"], json!("function"));
    assert_eq!(out["images"], json!("function"));
    assert_eq!(
        out["piError"],
        json!(["PiMessagesResponseError", "E1", { "a": 1 }, true])
    );
    assert_eq!(
        out["simple"],
        json!([{ "maxTokens": 9192, "thinkingBudget": 8192 }, "high", 16384, 7])
    );
    assert_eq!(out["promptCache"], json!([64, 64]));
    assert_eq!(
        out["strict"],
        json!({
            "type": "object",
            "properties": { "a": { "anyOf": [{ "type": "string" }, { "type": "null" }] } },
            "required": ["a"],
            "additionalProperties": false,
        })
    );
    assert_eq!(out["grammar"], json!(["{\"q\":\"a\\\"b", "c\"}", true]));
    assert_eq!(out["google"], json!(["length", true, "LOW"]));
    assert_eq!(out["llama"], json!([[0.5, 0.5], "http://h:1"]));
    assert_eq!(out["transform"], json!(["user"]));
    assert_eq!(
        out["azure"],
        json!(["https://r.openai.azure.com/openai/v1", "dep"])
    );
    assert_eq!(out["copilot"][1], json!("user"));
    assert_eq!(
        out["copilot"][0]["X-Initiator"],
        json!("user"),
        "{}",
        out["copilot"]
    );
    assert_eq!(
        out["cloudflare"],
        json!("https://api.cloudflare.com/client/v4/accounts/{CLOUDFLARE_ACCOUNT_ID}/ai/v1")
    );
    assert_eq!(
        out["estimate"],
        // 3.5 characters per token, as in TS.
        json!([{ "tokens": 3, "usageTokens": 0, "trailingTokens": 3, "lastUsageIndex": null }, 3])
    );
    assert_eq!(out["sanitize"], json!("ab"));
    assert_eq!(out["headers"], json!({ "x-a": "3" }));
    assert_eq!(
        out["errorBody"],
        json!("OpenAI (403): {\"message\":\"denied\"}")
    );
    assert_eq!(
        out["assert"],
        json!(["ModelsError", "Model p/m is not a chat model"])
    );
    assert_eq!(out["assertIsModelsError"], json!(true));
    assert_eq!(out["oauthPage"], json!(true));
    assert_eq!(out["sleep"], json!("stop"));
    assert_eq!(out["bedrock"], json!(["function", "function"]));
    assert_eq!(out["bunOAuth"], json!(true));
    let unsupported = out["unsupported"].as_array().unwrap();
    for entry in unsupported {
        assert_eq!(entry[0], json!("PiRsUnsupportedError"), "{entry}");
    }
    assert!(
        unsupported[0][1].as_str().unwrap().starts_with(
            "personalizeOpenRouterCatalog from @earendil-works/pi-ai/providers/openrouter is not supported in pi-rs:"
        ),
        "{}",
        unsupported[0][1]
    );
    // TS `shortHash("hello")`.
    assert_eq!(out["hash"], json!(js_short_hash("hello")));
}

/// pi-ai's `shortHash` (cyrb53-style), computed here to check the JS port.
fn js_short_hash(text: &str) -> String {
    let mut h1: u32 = 0xdeadbeef;
    let mut h2: u32 = 0x41c6ce57;
    for ch in text.encode_utf16() {
        h1 = (h1 ^ u32::from(ch)).wrapping_mul(2654435761);
        h2 = (h2 ^ u32::from(ch)).wrapping_mul(1597334677);
    }
    let n1 =
        (h1 ^ (h1 >> 16)).wrapping_mul(2246822507) ^ (h2 ^ (h2 >> 13)).wrapping_mul(3266489909);
    let n2 =
        (h2 ^ (h2 >> 16)).wrapping_mul(2246822507) ^ (n1 ^ (n1 >> 13)).wrapping_mul(3266489909);
    format!("{}{}", radix36(n2), radix36(n1))
}

fn radix36(mut n: u32) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    loop {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
        if n == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8(out).expect("ascii digits")
}

/// A faux provider from `@earendil-works/pi-ai/providers/faux` streams inside the extension
/// (through a `Models` collection from `/models`) and through pi's model runtime once registered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn faux_provider_from_the_deep_subpath_streams() {
    let dir = std::env::temp_dir().join(format!("pi-ext-ai-subpaths-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("extension.ts");
    std::fs::write(
        &path,
        r#"
import { fauxAssistantMessage, fauxProvider, fauxText } from "@earendil-works/pi-ai/providers/faux";
import { createModels } from "@earendil-works/pi-ai/models";
import { estimateMessageTokens } from "@earendil-works/pi-ai/utils/estimate";

export default async function (pi) {
	const local = fauxProvider({ provider: "subpath-local", models: [{ id: "local-1" }] });
	local.setResponses([fauxAssistantMessage([fauxText("streamed "), fauxText("locally")])]);
	const models = createModels();
	models.setProvider(local.provider);
	const events = [];
	const stream = models.stream(local.getModel(), { messages: [{ role: "user", content: "hi", timestamp: 1 }] });
	for await (const event of stream) events.push(event.type);
	const message = await stream.result();
	const local_out = { events, text: message.content.map((c) => c.text).join(""), tokens: estimateMessageTokens(message) };

	const faux = fauxProvider({ provider: "subpath-faux", models: [{ id: "deep-1", name: "Deep One" }] });
	faux.setResponses([(context) => fauxAssistantMessage(`deep: ${context.messages.at(-1).content}`)]);
	pi.registerProvider(faux.provider);
	pi.registerTool({
		name: "local",
		label: "local",
		description: JSON.stringify(local_out),
		parameters: { type: "object", properties: {} },
		execute: async () => ({ content: [] }),
	});
}
"#,
    )
    .unwrap();
    let loader = Arc::new(DefaultResourceLoader::new(&DefaultResourceLoaderOptions {
        cwd: dir.display().to_string(),
        agent_dir: dir.display().to_string(),
        additional_extension_paths: Some(vec![path.display().to_string()]),
        no_skills: Some(true),
        no_prompt_templates: Some(true),
        no_context_files: Some(true),
        ..Default::default()
    }));
    loader.reload(None).await.unwrap();
    let extensions = loader.get_extensions();
    assert!(extensions.errors.is_empty(), "{:#?}", extensions.errors);
    let local: Value = serde_json::from_str(
        &extensions.extensions[0].read().tools["local"]
            .definition
            .description,
    )
    .unwrap();
    assert_eq!(local["text"], json!("streamed locally"));
    let events: Vec<String> = serde_json::from_value(local["events"].clone()).unwrap();
    assert_eq!(
        events.first().map(String::as_str),
        Some("start"),
        "{events:?}"
    );
    assert_eq!(
        events.last().map(String::as_str),
        Some("done"),
        "{events:?}"
    );
    assert!(events.iter().any(|e| e == "text_delta"), "{events:?}");
    assert!(local["tokens"].as_u64().unwrap() > 0, "{local}");

    let harness = create_harness(HarnessOptions {
        resource_loader: Some(loader),
        ..Default::default()
    })
    .await
    .unwrap();
    harness
        .session
        .bind_extensions(&Default::default())
        .await
        .unwrap();
    let runtime = &harness.model_runtime;
    let model = runtime
        .get_model("subpath-faux", "deep-1")
        .expect("faux model from the deep subpath is registered");
    assert_eq!(model.name, "Deep One");
    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserContent::Text("over the bridge".to_string()),
            timestamp: 0,
            ..Default::default()
        })],
        tools: None,
    };
    let message = runtime.complete_simple(model, context, None).await;
    assert_eq!(
        message.stop_reason,
        StopReason::Stop,
        "{:?}",
        message.error_message
    );
    let text: String = message
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect();
    assert_eq!(text, "deep: over the bridge");
    harness.cleanup();
    let _ = std::fs::remove_dir_all(dir);
}
