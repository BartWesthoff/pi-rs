//! Port of `packages/agent/test/e2e.test.ts` against pi-ai's faux provider.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use pi_agent::{
    Agent, AgentEvent, AgentInitialState, AgentMessage, AgentOptions, AgentToolRef,
    AgentToolResult, FnAgentTool, ThinkingLevel, models_stream_fn,
};
use pi_ai::providers::faux::{
    FauxMessageOptions, FauxModelDefinition, FauxProviderHandle, FauxResponseStep,
    RegisterFauxProviderOptions, faux_assistant_message, faux_provider, faux_text,
    faux_text_message, faux_thinking, faux_tool_call,
};
use pi_ai::{
    AssistantContent, AssistantMessage, Message, Model, Models, StopReason, TextContent,
    ThinkingContent, ToolCall, ToolResultMessage, Usage, UserContent, UserContentBlock,
    UserMessage,
};
use serde_json::{Value, json};

fn faux(options: RegisterFauxProviderOptions) -> (FauxProviderHandle, Models) {
    let handle = faux_provider(options);
    let models = Models::default();
    models.set_provider(handle.provider.clone());
    (handle, models)
}

fn evaluate(expression: &str) -> anyhow::Result<f64> {
    for op in ['+', '-', '*', '/'] {
        if let Some((left, right)) = expression.split_once(op) {
            let (a, b): (f64, f64) = (left.trim().parse()?, right.trim().parse()?);
            return Ok(match op {
                '+' => a + b,
                '-' => a - b,
                '*' => a * b,
                _ => a / b,
            });
        }
    }
    Ok(expression.trim().parse()?)
}

fn calculate_tool() -> AgentToolRef {
    FnAgentTool::new(
        "calculate",
        "Calculator",
        "Evaluate mathematical expressions",
        json!({
            "type": "object",
            "properties": {
                "expression": { "type": "string", "description": "The mathematical expression to evaluate" }
            },
            "required": ["expression"]
        }),
        |_, args, _, _| async move {
            let expression = args["expression"].as_str().unwrap_or_default().to_string();
            let result = evaluate(&expression)?;
            Ok(AgentToolResult::text(format!("{expression} = {result}"), None))
        },
    )
    .into_ref()
}

fn agent(models: Models, model: Model, system_prompt: &str, tools: Vec<AgentToolRef>) -> Agent {
    Agent::new(AgentOptions {
        stream_fn: Some(models_stream_fn(models)),
        initial_state: Some(AgentInitialState {
            system_prompt: Some(system_prompt.into()),
            model: Some(model),
            thinking_level: Some(ThinkingLevel::Off),
            tools: Some(tools),
            ..Default::default()
        }),
        ..Default::default()
    })
}

