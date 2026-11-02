//! Port of `test/suite/agent-session-compaction.test.ts`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use parking_lot::Mutex;
use pi_agent::types::{AgentMessage, AgentToolRef, AgentToolResult, FnAgentTool};
use pi_ai::auth::types::{ApiKeyAuth, ApiKeyResolveInput, AuthResult, ModelAuth, ProviderAuth};
use pi_ai::AnyModel;
use pi_ai::models::{CreateProviderOptions, ProviderApi, ProviderStreams, create_provider};
use pi_ai::providers::faux::{FauxMessageOptions, FauxModelDefinition, FauxResponseStep, faux_assistant_message, faux_text};
use pi_ai::types::{SimpleStreamOptions, StopReason, TranscriptContext};
use pi_coding_agent::core::agent_session::{AgentSessionEvent, AgentSessionEventListener};
use pi_coding_agent::core::compaction::{CompactionResult, estimate_tokens};
use pi_coding_agent::core::extensions::types::{
    CompactionReason, SessionBeforeCompactEvent, SessionBeforeCompactResult,
    SessionCompactFailedEvent, on_event,
};
use pi_coding_agent::core::session_manager::SessionEntry;
use serde_json::{Value, json};

use crate::common::*;
use crate::support::*;

/// An extension answering `session_before_compact` with `summary` (and optional usage/details).
pub fn compactor(summary: &'static str) -> TestExtension {
    compactor_with(summary, None, Some(json!({})))
}

pub fn compactor_with(
    summary: &'static str,
    usage: Option<pi_ai::types::Usage>,
    details: Option<Value>,
) -> TestExtension {
    ext(move |pi| {
        let usage = usage.clone();
        let details = details.clone();
        on_event::<SessionBeforeCompactEvent, _, _>(&*pi, move |event, _ctx| {
            let usage = usage.clone();
            let details = details.clone();
            async move {
                Ok(Some(SessionBeforeCompactResult {
                    cancel: None,
                    compaction: Some(CompactionResult {
                        summary: summary.into(),
                        first_kept_entry_id: event.preparation.first_kept_entry_id.clone(),
                        tokens_before: event.preparation.tokens_before,
                        estimated_tokens_after: None,
                        usage,
                        details,
                    }),
                }))
            }
        })?;
        Ok(())
    })
}

fn compaction_entries(harness: &Harness) -> Vec<SessionEntry> {
    harness
        .session_manager
        .get_entries()
        .into_iter()
        .filter(|e| matches!(e, SessionEntry::Compaction(_)))
        .collect()
}

fn faux_model(id: &str, context_window: u64, max_tokens: Option<u64>) -> FauxModelDefinition {
    FauxModelDefinition {
        id: id.into(),
        context_window: Some(context_window),
        max_tokens,
        ..Default::default()
    }
}

fn settings(value: Value) -> Option<Value> {
    Some(value)
}

fn last_event(harness: &Harness, ty: &str) -> Value {
    harness.events_of_type(ty).last().cloned().unwrap_or(Value::Null)
}

fn large_tool(name: &'static str, size: usize, terminate: bool) -> AgentToolRef {
    FnAgentTool::new(
        name,
        name,
        "Returns enough content to cross the compaction threshold",
        json!({"type": "object", "properties": {}}),
        move |_id, _params, _signal, _update| async move {
            let mut result = AgentToolResult::text(
                format!("large-tool-result:{}", "x".repeat(size)),
                Some(json!({})),
            );
            if terminate {
                result.terminate = Some(true);
            }
            Ok(result)
        },
    )
    .into_ref()
}

/// TS `createAbortableCompactionHarness`.
async fn create_abortable_compaction_harness() -> (Arc<Harness>, tokio::sync::oneshot::Receiver<()>) {
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let tx = Arc::new(Mutex::new(Some(tx)));
    let harness = create_harness(HarnessOptions {
        settings: settings(json!({"compaction": {"keepRecentTokens": 1}})),
        extension_factories: vec![ext(move |pi| {
            let tx = tx.clone();
            on_event::<SessionBeforeCompactEvent, _, _>(&*pi, move |event, _ctx| {
                let tx = tx.clone();
                async move {
                    if let Some(tx) = tx.lock().take() {
                        let _ = tx.send(());
                    }
                    event.signal.cancelled().await;
                    Ok(Some(SessionBeforeCompactResult {
                        cancel: Some(true),
                        compaction: None,
                    }))
                }
            })?;
            Ok(())
        })],
        ..Default::default()
    })
    .await
    .unwrap();
    seed_compactable_session(&harness);
    (Arc::new(harness), rx)
}

/// Replace automatic compaction with a spy returning `false` (TS `vi.spyOn(...).mockResolvedValue(false)`).
fn spy_run_auto_compaction(harness: &Harness) -> Arc<Mutex<Vec<(CompactionReason, bool)>>> {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let c = calls.clone();
    harness
        .session
        .set_run_auto_compaction_override(Some(Arc::new(move |reason, will_retry| {
            c.lock().push((reason, will_retry));
            false
        })));
    calls
}

fn record_compaction_errors(harness: &Harness) -> Arc<Mutex<Vec<String>>> {
    let errors = Arc::new(Mutex::new(Vec::new()));
    let e = errors.clone();
    let listener: AgentSessionEventListener = Arc::new(move |event| {
        if let AgentSessionEvent::CompactionEnd {
            error_message: Some(message),
            ..
        } = &event
        {
            e.lock().push(message.clone());
        }
    });
    harness.session.subscribe(&listener);
    errors
}

