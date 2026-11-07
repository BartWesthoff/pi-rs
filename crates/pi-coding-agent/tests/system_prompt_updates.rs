//! Port of `packages/coding-agent/test/system-prompt-updates.test.ts`: the structured system
//! prompt is declared once in the transcript, patched by section, and forced prompts are sent but
//! never recorded.

mod support;

use std::sync::Arc;

use indexmap::IndexMap;
use parking_lot::Mutex;
use pi_agent::types::{AgentMessage, AgentToolResult, FnAgentTool};
use pi_ai::providers::faux::FauxResponseStep;
use pi_ai::types::{AssistantMessage, Message, StopReason, SystemMessage, TranscriptContext};
use pi_ai::utils::text::get_system_message_text;
use pi_ai::utils::transcript::{get_current_system_message, get_current_system_prompt};
use pi_coding_agent::core::extensions::types::{
    BeforeAgentStartEventResult, ExtensionAPIRef, ExtensionEvent, ExtensionEventResult, ExtensionFactory,
    ExtensionHandler, ToolDefinition,
};
use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, CreateAgentSessionOptionsNoTools, create_agent_session};
use pi_coding_agent::core::session_manager::{SessionEntry, SessionManager};
use pi_coding_agent::core::settings_manager::SettingsManager;
use pi_coding_agent::core::system_prompt::{
    BuildSystemPromptOptions, build_system_prompt_sections, build_system_prompt_state, diff_system_prompt_sections,
};
use serde_json::{Value, json};
use support::*;

type Requests = Arc<Mutex<Vec<TranscriptContext>>>;

/// Responses that record each provider request before answering.
fn capturing(requests: &Requests, replies: Vec<AssistantMessage>) -> Vec<FauxResponseStep> {
    replies
        .into_iter()
        .map(|reply| {
            let requests = requests.clone();
            FauxResponseStep::factory(move |context, _options, _state, _model| {
                requests.lock().push(context.clone());
                reply.clone()
            })
        })
        .collect()
}

fn to_llm(messages: &[AgentMessage]) -> Vec<Message> {
    messages
        .iter()
        .filter(|m| !matches!(m, AgentMessage::Custom(_)))
        .map(|m| serde_json::from_value(serde_json::to_value(m).unwrap()).unwrap())
        .collect()
}

fn roles(messages: &[AgentMessage]) -> Vec<String> {
    messages.iter().map(|m| m.role().to_string()).collect()
}

fn llm_roles(messages: &[Message]) -> Vec<&'static str> {
    messages.iter().map(|m| m.role()).collect()
}

fn system_messages(messages: &[Message]) -> Vec<Value> {
    messages
        .iter()
        .filter(|m| m.role() == "system")
        .map(|m| serde_json::to_value(m).unwrap())
        .collect()
}

fn tool_names(value: &Value) -> Vec<String> {
    value.as_array().map(|tools| tools.iter().map(|t| t["name"].as_str().unwrap().to_string()).collect()).unwrap_or_default()
}

fn added_names(message: &SystemMessage) -> Vec<String> {
    message.tools_added.iter().flatten().map(|t| t.name.clone()).collect()
}

fn extension(setup: impl Fn(ExtensionAPIRef) -> anyhow::Result<()> + Send + Sync + 'static) -> TestExtension {
    let setup = Arc::new(setup);
    let factory: ExtensionFactory = Arc::new(move |pi| {
        let setup = setup.clone();
        Box::pin(async move { setup(pi) })
    });
    TestExtension::new(factory)
}

fn options(cwd: &str) -> BuildSystemPromptOptions {
    serde_json::from_value(json!({ "cwd": cwd })).unwrap()
}

fn with_sections(sections: &[(&str, &str)]) -> BuildSystemPromptOptions {
    BuildSystemPromptOptions {
        sections: Some(sections.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()),
        ..options("/tmp")
    }
}

fn replayed(sections: IndexMap<String, String>) -> IndexMap<String, Option<String>> {
    sections.into_iter().map(|(k, v)| (k, Some(v))).collect()
}