fn text_of(message: &AgentMessage) -> String {
    match message {
        AgentMessage::Assistant(m) => m
            .content
            .iter()
            .filter_map(AssistantContent::as_text)
            .map(|t| t.text.clone())
            .collect::<Vec<_>>()
            .join("\n"),
        AgentMessage::ToolResult(m) => m
            .content
            .iter()
            .filter_map(UserContentBlock::as_text)
            .map(|t| t.text.clone())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[tokio::test]
async fn handles_a_basic_text_prompt() {
    let (faux, models) = faux(RegisterFauxProviderOptions::default());
    faux.set_responses(vec![faux_text_message("4").into()]);
    let agent = agent(
        models,
        faux.get_model(),
        "You are a helpful assistant. Keep your responses concise.",
        vec![],
    );
    agent
        .prompt("What is 2+2? Answer with just the number.")
        .await
        .unwrap();
    assert!(!agent.is_streaming());
    let messages = agent.messages();
    assert_eq!(
        messages.iter().map(AgentMessage::role).collect::<Vec<_>>(),
        vec!["system", "user", "assistant"]
    );
    assert!(text_of(&messages[2]).contains('4'));
}

#[tokio::test]
async fn executes_tools_and_tracks_pending_tool_calls() {
    let (faux, models) = faux(RegisterFauxProviderOptions::default());
    let mut args = serde_json::Map::new();
    args.insert("expression".into(), json!("123 * 456"));
    faux.set_responses(vec![
        faux_assistant_message(
            vec![
                faux_text("Let me calculate that."),
                faux_tool_call("calculate", args, Some("calc-1")),
            ],
            FauxMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            },
        )
        .into(),
        faux_text_message("The result is 56088.").into(),
    ]);
    let agent = agent(
        models,
        faux.get_model(),
        "You are a helpful assistant. Always use the calculator tool for math.",
        vec![calculate_tool()],
    );
    let pending = Arc::new(Mutex::new(Vec::<(&'static str, Vec<String>)>::new()));
    let (p, weak) = (pending.clone(), agent.downgrade());
    agent.subscribe_fn(move |event, _| {
        if matches!(
            event,
            AgentEvent::ToolExecutionStart { .. } | AgentEvent::ToolExecutionEnd { .. }
        ) && let Some(agent) = weak.upgrade()
        {
            p.lock().push((
                event.type_name(),
                agent.pending_tool_calls().into_iter().collect(),
            ));
        }
    });

    agent
        .prompt("Calculate 123 * 456 using the calculator tool.")
        .await
        .unwrap();

    assert!(!agent.is_streaming());
    let messages = agent.messages();
    assert!(messages.len() >= 4);
    let tool_result = messages
        .iter()
        .find(|m| m.role() == "toolResult")
        .expect("tool result");
    assert!(text_of(tool_result).contains("123 * 456 = 56088"));
    assert!(text_of(messages.last().unwrap()).contains("56088"));
    assert!(agent.pending_tool_calls().is_empty());
    assert_eq!(
        *pending.lock(),
        vec![
            ("tool_execution_start", vec!["calc-1".to_string()]),
            ("tool_execution_end", vec![])
        ]
    );
}

#[tokio::test]
async fn handles_abort_during_streaming() {
    let (faux, models) = faux(RegisterFauxProviderOptions {
        tokens_per_second: Some(20.0),
        token_size_min: Some(2),
        token_size_max: Some(2),
        ..Default::default()
    });
    faux.set_responses(vec![
        faux_text_message("one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen")
            .into(),
    ]);
    let agent = agent(
        models,
        faux.get_model(),
        "You are a helpful assistant.",
        vec![],
    );
    let prompt = agent.prompt("Count slowly from 1 to 20.");
    let aborter = agent.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        aborter.abort();
    });
    prompt.await.unwrap();

    assert!(!agent.is_streaming());
    let messages = agent.messages();
    assert!(messages.len() >= 2);
    let last = messages
        .last()
        .unwrap()
        .as_assistant()
        .expect("assistant message");
    assert_eq!(last.stop_reason, StopReason::Aborted);
    assert!(last.error_message.is_some());
    assert_eq!(agent.error_message(), last.error_message);
}

#[tokio::test]
async fn emits_lifecycle_updates_while_streaming() {
    let (faux, models) = faux(RegisterFauxProviderOptions {
        token_size_min: Some(1),
        token_size_max: Some(1),
        ..Default::default()
    });
    faux.set_responses(vec![faux_text_message("1 2 3 4 5").into()]);
    let agent = agent(
        models,
        faux.get_model(),
        "You are a helpful assistant.",
        vec![],
    );
    let events = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let e = events.clone();
    agent.subscribe_fn(move |event, _| e.lock().push(event.type_name()));
    agent.prompt("Count from 1 to 5.").await.unwrap();

    let events = events.lock().clone();
    for expected in [
        "agent_start",
        "turn_start",
        "message_start",
        "message_update",
        "message_end",
        "turn_end",
        "agent_end",
    ] {
        assert!(events.contains(&expected), "missing {expected}");
    }
    let first = |name: &str| events.iter().position(|e| *e == name).unwrap();
    let last = |name: &str| events.iter().rposition(|e| *e == name).unwrap();
    assert!(first("agent_start") < first("message_start"));
    assert!(first("message_start") < first("message_end"));
    assert!(first("message_end") < last("agent_end"));
    assert!(!agent.is_streaming());
    assert_eq!(agent.messages().len(), 3);
}

#[tokio::test]
async fn maintains_context_across_multiple_turns() {
    let (faux, models) = faux(RegisterFauxProviderOptions::default());
    faux.set_responses(vec![
        faux_text_message("Nice to meet you, Alice.").into(),
        FauxResponseStep::factory(|context, _, _, _| {
            let has_alice = context.messages.iter().any(|m| match m {
                Message::User(UserMessage {
                    content: UserContent::Text(text),
                    ..
                }) => text.contains("Alice"),
                Message::User(UserMessage {
                    content: UserContent::Blocks(blocks),
                    ..
                }) => blocks
                    .iter()
                    .any(|b| b.as_text().is_some_and(|t| t.text.contains("Alice"))),
                _ => false,
            });
            faux_text_message(if has_alice {
                "Your name is Alice."
            } else {
                "I do not know your name."
            })
        }),
    ]);
    let agent = agent(
        models,
        faux.get_model(),
        "You are a helpful assistant.",
        vec![],
    );
    agent.prompt("My name is Alice.").await.unwrap();
    assert_eq!(agent.messages().len(), 3);
    agent.prompt("What is my name?").await.unwrap();
    let messages = agent.messages();
    assert_eq!(messages.len(), 5);
    assert!(text_of(&messages[4]).to_lowercase().contains("alice"));
}

#[tokio::test]
async fn preserves_thinking_content_blocks() {
    let (faux, models) = faux(RegisterFauxProviderOptions {
        models: vec![FauxModelDefinition {
            id: "faux-reasoning".into(),
            reasoning: Some(true),
            ..Default::default()
        }],
        ..Default::default()
    });
    faux.set_responses(vec![
        faux_assistant_message(
            vec![faux_thinking("step by step"), faux_text("4")],
            FauxMessageOptions::default(),
        )
        .into(),
    ]);
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(models_stream_fn(models)),
        initial_state: Some(AgentInitialState {
            system_prompt: Some("You are a helpful assistant.".into()),
            model: Some(faux.get_model()),
            thinking_level: Some(ThinkingLevel::Low),
            tools: Some(vec![]),
            ..Default::default()
        }),
        ..Default::default()
    });
    agent.prompt("What is 2+2?").await.unwrap();
    let messages = agent.messages();
    let assistant = messages[2].as_assistant().expect("assistant message");
    assert_eq!(
        assistant.content,
        vec![
            AssistantContent::Thinking(ThinkingContent::new("step by step")),
            AssistantContent::Text(TextContent::new("4")),
        ]
    );
}