#[tokio::test]
async fn manually_compacts_using_an_extension_provided_summary() {
    let summary_usage: pi_ai::types::Usage = serde_json::from_value(json!({
        "input": 10, "output": 20, "cacheRead": 30, "cacheWrite": 40, "totalTokens": 100,
        "cost": { "input": 0.1, "output": 0.2, "cacheRead": 0.3, "cacheWrite": 0.4, "total": 1 }
    }))
    .unwrap();
    let harness = create_harness(HarnessOptions {
        settings: settings(json!({"compaction": {"keepRecentTokens": 1}})),
        extension_factories: vec![compactor_with(
            "summary from extension",
            Some(summary_usage.clone()),
            Some(json!({"source": "extension"})),
        )],
        ..Default::default()
    })
    .await
    .unwrap();

    harness.session.prompt("one", None).await.unwrap();
    harness.session.prompt("two", None).await.unwrap();
    let stats_before = harness.session.get_session_stats();

    let result = harness.session.compact(None).await.unwrap();
    let entries = compaction_entries(&harness);
    let estimated_after: u64 = harness.session.messages().iter().map(estimate_tokens).sum();

    assert_eq!(result.summary, "summary from extension");
    assert_eq!(result.usage.as_ref(), Some(&summary_usage));
    assert_eq!(result.estimated_tokens_after, Some(estimated_after));
    assert_eq!(entries.len(), 1);
    if let SessionEntry::Compaction(entry) = &entries[0] {
        assert_eq!(entry.usage.as_ref(), Some(&summary_usage));
    }
    let stats_after = harness.session.get_session_stats();
    assert_eq!(stats_after.tokens.input, stats_before.tokens.input + 10.0);
    assert_eq!(stats_after.tokens.output, stats_before.tokens.output + 20.0);
    assert_eq!(stats_after.tokens.cache_read, stats_before.tokens.cache_read + 30.0);
    assert_eq!(stats_after.tokens.cache_write, stats_before.tokens.cache_write + 40.0);
    assert_eq!(stats_after.cost, stats_before.cost + 1.0);
    let messages = harness.session.messages();
    assert_eq!(messages[0].role(), "system");
    assert_eq!(messages[1].role(), "compactionSummary");
    harness.cleanup();
}

#[tokio::test]
async fn checkpoints_the_replayed_system_state_and_folds_summarized_and_retained_system_patches_into_it() {
    let harness = create_harness(HarnessOptions::default()).await.unwrap();
    harness.set_responses(vec![text("declared")]);
    harness.session.prompt("declare the prompt", None).await.unwrap();
    let declared = match &harness.session.messages()[0] {
        AgentMessage::System(m) => m.clone(),
        other => panic!("expected declared system message, got {other:?}"),
    };

    let sm = &harness.session_manager;
    sm.append_message(agent_message(json!({
        "role": "system",
        "content": "summarized instruction",
        "sections": {"early": "<early>1</early>"},
        "toolsRemoved": [{"name": "bash"}],
        "timestamp": now_ms(),
    })))
    .unwrap();
    let first_kept = sm.append_message(user_blocks("kept before patch", now_ms())).unwrap();
    sm.append_message(agent_message(json!({
        "role": "system",
        "content": "retained instruction",
        "sections": {"extra": "<extra>late</extra>"},
        "toolsRemoved": [{"name": "read"}],
        "timestamp": now_ms(),
    })))
    .unwrap();
    sm.append_message(user_blocks("kept after patch", now_ms())).unwrap();
    sm.append_compaction("compacted", Some(&first_kept), 100, None, None, None).unwrap();

    let messages = sm.build_session_context().messages;
    let roles: Vec<_> = messages.iter().map(|m| m.role().to_string()).collect();
    assert_eq!(roles, ["system", "compactionSummary", "user", "user"]);
    let AgentMessage::System(checkpoint) = &messages[0] else {
        panic!("expected checkpoint system message");
    };
    assert_eq!(
        serde_json::to_value(&checkpoint.content).unwrap(),
        json!("summarized instruction\n\nretained instruction")
    );
    let mut expected_sections = declared.sections.clone().unwrap_or_default();
    expected_sections.insert("early".into(), Some("<early>1</early>".into()));
    expected_sections.insert("extra".into(), Some("<extra>late</extra>".into()));
    assert_eq!(checkpoint.sections.clone().unwrap_or_default(), expected_sections);
    let tools: Vec<String> = checkpoint
        .tools_added
        .clone()
        .unwrap_or_default()
        .iter()
        .map(|t| t.name.clone())
        .collect();
    let expected: Vec<String> = harness
        .session
        .get_active_tool_names()
        .into_iter()
        .filter(|n| n != "read" && n != "bash")
        .collect();
    assert_eq!(tools, expected);
    harness.cleanup();
}

#[tokio::test]
async fn allows_a_queued_prompt_to_start_when_manual_compaction_ends() {
    let harness = Arc::new(
        create_harness(HarnessOptions {
            settings: settings(json!({"compaction": {"keepRecentTokens": 1}})),
            extension_factories: vec![compactor("manual compacted")],
            ..Default::default()
        })
        .await
        .unwrap(),
    );
    seed_compactable_session(&harness);
    harness.set_responses(vec![text("queued response")]);

    type Run = tokio::task::JoinHandle<anyhow::Result<()>>;
    let queued: Arc<Mutex<Option<Run>>> = Arc::new(Mutex::new(None));
    let compacting_at_end = Arc::new(Mutex::new(None::<bool>));
    let (q, c, weak) = (queued.clone(), compacting_at_end.clone(), Arc::downgrade(&harness.session));
    let listener: AgentSessionEventListener = Arc::new(move |event| {
        if let AgentSessionEvent::CompactionEnd {
            reason: CompactionReason::Manual,
            result: Some(_),
            ..
        } = &event
            && let Some(session) = weak.upgrade()
        {
            *c.lock() = Some(session.is_compacting());
            *q.lock() = Some(tokio::spawn(async move {
                session.prompt("queued after compaction", None).await
            }));
        }
    });
    harness.session.subscribe(&listener);

    harness.session.compact(None).await.unwrap();
    let run = queued.lock().take().expect("compaction_end did not start the queued prompt");
    run.await.unwrap().unwrap();

    assert_eq!(*compacting_at_end.lock(), Some(false));
    assert!(harness.user_texts().contains(&"queued after compaction".to_string()));
    assert_eq!(harness.session.get_last_assistant_text().as_deref(), Some("queued response"));
    harness.cleanup();
}

/// TS sets `agent.state.model = undefined` and expects "No model selected"; pi-agent's model is
/// not optional, so that state cannot exist in Rust. Without a selected model the agent holds the
/// "unknown" placeholder (as TS sessions do), and compaction proceeds past the model check.
#[tokio::test]
async fn compacting_with_the_placeholder_model_passes_the_model_check() {
    let harness = create_harness(HarnessOptions::default()).await.unwrap();
    harness.session.agent().set_model(pi_agent::default_model());

    let error = harness.session.compact(None).await.unwrap_err();
    assert!(error.to_string().contains("Nothing to compact (session too small)"), "{error}");
    harness.cleanup();
}