#[tokio::test]
async fn declares_the_prompt_and_tools_once_and_reuses_them_across_resume() {
    let harness = create_harness(HarnessOptions::default()).await.unwrap();
    harness.set_responses(vec![faux_text_message("first").into(), faux_text_message("second").into()]);
    harness.session.prompt("one", None).await.unwrap();
    harness.session.prompt("two", None).await.unwrap();

    let system_entries = harness
        .session_manager
        .get_entries()
        .into_iter()
        .filter(|entry| matches!(entry, SessionEntry::Message(m) if m.message.role() == "system"))
        .count();
    assert_eq!(system_entries, 1);
    let messages = harness.session.messages();
    assert_eq!(roles(&messages), vec!["system", "user", "assistant", "user", "assistant"]);
    let AgentMessage::System(head) = &messages[0] else { panic!("expected system message") };
    assert_eq!(serde_json::to_value(&head.content).unwrap(), json!(""));
    let section_names: Vec<&str> = head.sections.as_ref().unwrap().keys().map(String::as_str).collect();
    assert_eq!(section_names, vec!["preamble", "tools", "rules", "docs", "cwd"]);
    assert_eq!(added_names(head), vec!["read", "bash", "edit", "write"]);
    assert_eq!(get_system_message_text(head), harness.session.system_prompt());
    harness.cleanup();
}

#[tokio::test]
async fn opens_a_transcript_without_a_system_message_and_declares_the_prompt_on_the_first_request() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_string_lossy().into_owned();
    let session_manager = SessionManager::in_memory(Some(&cwd), None, None).unwrap();
    session_manager
        .append_message(serde_json::from_value::<AgentMessage>(json!({ "role": "user", "content": "existing", "timestamp": 1 })).unwrap())
        .unwrap();
    let created = create_agent_session(Some(&CreateAgentSessionOptions {
        cwd: Some(cwd.clone()),
        agent_dir: Some(format!("{cwd}/agent")),
        model: pi_ai::compat::get_model("anthropic", "claude-sonnet-4-5"),
        settings_manager: Some(SettingsManager::in_memory(None, None)),
        session_manager: Some(session_manager.clone()),
        no_tools: Some(CreateAgentSessionOptionsNoTools::All),
        ..Default::default()
    }))
    .await
    .unwrap();
    // Nothing is synthesized or persisted until a request needs it.
    let messages = created.session.messages();
    assert_eq!(roles(&messages), vec!["user"]);
    assert_eq!(roles(&session_manager.build_session_context().messages), vec!["user"]);
    assert!(get_current_system_message(&to_llm(&messages)).is_none());
    created.session.dispose();
}

#[test]
fn diffs_sections_into_a_patch() {
    let previous = build_system_prompt_sections(&with_sections(&[("plan_mode", "Plan only.")])).unwrap();
    let current = build_system_prompt_sections(&with_sections(&[("plan_mode", "Implementation allowed.")])).unwrap();
    let patch = diff_system_prompt_sections(replayed(previous.clone()), &current).unwrap();
    assert_eq!(
        serde_json::to_value(&patch).unwrap(),
        json!({ "plan_mode": "<plan_mode>\nImplementation allowed.\n</plan_mode>" })
    );
    assert!(diff_system_prompt_sections(replayed(previous.clone()), &previous).is_none());
    let without = build_system_prompt_sections(&options("/tmp")).unwrap();
    let patch = diff_system_prompt_sections(replayed(previous), &without).unwrap();
    assert_eq!(serde_json::to_value(&patch).unwrap(), json!({ "plan_mode": null }));
}

#[test]
fn keeps_the_preamble_untagged_and_replaces_it_like_any_section() {
    let custom = |prompt: &str| BuildSystemPromptOptions { custom_prompt: Some(prompt.into()), ..options("/tmp") };
    let previous = build_system_prompt_sections(&custom("You are A.")).unwrap();
    let current = build_system_prompt_sections(&custom("You are B.")).unwrap();
    assert_eq!(previous["preamble"], "You are A.");
    let patch = diff_system_prompt_sections(replayed(previous), &current).unwrap();
    assert_eq!(serde_json::to_value(&patch).unwrap(), json!({ "preamble": "You are B." }));

    let forced = build_system_prompt_state(&BuildSystemPromptOptions {
        force_system_prompt: Some("Exact prompt.".into()),
        ..options("/tmp")
    })
    .unwrap();
    assert_eq!(serde_json::to_value(&forced).unwrap(), json!({ "content": "Exact prompt." }));
    let state = build_system_prompt_state(&options("/tmp")).unwrap();
    assert_eq!(
        serde_json::to_value(&state).unwrap(),
        json!({ "content": "", "sections": build_system_prompt_sections(&options("/tmp")).unwrap() })
    );
    let err = build_system_prompt_sections(&with_sections(&[("preamble", "x")])).unwrap_err();
    assert!(err.to_string().contains("Invalid system prompt section name"), "{err}");
}