#[tokio::test]
async fn continue_throws_when_no_messages_in_context() {
    let (faux, models) = faux(RegisterFauxProviderOptions::default());
    let agent = agent(models, faux.get_model(), "Test", vec![]);
    assert_eq!(
        agent.continue_run().await.unwrap_err().to_string(),
        "No messages to continue from"
    );
}

fn empty_assistant(
    model: &Model,
    content: Vec<AssistantContent>,
    stop_reason: StopReason,
) -> AssistantMessage {
    AssistantMessage {
        content: content.into(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        usage: Usage::default(),
        stop_reason,
        timestamp: pi_ai::utils::now_ms(),
        ..Default::default()
    }
}

#[tokio::test]
async fn continue_throws_when_last_message_is_assistant() {
    let (faux, models) = faux(RegisterFauxProviderOptions::default());
    let model = faux.get_model();
    let agent = agent(models, model.clone(), "Test", vec![]);
    agent.set_messages(vec![
        empty_assistant(
            &model,
            vec![AssistantContent::Text(TextContent::new("Hello"))],
            StopReason::Stop,
        )
        .into(),
    ]);
    assert_eq!(
        agent.continue_run().await.unwrap_err().to_string(),
        "Cannot continue from message role: assistant"
    );
}

#[tokio::test]
async fn continues_and_gets_a_response_when_last_message_is_user() {
    let (faux, models) = faux(RegisterFauxProviderOptions::default());
    faux.set_responses(vec![faux_text_message("HELLO WORLD").into()]);
    let agent = agent(
        models,
        faux.get_model(),
        "You are a helpful assistant. Follow instructions exactly.",
        vec![],
    );
    agent.set_messages(vec![AgentMessage::User(UserMessage::new(
        vec![UserContentBlock::text("Say exactly: HELLO WORLD")],
        pi_ai::utils::now_ms(),
    ))]);
    agent.continue_run().await.unwrap();
    assert!(!agent.is_streaming());
    let messages = agent.messages();
    assert_eq!(
        messages.iter().map(AgentMessage::role).collect::<Vec<_>>(),
        vec!["user", "assistant"]
    );
    assert!(text_of(&messages[1]).to_uppercase().contains("HELLO WORLD"));
}

#[tokio::test]
async fn continues_and_processes_tool_results() {
    let (faux, models) = faux(RegisterFauxProviderOptions::default());
    let model = faux.get_model();
    faux.set_responses(vec![faux_text_message("The answer is 8.").into()]);
    let agent = agent(
        models,
        model.clone(),
        "You are a helpful assistant. After getting a calculation result, state the answer clearly.",
        vec![calculate_tool()],
    );
    let mut arguments = serde_json::Map::new();
    arguments.insert("expression".into(), Value::String("5 + 3".into()));
    agent.set_messages(vec![
        AgentMessage::User(UserMessage::new(
            vec![UserContentBlock::text("What is 5 + 3?")],
            pi_ai::utils::now_ms(),
        )),
        empty_assistant(
            &model,
            vec![
                AssistantContent::Text(TextContent::new("Let me calculate that.")),
                AssistantContent::ToolCall(ToolCall {
                    id: "calc-1".into(),
                    name: "calculate".into(),
                    arguments,
                    ..Default::default()
                }),
            ],
            StopReason::ToolUse,
        )
        .into(),
        AgentMessage::ToolResult(ToolResultMessage {
            tool_call_id: "calc-1".into(),
            tool_name: "calculate".into(),
            content: vec![UserContentBlock::text("5 + 3 = 8")],
            is_error: false,
            timestamp: pi_ai::utils::now_ms(),
            ..Default::default()
        }),
    ]);
    agent.continue_run().await.unwrap();
    assert!(!agent.is_streaming());
    let messages = agent.messages();
    assert!(messages.len() >= 4);
    assert_eq!(messages.last().unwrap().role(), "assistant");
    assert!(text_of(messages.last().unwrap()).contains('8'));
}