#[tokio::test]
async fn throws_when_compacting_without_configured_auth() {
    let harness = create_harness(HarnessOptions {
        with_configured_auth: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();
    // Auth is resolved only when Pi summarizes itself, after checking there is something to compact.
    seed_compactable_session(&harness);

    let error = harness.session.compact(None).await.unwrap_err();
    let expected = format!("No API key found for {}.", harness.get_model().provider);
    assert!(error.to_string().contains(&expected), "{error}");
    harness.cleanup();
}

#[tokio::test]
async fn manually_compacts_with_a_custom_stream_fn_when_registry_auth_is_absent() {
    let harness = create_harness(HarnessOptions {
        with_configured_auth: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();
    seed_compactable_session(&harness);
    let calls = use_summary_stream_fn(&harness, "summary from custom stream", None);

    let result = harness.session.compact(None).await.unwrap();

    assert!(result.summary.contains("summary from custom stream"));
    assert_eq!(*calls.lock(), 1);
    harness.cleanup();
}

struct BearerAuth;

/// TS `stream: () => createAssistantMessageEventStream()`: a stream that never ends.
pub struct IdleStreams;

impl ProviderStreams for IdleStreams {
    fn stream(
        &self,
        _model: &pi_ai::types::Model,
        _context: &TranscriptContext,
        _options: pi_ai::types::StreamOptions,
    ) -> pi_ai::AssistantMessageEventStream {
        pi_ai::AssistantMessageEventStream::new().1
    }
    fn stream_simple(
        &self,
        _model: &pi_ai::types::Model,
        _context: &TranscriptContext,
        _options: pi_ai::types::StreamOptions,
    ) -> pi_ai::AssistantMessageEventStream {
        pi_ai::AssistantMessageEventStream::new().1
    }
}

#[async_trait::async_trait]
impl ApiKeyAuth for BearerAuth {
    fn name(&self) -> &str {
        "Faux bearer token"
    }
    async fn resolve(&self, _input: ApiKeyResolveInput) -> anyhow::Result<Option<AuthResult>> {
        let mut headers = pi_ai::types::ProviderHeaders::new();
        headers.insert("Authorization".into(), Some("Bearer ambient-token".into()));
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: None,
                headers: Some(headers),
                base_url: None,
            },
            env: None,
            source: Some("ambient bearer token".into()),
        }))
    }
}