#[tokio::test]
async fn a_forced_prompt_is_sent_as_the_leading_prompt_for_the_run_and_never_recorded() {
    let turn = Arc::new(Mutex::new(0));
    let forcing = extension(move |pi| {
        let turn = turn.clone();
        let handler: ExtensionHandler = Arc::new(move |event, _ctx| {
            let turn = turn.clone();
            Box::pin(async move {
                let ExtensionEvent::BeforeAgentStart(event) = event else { return Ok(None) };
                let current = {
                    let mut turn = turn.lock();
                    *turn += 1;
                    *turn
                };
                let mut result = BeforeAgentStartEventResult::default();
                if current == 3 {
                    let mut options = event.system_prompt_options.clone();
                    options.sections.insert("plan_mode".into(), "Plan only.".into());
                    result.system_prompt_options = Some(options);
                }
                if current == 2 || current == 3 {
                    result.system_prompt = Some("Exact prompt.".into());
                }
                Ok(Some(ExtensionEventResult::BeforeAgentStart(result)))
            })
        });
        pi.on("before_agent_start", handler)?;
        Ok(())
    });
    let harness = create_harness(HarnessOptions { extension_factories: vec![forcing], ..Default::default() }).await.unwrap();
    let requests: Requests = Arc::default();
    harness.set_responses(capturing(
        &requests,
        ["one", "two", "three", "four"].map(faux_text_message).to_vec(),
    ));
    for text in ["one", "two", "three", "four"] {
        harness.session.prompt(text, None).await.unwrap();
    }
    let requests = requests.lock().clone();
    let system: Vec<Vec<Value>> = requests.iter().map(|r| system_messages(&r.messages)).collect();
    // Forced turns collapse to one leading message; the unforced fourth turn passes the recorded
    // head and both plan_mode patches through.
    assert_eq!(system.iter().map(Vec::len).collect::<Vec<_>>(), vec![1, 1, 1, 3]);

    let forced = system[1].last().unwrap().clone();
    assert_eq!(
        forced,
        json!({
            "role": "system",
            "content": "Exact prompt.",
            "toolsAdded": system[0][0]["toolsAdded"],
            "timestamp": system[0][0]["timestamp"],
        })
    );
    assert_eq!(system[2].last().unwrap(), &forced);
    assert_eq!(get_current_system_prompt(&requests[2].messages), "Exact prompt.");
    assert_eq!(llm_roles(&requests[2].messages), vec!["system", "user", "assistant", "user", "assistant", "user"]);

    // The transcript only records the structured sections, never the forced text.
    let messages = harness.session.messages();
    let recorded: Vec<Value> = messages
        .iter()
        .filter_map(|m| match m {
            AgentMessage::System(s) => Some(serde_json::to_value(&s.sections).unwrap()),
            _ => None,
        })
        .collect();
    assert_eq!(
        recorded,
        vec![
            system[0][0]["sections"].clone(),
            json!({ "plan_mode": "<plan_mode>\nPlan only.\n</plan_mode>" }),
            json!({ "plan_mode": null }),
        ]
    );
    assert_eq!(get_current_system_prompt(&to_llm(&messages)), harness.session.system_prompt());
    harness.cleanup();
}

fn register_first_and_second(pi: &ExtensionAPIRef, with_prompt_text: bool) -> anyhow::Result<()> {
    for name in ["first", "second"] {
        let mut tool = ToolDefinition::new(
            name,
            name,
            format!("{name} description"),
            json!({ "type": "object", "properties": {} }),
            move |_id, _params, _signal, _update, _ctx| async move { Ok(AgentToolResult::text(name, Some(json!({})))) },
        );
        if with_prompt_text {
            tool.prompt_snippet = Some(format!("{name} prompt snippet"));
            tool.prompt_guidelines = Some(vec![format!("Use {name} carefully.")]);
        }
        pi.register_tool(tool)?;
    }
    Ok(())
}