#[tokio::test]
async fn manually_compacts_with_provider_resolved_bearer_auth() {
    let harness = create_harness(HarnessOptions {
        with_configured_auth: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();
    let model = harness.get_model();
    let provider = create_provider(CreateProviderOptions {
        id: model.provider.clone(),
        name: Some("Faux bearer provider".into()),
        auth: ProviderAuth {
            api_key: Some(Arc::new(BearerAuth)),
            oauth: None,
        },
        models: harness.models.iter().cloned().map(AnyModel::Chat).collect(),
        api: Some(ProviderApi::Single(Arc::new(IdleStreams))),
        ..Default::default()
    })
    .unwrap();
    harness.model_runtime.register_native_provider(provider).unwrap();
    seed_compactable_session(&harness);
    let checked = Arc::new(Mutex::new(None::<(Option<String>, Value)>));
    let ch = checked.clone();
    harness.set_responses(vec![FauxResponseStep::factory(move |_context, options, _s, _m| {
        *ch.lock() = Some((
            options.api_key.clone(),
            serde_json::to_value(&options.headers).unwrap(),
        ));
        faux_text_message("summary with bearer auth")
    })]);

    let result = harness.session.compact(None).await.unwrap();

    assert!(result.summary.contains("summary with bearer auth"));
    assert_eq!(harness.faux.state().call_count, 1);
    let (api_key, headers) = checked.lock().clone().unwrap();
    assert_eq!(api_key, None);
    assert_eq!(headers, json!({"Authorization": "Bearer ambient-token"}));
    harness.cleanup();
}

#[tokio::test]
async fn uses_the_standalone_compaction_request_context() {
    let harness = create_harness(HarnessOptions {
        settings: settings(json!({"compaction": {"keepRecentTokens": 1}})),
        ..Default::default()
    })
    .await
    .unwrap();
    seed_compactable_session(&harness);

    let transform_calls = Arc::new(AtomicUsize::new(0));
    let t = transform_calls.clone();
    let agent = harness.session.agent();
    agent.set_transform_context(Some(Arc::new(move |messages, _signal| {
        t.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { messages })
    })));
    agent.set_session_id(Some("active-routing-session".into()));
    agent.set_transport(pi_ai::types::Transport::Websocket);

    let captured = Arc::new(Mutex::new(None::<(TranscriptContext, SimpleStreamOptions)>));
    let c = captured.clone();
    use_summary_stream_fn(
        &harness,
        "standalone summary",
        Some(Arc::new(move |context, options| {
            *c.lock() = Some((context.clone(), options.clone()));
        })),
    );

    harness.session.compact(None).await.unwrap();

    assert_eq!(transform_calls.load(Ordering::SeqCst), 0);
    let (context, options) = captured.lock().clone().unwrap();
    assert_ne!(
        pi_ai::get_current_system_prompt(&context.messages),
        harness.session.agent().system_prompt()
    );
    assert!(pi_ai::utils::transcript::get_current_tools(&context.messages).is_empty());
    // Regression test for #9652: split-turn summaries use a clear Markdown conversation boundary.
    assert!(
        serde_json::to_string(&context.messages)
            .unwrap()
            .contains("# Conversation\\n[User]: message to compact")
    );
    assert_eq!(
        serde_json::to_value(options.cache_retention).unwrap(),
        json!("none")
    );
    assert_ne!(options.session_id.as_deref(), Some("active-routing-session"));
    assert!(options.transport.is_none());
    harness.cleanup();
}

#[tokio::test]
async fn persists_usage_from_pi_generated_manual_compaction() {
    let harness = create_harness(HarnessOptions {
        with_configured_auth: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();
    seed_compactable_session(&harness);
    use_summary_stream_fn(&harness, "summary from custom stream", None);

    let result = harness.session.compact(None).await.unwrap();

    let entries = compaction_entries(&harness);
    assert_eq!(result.usage, Some(usage(10)));
    assert_eq!(entries.len(), 1);
    let SessionEntry::Compaction(entry) = &entries[0] else {
        unreachable!()
    };
    assert_eq!(entry.usage, Some(usage(10)));
    harness.cleanup();
}

#[tokio::test]
async fn auto_compacts_with_a_custom_stream_fn_when_registry_auth_is_absent() {
    let harness = create_harness(HarnessOptions {
        with_configured_auth: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();
    seed_compactable_session(&harness);
    let calls = use_summary_stream_fn(&harness, "auto summary from custom stream", None);

    harness
        .session
        .run_auto_compaction_for_test(CompactionReason::Threshold, false)
        .await;

    assert_eq!(compaction_entries(&harness).len(), 1);
    let end = last_event(&harness, "compaction_end");
    assert!(end["result"]["estimatedTokensAfter"].as_u64().unwrap() > 0);
    assert_eq!(*calls.lock(), 1);
    harness.cleanup();
}

#[tokio::test]
async fn notifies_extensions_when_auto_compaction_fails() {
    let failed = Arc::new(Mutex::new(Vec::<SessionCompactFailedEvent>::new()));
    let f = failed.clone();
    let harness = create_harness(HarnessOptions {
        extension_factories: vec![ext(move |pi| {
            let f = f.clone();
            on_event::<SessionCompactFailedEvent, _, _>(&*pi, move |event, _ctx| {
                let f = f.clone();
                async move {
                    f.lock().push(event);
                    Ok(None)
                }
            })?;
            Ok(())
        })],
        ..Default::default()
    })
    .await
    .unwrap();
    seed_compactable_session(&harness);
    harness.session.agent().set_stream_fn(Some(Arc::new(|_model, _context, _options| {
        Box::pin(async { Err(anyhow::anyhow!("summary generator blew up")) })
    })));

    let retry = harness
        .session
        .run_auto_compaction_for_test(CompactionReason::Threshold, false)
        .await;
    assert!(!retry);

    let end = last_event(&harness, "compaction_end");
    assert_eq!(end["reason"], json!("threshold"));
    assert_eq!(end["aborted"], json!(false));
    assert_eq!(end["willRetry"], json!(false));
    assert_eq!(end["errorMessage"], json!("Auto-compaction failed: summary generator blew up"));
    let failed = failed.lock();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].reason, CompactionReason::Threshold);
    assert!(!failed[0].aborted);
    assert!(!failed[0].will_retry);
    assert!(!failed[0].from_extension);
    assert_eq!(
        failed[0].error_message.as_deref(),
        Some("Auto-compaction failed: summary generator blew up")
    );
    harness.cleanup();
}

#[tokio::test]
async fn compacts_and_resumes_after_a_length_stop_below_the_desired_output_limit() {
    let harness = create_harness(HarnessOptions {
        models: vec![faux_model("faux-1", 1000, Some(100))],
        settings: settings(json!({"compaction": {"keepRecentTokens": 1, "reserveTokens": 0}})),
        extension_factories: vec![compactor("overflow compacted")],
        ..Default::default()
    })
    .await
    .unwrap();
    harness.set_responses(vec![
        message(vec![faux_text("partial response")], Some(StopReason::Length)).into(),
        text("completed response"),
    ]);

    harness.session.prompt(&"x".repeat(5000), None).await.unwrap();

    assert_eq!(harness.faux.state().call_count, 2);
    let end = last_event(&harness, "compaction_end");
    assert_eq!(end["reason"], json!("overflow"));
    assert_eq!(end["aborted"], json!(false));
    assert_eq!(end["willRetry"], json!(true));
    assert_eq!(harness.session.get_last_assistant_text().as_deref(), Some("completed response"));
    harness.cleanup();
}

/// The `errorMessage` pi-ai builds from llama.cpp's real `exceed_context_size_error` 400 body
/// (asserted end to end in `pi-ai/tests/openai_completions.rs`).
const LLAMA_CPP_OVERFLOW_ERROR: &str = r#"400: {"code":400,"message":"the request exceeds the available context size, try increasing it","type":"exceed_context_size_error","n_prompt_tokens":9000,"n_ctx":8192}"#;

/// LiteLLM's pre-call check (#11494), as a local proxy in front of llama.cpp reports it.
const LITELLM_OVERFLOW_ERROR: &str = "400: {\"message\":\"litellm.ContextWindowExceededError: litellm.BadRequestError: litellm._pre_call_checks: Context Window exceeded for given call. No models have context window large enough for this call.\"}";

#[tokio::test]
async fn compacts_and_retries_after_a_local_server_context_overflow_error() {
    for overflow_error in [LLAMA_CPP_OVERFLOW_ERROR, LITELLM_OVERFLOW_ERROR] {
        let harness = create_harness(HarnessOptions {
            models: vec![faux_model("faux-1", 8192, Some(100))],
            settings: settings(json!({"compaction": {"keepRecentTokens": 1, "reserveTokens": 0}})),
            extension_factories: vec![compactor("overflow compacted")],
            ..Default::default()
        })
        .await
        .unwrap();
        harness.set_responses(vec![
            text("first answer"),
            faux_assistant_message(
                vec![faux_text("")],
                FauxMessageOptions {
                    stop_reason: Some(StopReason::Error),
                    error_message: Some(overflow_error.into()),
                    ..Default::default()
                },
            )
            .into(),
            text("answer after compaction"),
        ]);

        harness.session.prompt(&"x".repeat(2000), None).await.unwrap();
        harness.session.prompt(&"y".repeat(2000), None).await.unwrap();

        assert_eq!(harness.faux.state().call_count, 3, "{overflow_error}");
        let end = last_event(&harness, "compaction_end");
        assert_eq!(end["reason"], json!("overflow"), "{overflow_error}");
        assert_eq!(end["aborted"], json!(false), "{overflow_error}");
        assert_eq!(end["willRetry"], json!(true), "{overflow_error}");
        assert_eq!(compaction_entries(&harness).len(), 1, "{overflow_error}");
        assert_eq!(harness.session.get_last_assistant_text().as_deref(), Some("answer after compaction"));
        // The failed overflow response is not replayed to the model on the retry.
        let replayed_error = harness.session.messages().iter().any(|message| {
            matches!(message, AgentMessage::Assistant(a) if a.stop_reason == StopReason::Error)
        });
        assert!(!replayed_error, "{overflow_error}");
        harness.cleanup();
    }
}

// Regression coverage for #8133 (model overrides between assistant turns) and #9740 (an oversized
// trailing tool result must still produce a cut point).
async fn compacts_after_an_oversized_tool_result(model_override: bool) {
    let tool_result = format!("large-tool-result:{}", "x".repeat(8000));
    let order = Arc::new(Mutex::new(Vec::<String>::new()));
    let observed = Arc::new(Mutex::new(Vec::<Value>::new()));
    let compaction_settings = if model_override {
        json!({
            "enabled": true,
            "reserveTokens": 0,
            "keepRecentTokens": 20000,
            "modelOverrides": {"faux/faux-1": {"reserveTokens": 400, "keepRecentTokens": 1750}}
        })
    } else {
        json!({"enabled": true, "reserveTokens": 400, "keepRecentTokens": 1750})
    };
    let (o, obs) = (order.clone(), observed.clone());
    let tr = tool_result.clone();
    let harness = create_harness(HarnessOptions {
        models: vec![faux_model("faux-1", 2600, Some(100))],
        settings: settings(json!({"compaction": compaction_settings})),
        tools: Some(vec![FnAgentTool::new(
            "large_result",
            "Large result",
            "Returns enough content to cross the compaction threshold",
            json!({"type": "object", "properties": {}}),
            move |_id, _p, _s, _u| {
                let tr = tr.clone();
                async move { Ok(AgentToolResult::text(tr, Some(json!({})))) }
            },
        )
        .into_ref()]),
        extension_factories: vec![ext(move |pi| {
            let (o, obs) = (o.clone(), obs.clone());
            on_event::<SessionBeforeCompactEvent, _, _>(&*pi, move |event, _ctx| {
                let (o, obs) = (o.clone(), obs.clone());
                async move {
                    o.lock().push("compaction".into());
                    obs.lock()
                        .push(serde_json::to_value(&event.preparation.settings).unwrap());
                    Ok(Some(SessionBeforeCompactResult {
                        cancel: None,
                        compaction: Some(CompactionResult {
                            summary: "compacted history".into(),
                            first_kept_entry_id: event.preparation.first_kept_entry_id.clone(),
                            tokens_before: event.preparation.tokens_before,
                            estimated_tokens_after: None,
                            usage: None,
                            details: Some(json!({})),
                        }),
                    }))
                }
            })?;
            Ok(())
        })],
        ..Default::default()
    })
    .await
    .unwrap();
    let resumed = Arc::new(Mutex::new(String::new()));
    let (o, r) = (order.clone(), resumed.clone());
    harness.set_responses(vec![
        text(&format!("old-history:{}", "a".repeat(800))),
        text(&format!("recent-history:{}", "b".repeat(800))),
        tool_use("large_result", json!({})),
        respond_with(move |context| {
            o.lock().push("provider".into());
            *r.lock() = serde_json::to_string(&context.messages).unwrap();
            faux_text_message("finished after compaction")
        }),
    ]);

    harness.session.prompt("seed old history", None).await.unwrap();
    harness.session.prompt("seed recent history", None).await.unwrap();
    let agent_starts_before = harness.events_of_type("agent_start").len();
    harness.session.prompt("run the large tool", None).await.unwrap();

    assert_eq!(order.lock()[..2], ["compaction".to_string(), "provider".to_string()]);
    assert_eq!(
        observed.lock()[0],
        json!({"enabled": true, "reserveTokens": 400, "keepRecentTokens": 1750})
    );
    assert_eq!(harness.events_of_type("agent_start").len(), agent_starts_before + 1);
    assert_eq!(
        last_event(&harness, "compaction_start"),
        json!({"type": "compaction_start", "reason": "threshold"})
    );
    let resumed = resumed.lock().clone();
    assert!(resumed.contains("compacted history"));
    assert!(resumed.contains("large-tool-result"));
    assert_eq!(
        harness.session.get_last_assistant_text().as_deref(),
        Some("finished after compaction")
    );
    harness.cleanup();
}

#[tokio::test]
async fn compacts_after_an_oversized_tool_result_in_the_same_run() {
    compacts_after_an_oversized_tool_result(false).await;
}

#[tokio::test]
async fn compacts_after_an_oversized_tool_result_in_the_same_run_with_model_override() {
    compacts_after_an_oversized_tool_result(true).await;
}

#[tokio::test]
async fn includes_steering_queued_during_compaction_in_the_resumed_assistant_request() {
    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    let started_tx = Arc::new(Mutex::new(Some(started_tx)));
    let release = Arc::new(tokio::sync::Notify::new());
    let rel = release.clone();
    let harness = Arc::new(
        create_harness(HarnessOptions {
            models: vec![faux_model("faux-1", 2600, Some(100))],
            settings: settings(json!({"compaction": {"enabled": true, "reserveTokens": 400, "keepRecentTokens": 1750}})),
            tools: Some(vec![large_tool("large_result", 6800, false)]),
            extension_factories: vec![ext(move |pi| {
                let (tx, rel) = (started_tx.clone(), rel.clone());
                on_event::<SessionBeforeCompactEvent, _, _>(&*pi, move |event, _ctx| {
                    let (tx, rel) = (tx.clone(), rel.clone());
                    async move {
                        if let Some(tx) = tx.lock().take() {
                            let _ = tx.send(());
                        }
                        rel.notified().await;
                        Ok(Some(SessionBeforeCompactResult {
                            cancel: None,
                            compaction: Some(CompactionResult {
                                summary: "compacted history".into(),
                                first_kept_entry_id: event.preparation.first_kept_entry_id.clone(),
                                tokens_before: event.preparation.tokens_before,
                                estimated_tokens_after: None,
                                usage: None,
                                details: Some(json!({})),
                            }),
                        }))
                    }
                })?;
                Ok(())
            })],
            ..Default::default()
        })
        .await
        .unwrap(),
    );
    let resumed = Arc::new(Mutex::new(String::new()));
    let r = resumed.clone();
    harness.set_responses(vec![
        text(&format!("old-history:{}", "a".repeat(800))),
        text(&format!("recent-history:{}", "b".repeat(800))),
        tool_use("large_result", json!({})),
        respond_with(move |context| {
            *r.lock() = serde_json::to_string(&context.messages).unwrap();
            faux_text_message("finished after compaction")
        }),
        text("finished after delayed steering"),
    ]);

    harness.session.prompt("seed old history", None).await.unwrap();
    harness.session.prompt("seed recent history", None).await.unwrap();
    let session = harness.session.clone();
    let run = tokio::spawn(async move { session.prompt("run the large tool", None).await });
    started_rx.await.unwrap();
    harness.session.steer("change direction", None, None).await.unwrap();
    release.notify_one();
    run.await.unwrap().unwrap();

    assert!(resumed.lock().contains("change direction"));
    assert_eq!(harness.faux.state().call_count, 4);
    harness.cleanup();
}

#[tokio::test]
async fn does_not_compact_after_a_terminating_tool_result() {
    let harness = create_harness(HarnessOptions {
        models: vec![faux_model("faux-1", 2600, Some(100))],
        settings: settings(json!({"compaction": {"enabled": true, "reserveTokens": 400, "keepRecentTokens": 1750}})),
        tools: Some(vec![large_tool("terminate_with_large_result", 6800, true)]),
        extension_factories: vec![compactor("unexpected compaction")],
        ..Default::default()
    })
    .await
    .unwrap();
    harness.set_responses(vec![
        text(&format!("old-history:{}", "a".repeat(800))),
        text(&format!("recent-history:{}", "b".repeat(800))),
        tool_use("terminate_with_large_result", json!({})),
    ]);

    harness.session.prompt("seed old history", None).await.unwrap();
    harness.session.prompt("seed recent history", None).await.unwrap();
    harness.session.prompt("run the terminating tool", None).await.unwrap();

    assert!(harness.events_of_type("compaction_start").is_empty());
    assert!(compaction_entries(&harness).is_empty());
    assert_eq!(harness.get_pending_response_count(), 0);
    harness.cleanup();
}

#[tokio::test]
async fn does_not_compact_when_a_length_stop_reaches_the_desired_output_limit() {
    let harness = create_harness(HarnessOptions {
        models: vec![faux_model("faux-1", 1_000_000, Some(100))],
        ..Default::default()
    })
    .await
    .unwrap();
    harness.set_responses(vec![
        message(vec![faux_text("x".repeat(400))], Some(StopReason::Length)).into(),
    ]);

    harness.session.prompt("hello", None).await.unwrap();

    assert_eq!(harness.faux.state().call_count, 1);
    assert!(harness.events_of_type("compaction_start").is_empty());
    harness.cleanup();
}

#[tokio::test]
async fn stops_after_one_compact_and_retry_when_a_second_response_is_also_truncated() {
    let harness = create_harness(HarnessOptions {
        models: vec![faux_model("faux-1", 1_000_000, Some(100))],
        settings: settings(json!({"compaction": {"keepRecentTokens": 1, "reserveTokens": 0}})),
        extension_factories: vec![compactor("overflow compacted")],
        ..Default::default()
    })
    .await
    .unwrap();
    let truncated = |text: &'static str| {
        respond_with(move |_| {
            let mut m = message(vec![faux_text(text.repeat(64))], Some(StopReason::Length));
            m.timestamp = now_ms() + 10_000;
            m
        })
    };
    harness.set_responses(vec![truncated("x"), truncated("y")]);

    harness.session.prompt(&"x".repeat(5000), None).await.unwrap();

    assert_eq!(harness.faux.state().call_count, 2);
    let overflow_starts = harness
        .events_of_type("compaction_start")
        .iter()
        .filter(|e| e["reason"] == json!("overflow"))
        .count();
    assert_eq!(overflow_starts, 1);
    assert_eq!(
        last_event(&harness, "compaction_end")["errorMessage"],
        json!("Truncated response recovery failed after one compact-and-retry attempt.")
    );
    harness.cleanup();
}

const OVERFLOW_FAILED: &str = "Context overflow recovery failed after one compact-and-retry attempt. Try reducing context or switching to a larger-context model.";

#[tokio::test]
async fn keeps_overflow_wording_when_a_repeated_length_stop_fills_the_context_window() {
    let harness = create_harness(HarnessOptions {
        models: vec![faux_model("faux-1", 100, Some(100))],
        ..Default::default()
    })
    .await
    .unwrap();
    let length_overflow = create_assistant(&harness, StopReason::Length, None, 100, now_ms());
    let spy = spy_run_auto_compaction(&harness);
    let errors = record_compaction_errors(&harness);

    harness.session.check_compaction_for_test(&length_overflow, true).await;
    let mut second = length_overflow.clone();
    second.timestamp = now_ms() + 1;
    harness.session.check_compaction_for_test(&second, true).await;

    assert_eq!(spy.lock().len(), 1);
    assert!(errors.lock().contains(&OVERFLOW_FAILED.to_string()));
    harness.cleanup();
}

#[tokio::test]
async fn cancels_in_progress_manual_compaction_when_abort_compaction_is_called() {
    let (harness, started) = create_abortable_compaction_harness().await;
    let session = harness.session.clone();
    let compact = tokio::spawn(async move { session.compact(None).await });
    started.await.unwrap();
    harness.session.abort_compaction();

    let error = compact.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("Compaction cancelled"), "{error}");
    harness.cleanup();
}

// Regression test for #8920.
#[tokio::test]
async fn aborts_an_in_progress_manual_compaction_and_waits_until_the_session_is_idle() {
    let (harness, started) = create_abortable_compaction_harness().await;
    harness.set_responses(vec![text("continued")]);
    let session = harness.session.clone();
    let compact = tokio::spawn(async move { session.compact(None).await });
    started.await.unwrap();
    harness.session.abort().await.unwrap();
    let error = compact.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("Compaction cancelled"), "{error}");

    let end = last_event(&harness, "compaction_end");
    assert_eq!(end["reason"], json!("manual"));
    assert_eq!(end["aborted"], json!(true));
    assert!(!harness.session.is_compacting());
    assert!(harness.session.is_idle());

    harness.session.prompt("next prompt", None).await.unwrap();
    assert_eq!(harness.session.get_last_assistant_text().as_deref(), Some("continued"));
    harness.cleanup();
}

#[tokio::test]
async fn resumes_after_threshold_compaction_when_only_agent_level_queued_messages_exist() {
    let harness = create_harness(HarnessOptions {
        settings: settings(json!({"compaction": {"keepRecentTokens": 1}})),
        extension_factories: vec![compactor("auto compacted")],
        ..Default::default()
    })
    .await
    .unwrap();
    harness.set_responses(vec![text("one"), text("two")]);
    harness.session.prompt("first", None).await.unwrap();
    harness.session.prompt("second", None).await.unwrap();

    harness.session.agent().follow_up(agent_message(json!({
        "role": "custom",
        "customType": "test",
        "content": [{"type": "text", "text": "queued custom"}],
        "display": false,
        "timestamp": now_ms(),
    })));

    assert!(
        harness
            .session
            .run_auto_compaction_for_test(CompactionReason::Threshold, false)
            .await
    );
    harness.cleanup();
}

#[tokio::test]
async fn does_not_retry_overflow_recovery_more_than_once() {
    let harness = create_harness(HarnessOptions::default()).await.unwrap();
    let overflow = create_assistant(&harness, StopReason::Error, Some("prompt is too long"), 0, now_ms());
    let spy = spy_run_auto_compaction(&harness);
    let errors = record_compaction_errors(&harness);

    harness.session.check_compaction_for_test(&overflow, true).await;
    let mut second = overflow.clone();
    second.timestamp = now_ms() + 1;
    harness.session.check_compaction_for_test(&second, true).await;

    assert_eq!(spy.lock().len(), 1);
    assert!(errors.lock().contains(&OVERFLOW_FAILED.to_string()));
    harness.cleanup();
}