// #11495
#[tokio::test]
async fn does_not_patch_the_prompt_when_the_active_tools_are_only_reordered() {
    let harness = create_harness(HarnessOptions::default()).await.unwrap();
    harness.set_responses(vec![faux_text_message("first").into(), faux_text_message("second").into()]);
    harness.session.prompt("one", None).await.unwrap();
    let before = harness.session.get_active_tool_names();
    assert!(before.len() > 1, "needs several active tools to reorder: {before:?}");
    let reversed: Vec<String> = before.iter().rev().cloned().collect();
    harness.session.set_active_tools_by_name(&reversed);
    assert_eq!(harness.session.get_active_tool_names(), before);
    harness.session.prompt("two", None).await.unwrap();
    let system_count = harness.session.messages().iter().filter(|m| m.role() == "system").count();
    assert_eq!(system_count, 1);
    harness.cleanup();
}

#[tokio::test]
async fn set_active_tools_keeps_kept_tools_in_place_and_appends_new_ones() {
    let harness = create_harness(HarnessOptions {
        extension_factories: vec![extension(|pi| register_first_and_second(&pi, false))],
        ..Default::default()
    })
    .await
    .unwrap();
    // Narrowing keeps the current relative order, whatever order the names are given in.
    harness.session.set_active_tools_by_name(&["second".into(), "read".into()]);
    assert_eq!(harness.session.get_active_tool_names(), vec!["read", "second"]);
    // A newly active tool is appended after the kept ones.
    harness.session.set_active_tools_by_name(&["first".into(), "second".into(), "read".into()]);
    assert_eq!(harness.session.get_active_tool_names(), vec!["read", "second", "first"]);
    // Dropping one leaves the others in place.
    harness.session.set_active_tools_by_name(&["first".into(), "read".into()]);
    assert_eq!(harness.session.get_active_tool_names(), vec!["read", "first"]);
    harness.cleanup();
}

#[tokio::test]
async fn set_active_tools_emits_prompt_sections_and_tool_changes_before_the_next_request() {
    let harness = create_harness(HarnessOptions {
        extension_factories: vec![extension(|pi| register_first_and_second(&pi, true))],
        initial_active_tool_names: Some(vec!["first".into()]),
        ..Default::default()
    })
    .await
    .unwrap();
    let requests: Requests = Arc::default();
    harness.set_responses(capturing(
        &requests,
        vec![
            faux_text_message("first"),
            faux_assistant_message(
                vec![faux_tool_call("first", Default::default(), None)],
                FauxMessageOptions { stop_reason: Some(StopReason::ToolUse), ..Default::default() },
            ),
            faux_text_message("second"),
        ],
    ));
    harness.session.prompt("first", None).await.unwrap();
    harness.session.set_active_tools_by_name(&["second".to_string()]);
    harness.session.prompt("second", None).await.unwrap();
    let requests = requests.lock().clone();
    assert_eq!(requests.len(), 3);

    let keys: Vec<String> = serde_json::to_value(&requests[0]).unwrap().as_object().unwrap().keys().cloned().collect();
    assert_eq!(keys, vec!["messages"]);
    let initial = serde_json::to_value(&requests[0].messages[0]).unwrap();
    assert_eq!(initial["role"], "system");
    assert_eq!(tool_names(&initial["toolsAdded"]), vec!["first", "second"]);
    assert!(initial["sections"]["tools"].as_str().unwrap().contains("first prompt snippet"));

    let update = system_messages(&requests[1].messages).last().unwrap().clone();
    let mut update_keys: Vec<&str> = update.as_object().unwrap().keys().map(String::as_str).collect();
    update_keys.sort();
    assert_eq!(update_keys, vec!["content", "role", "sections", "timestamp", "toolsRemoved"]);
    assert_eq!(update["content"], "");
    assert!(update["timestamp"].is_number());
    assert_eq!(update["toolsRemoved"], json!([{ "name": "first" }]));
    let mut section_keys: Vec<&str> = update["sections"].as_object().unwrap().keys().map(String::as_str).collect();
    section_keys.sort();
    assert_eq!(section_keys, vec!["rules", "tools"]);
    let tools = update["sections"]["tools"].as_str().unwrap();
    assert!(tools.contains("second prompt snippet"));
    assert!(!tools.contains("first prompt snippet"));
    assert!(!update["sections"]["rules"].as_str().unwrap().contains("Use first carefully."));

    let result = requests[2].messages.iter().rev().find(|m| m.role() == "toolResult").unwrap();
    let result = serde_json::to_value(result).unwrap();
    assert_eq!(result["toolName"], "first");
    assert_eq!(result["isError"], true);

    let current = get_current_system_message(&to_llm(&harness.session.messages())).unwrap();
    assert_eq!(added_names(&current), vec!["second"]);
    assert_eq!(get_system_message_text(&current), harness.session.system_prompt());
    harness.cleanup();
}