#[tokio::test]
async fn compacts_successful_overflow_responses_without_retrying() {
    let harness = create_harness(HarnessOptions {
        settings: settings(json!({"compaction": {"enabled": true, "keepRecentTokens": 1, "reserveTokens": 0}})),
        models: vec![faux_model("faux-1", 1, Some(100))],
        extension_factories: vec![compactor("successful overflow compacted")],
        ..Default::default()
    })
    .await
    .unwrap();
    harness.set_responses(vec![text("completed answer")]);

    harness.session.prompt("hello", None).await.unwrap();

    let end = last_event(&harness, "compaction_end");
    assert_eq!(end["reason"], json!("overflow"));
    assert_eq!(end["aborted"], json!(false));
    assert_eq!(end["willRetry"], json!(false));
    assert_eq!(harness.faux.state().call_count, 1);
    harness.cleanup();
}

#[tokio::test]
async fn ignores_stale_pre_compaction_assistant_usage_on_pre_prompt_checks() {
    let harness = create_harness(HarnessOptions::default()).await.unwrap();
    let stale_timestamp = now_ms() - 10_000;
    let stale = create_assistant(&harness, StopReason::Stop, None, 610_000, stale_timestamp);
    let sm = &harness.session_manager;
    sm.append_message(user_blocks("before compaction", stale_timestamp - 1000)).unwrap();
    sm.append_message(AgentMessage::Assistant(stale.clone())).unwrap();
    let first_kept = sm.get_entries()[0].id().to_string();
    sm.append_compaction("summary", Some(&first_kept), stale.usage.total_tokens, None, Some(false), None)
        .unwrap();
    sm.append_message(user_blocks("after compaction", now_ms())).unwrap();
    let spy = spy_run_auto_compaction(&harness);

    harness.session.check_compaction_for_test(&stale, false).await;

    assert!(spy.lock().is_empty());
    harness.cleanup();
}

#[tokio::test]
async fn triggers_threshold_compaction_for_error_messages_using_the_last_successful_usage() {
    let harness = create_harness(HarnessOptions::default()).await.unwrap();
    let now = now_ms();
    let successful = create_assistant(&harness, StopReason::Stop, None, 190_000, now);
    let error = create_assistant(&harness, StopReason::Error, Some("529 overloaded"), 0, now + 1000);
    harness.session.agent().set_messages(vec![
        user_blocks("hello", now - 1000),
        AgentMessage::Assistant(successful),
        user_blocks("retry", now + 500),
        AgentMessage::Assistant(error.clone()),
    ]);
    let spy = spy_run_auto_compaction(&harness);

    harness.session.check_compaction_for_test(&error, true).await;

    assert_eq!(*spy.lock(), vec![(CompactionReason::Threshold, false)]);
    harness.cleanup();
}

#[tokio::test]
async fn does_not_trigger_threshold_compaction_for_error_messages_when_no_prior_usage_exists() {
    let harness = create_harness(HarnessOptions::default()).await.unwrap();
    let now = now_ms();
    let error = create_assistant(&harness, StopReason::Error, Some("529 overloaded"), 0, now);
    harness
        .session
        .agent()
        .set_messages(vec![user_blocks("hello", now - 1000), AgentMessage::Assistant(error.clone())]);
    let spy = spy_run_auto_compaction(&harness);

    harness.session.check_compaction_for_test(&error, true).await;

    assert!(spy.lock().is_empty());
    harness.cleanup();
}

#[tokio::test]
async fn does_not_trigger_threshold_compaction_when_only_kept_pre_compaction_usage_exists() {
    let harness = create_harness(HarnessOptions::default()).await.unwrap();
    let pre = now_ms() - 10_000;
    let kept = create_assistant(&harness, StopReason::Stop, None, 190_000, pre);
    let sm = &harness.session_manager;
    sm.append_message(user_blocks("before compaction", pre - 1000)).unwrap();
    sm.append_message(AgentMessage::Assistant(kept.clone())).unwrap();
    let first_kept = sm.get_entries()[0].id().to_string();
    sm.append_compaction("summary", Some(&first_kept), kept.usage.total_tokens, None, Some(false), None)
        .unwrap();
    let error = create_assistant(&harness, StopReason::Error, Some("529 overloaded"), 0, now_ms());
    harness.session.agent().set_messages(vec![
        user_blocks("kept user", pre - 1000),
        AgentMessage::Assistant(kept),
        user_blocks("new prompt", now_ms() - 500),
        AgentMessage::Assistant(error.clone()),
    ]);
    let spy = spy_run_auto_compaction(&harness);

    harness.session.check_compaction_for_test(&error, true).await;

    assert!(spy.lock().is_empty());
    harness.cleanup();
}