#[tokio::test]
async fn set_active_tools_in_before_agent_start_controls_the_same_request() {
    let switching = extension(|pi| {
        register_first_and_second(&pi, false)?;
        let turn = Arc::new(Mutex::new(0));
        let api = pi.clone();
        let handler: ExtensionHandler = Arc::new(move |_event, _ctx| {
            let (turn, api) = (turn.clone(), api.clone());
            Box::pin(async move {
                let current = {
                    let mut turn = turn.lock();
                    *turn += 1;
                    *turn - 1
                };
                if current == 1 {
                    api.set_active_tools(vec!["second".into()])?;
                }
                Ok(None)
            })
        });
        pi.on("before_agent_start", handler)?;
        Ok(())
    });
    let harness = create_harness(HarnessOptions {
        extension_factories: vec![switching],
        initial_active_tool_names: Some(vec!["first".into()]),
        ..Default::default()
    })
    .await
    .unwrap();
    let requests: Requests = Arc::default();
    harness.set_responses(capturing(&requests, vec![faux_text_message("first"), faux_text_message("second")]));
    harness.session.prompt("first", None).await.unwrap();
    harness.session.prompt("second", None).await.unwrap();
    let requests = requests.lock().clone();
    assert_eq!(requests.len(), 2);
    let update = system_messages(&requests[1].messages).last().unwrap().clone();
    assert_eq!(update["toolsRemoved"], json!([{ "name": "first" }]));
    assert!(update.get("toolsAdded").is_none());
    assert_eq!(harness.session.get_active_tool_names(), vec!["second".to_string()]);
    harness.cleanup();
}

#[tokio::test]
async fn keeps_tool_declarations_stable_across_a_session_json_round_trip() {
    let plain: pi_agent::types::AgentToolRef = Arc::new(FnAgentTool::new(
        "plain",
        "Plain",
        "Plain tool",
        json!({ "type": "object", "properties": {} }),
        |_, _, _, _| async { Ok(AgentToolResult { content: vec![], details: Some(json!({})), ..Default::default() }) },
    ));
    let harness = create_harness(HarnessOptions {
        tools: Some(vec![plain]),
        initial_active_tool_names: Some(vec!["plain".into()]),
        ..Default::default()
    })
    .await
    .unwrap();
    harness.set_responses(vec![faux_text_message("first").into(), faux_text_message("second").into()]);
    harness.session.prompt("one", None).await.unwrap();
    let messages = harness.session.messages();
    let head = serde_json::to_value(&messages[0]).unwrap();
    assert_eq!(head["role"], "system");
    let declaration = head["toolsAdded"][0].as_object().expect("tool declaration");
    assert!(!declaration.contains_key("constrainedSampling"));
    assert!(!declaration.contains_key("execute"));

    // Simulate a resume: the persisted JSON must replay to the same declarations.
    let round_trip: Vec<AgentMessage> =
        serde_json::from_value(serde_json::to_value(&messages).unwrap()).unwrap();
    harness.session.agent().set_messages(round_trip);
    harness.session.prompt("two", None).await.unwrap();
    assert_eq!(harness.session.messages().iter().filter(|m| m.role() == "system").count(), 1);
    harness.cleanup();
}