#[tokio::test]
async fn does_not_trigger_threshold_compaction_below_the_threshold_or_when_disabled() {
    let below = create_harness(HarnessOptions {
        settings: settings(json!({"compaction": {"enabled": true, "reserveTokens": 1000}})),
        models: vec![faux_model("faux-1", 200_000, None)],
        ..Default::default()
    })
    .await
    .unwrap();
    let disabled = create_harness(HarnessOptions {
        settings: settings(json!({"compaction": {"enabled": false}})),
        ..Default::default()
    })
    .await
    .unwrap();
    let below_spy = spy_run_auto_compaction(&below);
    let disabled_spy = spy_run_auto_compaction(&disabled);

    below
        .session
        .check_compaction_for_test(&create_assistant(&below, StopReason::Stop, None, 1_000, now_ms()), true)
        .await;
    disabled
        .session
        .check_compaction_for_test(
            &create_assistant(&disabled, StopReason::Stop, None, 1_000_000, now_ms()),
            true,
        )
        .await;

    assert!(below_spy.lock().is_empty());
    assert!(disabled_spy.lock().is_empty());
    below.cleanup();
    disabled.cleanup();
}

// ---------------------------------------------------------------------------
// `test/agent-session-compaction.test.ts` (an API-key e2e suite in TS) on the faux provider.
// ---------------------------------------------------------------------------

/// Responses: the prompts' replies, then the summaries (with `keepRecentTokens: 1` the last turn is
/// split, so compaction summarizes the history and the turn prefix separately).
async fn e2e_harness(in_memory_responses: Vec<&'static str>) -> Harness {
    let harness = create_harness(HarnessOptions {
        settings: settings(json!({"compaction": {"keepRecentTokens": 1}})),
        ..Default::default()
    })
    .await
    .unwrap();
    harness.set_responses(in_memory_responses.into_iter().map(text).collect());
    harness
}

#[tokio::test]
async fn e2e_triggers_manual_compaction_via_compact() {
    let harness = e2e_harness(vec!["4", "6", "summary of the arithmetic", "turn prefix summary"]).await;
    harness.session.prompt("What is 2+2? Reply with just the number.", None).await.unwrap();
    harness.session.agent().wait_for_idle().await;
    harness.session.prompt("What is 3+3? Reply with just the number.", None).await.unwrap();
    harness.session.agent().wait_for_idle().await;

    let result = harness.session.compact(None).await.unwrap();

    assert!(!result.summary.is_empty());
    assert!(result.tokens_before > 0);
    let roles: Vec<String> = harness
        .session
        .messages()
        .iter()
        .filter(|m| m.role() != "system")
        .map(|m| m.role().to_string())
        .collect();
    assert_eq!(roles.first().map(String::as_str), Some("compactionSummary"));
    harness.cleanup();
}

#[tokio::test]
async fn e2e_maintains_valid_session_state_after_compaction() {
    let harness = e2e_harness(vec!["Paris", "Berlin", "summary", "turn prefix summary", "Rome"]).await;
    harness.session.prompt("What is the capital of France? One word answer.", None).await.unwrap();
    harness.session.prompt("What is the capital of Germany? One word answer.", None).await.unwrap();
    harness.session.compact(None).await.unwrap();

    harness.session.prompt("What is the capital of Italy? One word answer.", None).await.unwrap();
    harness.session.agent().wait_for_idle().await;

    let messages = harness.session.messages();
    assert!(!messages.is_empty());
    assert!(messages.iter().any(|m| m.role() == "assistant"));
    assert_eq!(harness.assistant_texts().last().map(String::as_str), Some("Rome"));
    harness.cleanup();
}

async fn e2e_persists_compaction(harness: &Harness) {
    harness.session.prompt("Say hello", None).await.unwrap();
    harness.session.prompt("Say goodbye", None).await.unwrap();

    harness.session.compact(None).await.unwrap();

    let compactions = compaction_entries(harness);
    assert_eq!(compactions.len(), 1);
    let SessionEntry::Compaction(compaction) = &compactions[0] else {
        unreachable!()
    };
    assert!(!compaction.summary.is_empty());
    assert!(!compaction.first_kept_entry_id.is_empty());
    assert!(compaction.tokens_before > 0);
}

#[tokio::test]
async fn e2e_persists_compaction_to_the_session_file() {
    let temp = std::env::temp_dir().join(format!("pi-compaction-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp).unwrap();
    let cwd = temp.to_string_lossy().into_owned();
    let session_manager = pi_coding_agent::core::session_manager::SessionManager::create(&cwd, Some(&cwd), None).unwrap();
    let harness = create_harness(HarnessOptions {
        settings: settings(json!({"compaction": {"keepRecentTokens": 1}})),
        session_manager: Some(session_manager.clone()),
        ..Default::default()
    })
    .await
    .unwrap();
    harness.set_responses(vec![text("hello"), text("goodbye"), text("summary"), text("turn prefix summary")]);

    e2e_persists_compaction(&harness).await;

    // The compaction entry reached the session file.
    let file = session_manager.get_session_file().unwrap();
    let reopened = pi_coding_agent::core::session_manager::SessionManager::open(&file, None, None).unwrap();
    assert_eq!(
        reopened
            .get_entries()
            .iter()
            .filter(|e| matches!(e, SessionEntry::Compaction(_)))
            .count(),
        1
    );
    harness.cleanup();
    let _ = std::fs::remove_dir_all(&temp);
}

#[tokio::test]
async fn e2e_works_with_no_session_mode() {
    let harness = e2e_harness(vec!["hello", "goodbye", "summary", "turn prefix summary"]).await;
    assert!(harness.session.session_file().is_none());
    e2e_persists_compaction(&harness).await;
    harness.cleanup();
}

#[tokio::test]
async fn e2e_emits_compaction_events_during_manual_compaction() {
    let harness = e2e_harness(vec!["hello", "summary", "turn prefix summary"]).await;
    harness.session.prompt("Say hello", None).await.unwrap();

    harness.session.compact(None).await.unwrap();

    let events: Vec<Value> = harness
        .events
        .lock()
        .iter()
        .filter_map(|e| serde_json::to_value(e).ok())
        .filter(|e| e["type"] == "compaction_start" || e["type"] == "compaction_end")
        .collect();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0], json!({"type": "compaction_start", "reason": "manual"}));
    assert_eq!(
        (&events[1]["type"], &events[1]["reason"], &events[1]["aborted"], &events[1]["willRetry"]),
        (&json!("compaction_end"), &json!("manual"), &json!(false), &json!(false))
    );
    assert!(!harness.events_of_type("message_end").is_empty());
    harness.cleanup();
}
