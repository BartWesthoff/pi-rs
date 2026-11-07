//! Port of `packages/agent/test/agent.test.ts`.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use common::*;
use parking_lot::Mutex;
use pi_agent::hooks;
use pi_agent::{
    Agent, AgentEvent, AgentInitialState, AgentMessage, AgentOptions, AgentToolResult,
    AgentToolUpdateCallback, AgentTurnDecision, FnAgentTool, QueueMode, StreamFn, ThinkingLevel,
    set_default_stream_fn,
};
use pi_ai::utils::transcript::to_tool_declaration;
use pi_ai::{
    AssistantMessageEvent, AssistantMessageEventStream, ClientAttribution, ClientIdentity, Message,
    StopReason, SystemContent, SystemMessage, Tool, ToolReference, ToolResultMessage,
    UserAgentMode, UserAgentNamed, UserContent, UserContentBlock, UserMessage, pi_attribution,
};
use serde_json::json;
use tokio::sync::Notify;

fn unused_stream_fn() -> StreamFn {
    Arc::new(|_, _, _| Box::pin(async { Err(anyhow::anyhow!("Unexpected stream call")) }))
}

fn agent_with(options: AgentOptions) -> Agent {
    Agent::new(options)
}

fn done_stream_fn() -> StreamFn {
    mock_stream_fn(|_, _, _| respond(assistant_text("done")))
}

fn options_with(stream_fn: StreamFn) -> AgentOptions {
    AgentOptions {
        stream_fn: Some(stream_fn),
        ..Default::default()
    }
}

fn create_tool(name: &str) -> pi_agent::AgentToolRef {
    FnAgentTool::new(
        name,
        name,
        format!("{name} tool"),
        json!({ "type": "object", "properties": {} }),
        {
            let name = name.to_string();
            move |_, _, _, _| {
                let name = name.clone();
                async move { Ok(text_result(name, json!({}))) }
            }
        },
    )
    .into_ref()
}

fn system(content: &str, timestamp: i64) -> SystemMessage {
    SystemMessage {
        content: SystemContent::Text(content.into()),
        timestamp,
        ..Default::default()
    }
}

/// A stream that starts, then waits for `options.signal` and reports `aborted`.
fn abortable_stream_fn() -> StreamFn {
    mock_stream_fn(|_, _, options| {
        let (sender, stream) = AssistantMessageEventStream::new();
        let signal = options.signal.clone().unwrap_or_default();
        tokio::spawn(async move {
            sender.push(AssistantMessageEvent::Start {
                partial: assistant_text(""),
            });
            signal.cancelled().await;
            sender.push(AssistantMessageEvent::Error {
                reason: StopReason::Aborted,
                error: assistant_text("Aborted"),
            });
        });
        stream
    })
}

fn user_texts_recorder() -> (StreamFn, Arc<Mutex<Vec<Vec<String>>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let r = requests.clone();
    let stream_fn = mock_stream_fn(move |_, context, _| {
        r.lock().push(llm_user_texts(&context));
        respond(assistant_text("done"))
    });
    (stream_fn, requests)
}

#[tokio::test]
async fn uses_the_configured_default_when_stream_fn_is_omitted() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    set_default_stream_fn(Some(mock_stream_fn(move |_, _, _| {
        c.fetch_add(1, Ordering::SeqCst);
        respond(assistant_text("fallback"))
    })));
    let agent = Agent::new(AgentOptions::default());
    agent.prompt("Hello").await.unwrap();
    set_default_stream_fn(None);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn creates_an_agent_with_default_state() {
    let agent = agent_with(options_with(unused_stream_fn()));
    let state = agent.state();
    assert_eq!(state.thinking_level, ThinkingLevel::Off);
    assert!(state.tools.is_empty());
    assert!(state.messages.is_empty());
    assert!(!state.is_streaming);
    assert!(state.streaming_message.is_none());
    assert!(state.pending_tool_calls.is_empty());
    assert!(state.error_message.is_none());
    assert_eq!(state.model.id, "unknown");
}

#[tokio::test]
async fn creates_an_agent_with_custom_initial_state() {
    let mut custom_model = create_model();
    custom_model.id = "gpt-4o-mini".into();
    let agent = agent_with(AgentOptions {
        stream_fn: Some(unused_stream_fn()),
        initial_state: Some(AgentInitialState {
            system_prompt: Some("You are a helpful assistant.".into()),
            model: Some(custom_model.clone()),
            thinking_level: Some(ThinkingLevel::Low),
            ..Default::default()
        }),
        ..Default::default()
    });
    assert_eq!(
        agent.messages(),
        vec![AgentMessage::System(system(
            "You are a helpful assistant.",
            0
        ))]
    );
    assert_eq!(
        serde_json::to_value(&agent.messages()[0]).unwrap(),
        json!({ "role": "system", "content": "You are a helpful assistant.", "timestamp": 0 })
    );
    assert_eq!(agent.model(), custom_model);
    assert_eq!(agent.thinking_level(), ThinkingLevel::Low);
    assert_eq!(agent.system_prompt(), "You are a helpful assistant.");
}

#[tokio::test]
async fn converts_initial_prompt_and_tools_into_transcript_state() {
    let agent = agent_with(AgentOptions {
        stream_fn: Some(unused_stream_fn()),
        initial_state: Some(AgentInitialState {
            system_prompt: Some("You are helpful.".into()),
            tools: Some(vec![create_tool("echo")]),
            ..Default::default()
        }),
        ..Default::default()
    });
    let messages = agent.messages();
    let initial = messages[0].as_system().expect("initial system message");
    assert_eq!(
        initial.content,
        SystemContent::Text("You are helpful.".into())
    );
    let names: Vec<String> = initial
        .tools_added
        .as_ref()
        .unwrap()
        .iter()
        .map(|t| t.name.clone())
        .collect();
    assert_eq!(names, vec!["echo"]);
}

fn tool_markers(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(Message::as_system)
        .flat_map(|m| {
            let added: Vec<String> = m
                .tools_added
                .iter()
                .flatten()
                .map(|t| t.name.clone())
                .collect();
            let removed: Vec<String> = m
                .tools_removed
                .iter()
                .flatten()
                .map(|t| t.name.clone())
                .collect();
            vec![
                format!("+{}", added.join(",")),
                format!("-{}", removed.join(",")),
            ]
        })
        .collect()
}

#[tokio::test]
async fn declares_tool_loadout_changes_before_the_next_request() {
    let requests = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
    let r = requests.clone();
    let agent = agent_with(AgentOptions {
        stream_fn: Some(mock_stream_fn(move |_, context, _| {
            r.lock().push(tool_markers(&context.messages));
            respond(assistant_text("done"))
        })),
        initial_state: Some(AgentInitialState {
            system_prompt: Some("You are helpful.".into()),
            tools: Some(vec![create_tool("first")]),
            ..Default::default()
        }),
        ..Default::default()
    });

    agent.prompt("one").await.unwrap();
    agent.set_tools(vec![create_tool("second")]);
    agent.prompt("two").await.unwrap();
    agent.prompt("three").await.unwrap();

    assert_eq!(
        *requests.lock(),
        vec![
            vec!["+first", "-"],
            vec!["+first", "-", "+second", "-first"],
            vec!["+first", "-", "+second", "-first"],
        ]
    );
    let messages = agent.messages();
    let update = messages
        .iter()
        .filter_map(AgentMessage::as_system)
        .find(|m| m.tools_removed.is_some())
        .expect("tool update")
        .clone();
    assert_eq!(update.content, SystemContent::Text(String::new()));
    assert_eq!(
        update.tools_added,
        Some(vec![Tool::new(
            "second",
            "second tool",
            json!({ "type": "object", "properties": {} })
        )])
    );
    assert_eq!(
        update.tools_removed,
        Some(vec![ToolReference {
            name: "first".into()
        }])
    );
    assert!(update.timestamp > 0);
}

#[tokio::test]
async fn merges_tool_changes_into_a_pending_system_message() {
    let agent = agent_with(AgentOptions {
        stream_fn: Some(mock_stream_fn(|_, context, _| {
            assert_eq!(
                context
                    .messages
                    .iter()
                    .filter(|m| m.as_system().is_some())
                    .count(),
                2
            );
            respond(assistant_text("done"))
        })),
        initial_state: Some(AgentInitialState {
            system_prompt: Some("You are helpful.".into()),
            ..Default::default()
        }),
        ..Default::default()
    });
    let echo = FnAgentTool::new(
        "echo",
        "Echo",
        "Echo input",
        empty_schema(),
        |_, _, _, _| async { Ok(text_result("echo", json!({}))) },
    )
    .into_ref();
    agent.set_tools(vec![echo]);
    let mut sections = indexmap::IndexMap::new();
    sections.insert("skills".to_string(), Some("<skills>x</skills>".to_string()));
    let pending = SystemMessage {
        sections: Some(sections.clone()),
        ..system("", 1)
    };
    agent
        .prompt(vec![
            AgentMessage::System(pending),
            AgentMessage::User(UserMessage::new("hi", 2)),
        ])
        .await
        .unwrap();

    assert_eq!(
        agent.messages()[1],
        AgentMessage::System(SystemMessage {
            sections: Some(sections),
            tools_added: Some(vec![Tool::new("echo", "Echo input", empty_schema())]),
            ..system("", 1)
        })
    );
}

#[tokio::test]
async fn rewrites_pending_tool_declarations_to_match_the_executable_set() {
    let agent = agent_with(AgentOptions {
        stream_fn: Some(done_stream_fn()),
        initial_state: Some(AgentInitialState {
            system_prompt: Some("You are helpful.".into()),
            tools: Some(vec![create_tool("first")]),
            ..Default::default()
        }),
        ..Default::default()
    });
    let mut sections = indexmap::IndexMap::new();
    sections.insert("note".to_string(), Some("<note>x</note>".to_string()));
    agent
        .prompt(vec![
            AgentMessage::System(SystemMessage {
                sections: Some(sections.clone()),
                tools_added: Some(vec![to_tool_declaration(&create_tool("second").to_tool())]),
                tools_removed: Some(vec![ToolReference {
                    name: "first".into(),
                }]),
                ..system("", 1)
            }),
            AgentMessage::User(UserMessage::new("hi", 2)),
        ])
        .await
        .unwrap();

    assert_eq!(
        agent.messages()[1],
        AgentMessage::System(SystemMessage {
            sections: Some(sections),
            ..system("", 1)
        })
    );
    let current = pi_agent::transcript::get_current_system_message(&agent.messages()).unwrap();
    let names: Vec<String> = current
        .tools_added
        .unwrap()
        .iter()
        .map(|t| t.name.clone())
        .collect();
    assert_eq!(names, vec!["first"]);
}

#[tokio::test]
async fn restores_the_transcript_baseline_when_reset() {
    let agent = agent_with(AgentOptions {
        stream_fn: Some(unused_stream_fn()),
        initial_state: Some(AgentInitialState {
            system_prompt: Some("You are helpful.".into()),
            tools: Some(vec![create_tool("echo")]),
            messages: Some(vec![AgentMessage::User(UserMessage::new("old", 1))]),
            ..Default::default()
        }),
        ..Default::default()
    });
    agent.reset().unwrap();
    let messages = agent.messages();
    assert_eq!(messages.len(), 1);
    let initial = messages[0].as_system().unwrap();
    assert_eq!(
        initial.content,
        SystemContent::Text("You are helpful.".into())
    );
    let names: Vec<String> = initial
        .tools_added
        .as_ref()
        .unwrap()
        .iter()
        .map(|t| t.name.clone())
        .collect();
    assert_eq!(names, vec!["echo"]);
}

#[tokio::test]
async fn subscribes_to_events() {
    let agent = agent_with(options_with(unused_stream_fn()));
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    let subscription = agent.subscribe_fn(move |_, _| {
        c.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(count.load(Ordering::SeqCst), 0);
    agent.set_thinking_level(ThinkingLevel::Low);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(agent.thinking_level(), ThinkingLevel::Low);
    subscription.unsubscribe();
    agent.set_thinking_level(ThinkingLevel::High);
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn emits_full_lifecycle_events_for_thrown_run_failures() {
    let agent = agent_with(options_with(Arc::new(|_, _, _| {
        Box::pin(async { Err(anyhow::anyhow!("provider exploded")) })
    })));
    let events = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let e = events.clone();
    agent.subscribe_fn(move |event, _| e.lock().push(event.type_name()));
    agent.prompt("hello").await.unwrap();
    assert_eq!(
        *events.lock(),
        vec![
            "agent_start",
            "turn_start",
            "message_start",
            "message_end",
            "message_start",
            "message_end",
            "turn_end",
            "agent_end",
        ]
    );
    let messages = agent.messages();
    let last = messages
        .last()
        .unwrap()
        .as_assistant()
        .expect("assistant message");
    assert_eq!(last.stop_reason, StopReason::Error);
    assert_eq!(last.error_message.as_deref(), Some("provider exploded"));
    assert_eq!(agent.error_message().as_deref(), Some("provider exploded"));
}

#[tokio::test]
async fn awaits_async_subscribers_before_prompt_resolves() {
    let barrier = Arc::new(Notify::new());
    let agent = agent_with(options_with(mock_stream_fn(|_, _, _| {
        respond(assistant_text("ok"))
    })));
    let listener_finished = Arc::new(AtomicBool::new(false));
    let (b, f) = (barrier.clone(), listener_finished.clone());
    agent.subscribe(hooks::listener(move |event, _| {
        let b = b.clone();
        let f = f.clone();
        async move {
            if let AgentEvent::AgentEnd { .. } = event {
                b.notified().await;
                f.store(true, Ordering::SeqCst);
            }
        }
    }));
    let prompt_resolved = Arc::new(AtomicBool::new(false));
    let pr = prompt_resolved.clone();
    let prompt = agent.prompt("hello");
    let handle = tokio::spawn(async move {
        prompt.await.unwrap();
        pr.store(true, Ordering::SeqCst);
    });
    sleep_ms(10).await;
    assert!(!prompt_resolved.load(Ordering::SeqCst));
    assert!(!listener_finished.load(Ordering::SeqCst));
    assert!(agent.is_streaming());

    barrier.notify_one();
    handle.await.unwrap();
    assert!(listener_finished.load(Ordering::SeqCst));
    assert!(prompt_resolved.load(Ordering::SeqCst));
    assert!(!agent.is_streaming());
}

#[tokio::test]
async fn wait_for_idle_waits_for_async_subscribers() {
    let barrier = Arc::new(Notify::new());
    let agent = agent_with(options_with(mock_stream_fn(|_, _, _| {
        respond(assistant_text("ok"))
    })));
    let b = barrier.clone();
    agent.subscribe(hooks::listener(move |event, _| {
        let b = b.clone();
        async move {
            if let AgentEvent::MessageEnd {
                message: AgentMessage::Assistant(_),
            } = event
            {
                b.notified().await;
            }
        }
    }));
    let prompt = agent.prompt("hello");
    let idle_resolved = Arc::new(AtomicBool::new(false));
    let ir = idle_resolved.clone();
    let idle = agent.wait_for_idle();
    let idle_handle = tokio::spawn(async move {
        idle.await;
        ir.store(true, Ordering::SeqCst);
    });
    sleep_ms(10).await;
    assert!(!idle_resolved.load(Ordering::SeqCst));
    assert!(agent.is_streaming());

    barrier.notify_one();
    prompt.await.unwrap();
    idle_handle.await.unwrap();
    assert!(idle_resolved.load(Ordering::SeqCst));
    assert!(!agent.is_streaming());
}

#[tokio::test]
async fn passes_the_active_abort_signal_to_subscribers() {
    let agent = agent_with(options_with(abortable_stream_fn()));
    let received = Arc::new(Mutex::new(None));
    let r = received.clone();
    agent.subscribe_fn(move |event, signal| {
        if let AgentEvent::AgentStart = event {
            *r.lock() = Some(signal.clone());
        }
    });
    let prompt = agent.prompt("hello");
    sleep_ms(10).await;
    let signal = received.lock().clone().expect("signal");
    assert!(!signal.is_cancelled());
    agent.abort();
    prompt.await.unwrap();
    assert!(signal.is_cancelled());
}

fn tool_use_message(calls: Vec<pi_ai::ToolCall>) -> pi_ai::AssistantMessage {
    assistant_tool_calls(calls, StopReason::ToolUse)
}

#[tokio::test]
async fn ignores_tool_updates_after_the_tool_execution_settles() {
    let delayed: Arc<Mutex<Option<AgentToolUpdateCallback>>> = Arc::new(Mutex::new(None));
    let d = delayed.clone();
    let tool = FnAgentTool::new(
        "delayed_tool",
        "Delayed Tool",
        "Captures progress callbacks",
        empty_schema(),
        move |_, _, _, on_update| {
            *d.lock() = Some(on_update.clone());
            on_update(AgentToolResult::text(
                "running",
                Some(json!({ "status": "running" })),
            ));
            async {
                let mut result = text_result("ok", json!({ "status": "done" }));
                result.terminate = Some(true);
                Ok(result)
            }
        },
    )
    .into_ref();
    let agent = agent_with(AgentOptions {
        initial_state: Some(AgentInitialState {
            tools: Some(vec![tool]),
            ..Default::default()
        }),
        stream_fn: Some(mock_stream_fn(|_, _, _| {
            respond(tool_use_message(vec![tool_call(
                "call-1",
                "delayed_tool",
                json!({}),
            )]))
        })),
        ..Default::default()
    });
    let events = Arc::new(Mutex::new(Vec::<AgentEvent>::new()));
    let e = events.clone();
    agent.subscribe_fn(move |event, _| e.lock().push(event.clone()));

    agent.prompt("run tool").await.unwrap();
    let count_after_prompt = events.lock().len();
    let callback = delayed.lock().clone().unwrap();
    callback(AgentToolResult::text(
        "late",
        Some(json!({ "status": "late" })),
    ));
    sleep_ms(0).await;

    let events = events.lock();
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::ToolExecutionUpdate { .. }))
            .count(),
        1
    );
    assert_eq!(events.len(), count_after_prompt);
}

#[tokio::test]
async fn ignores_a_settled_parallel_tool_update_while_another_tool_is_running() {
    let slow_started = Arc::new(Notify::new());
    let settled_ended = Arc::new(Notify::new());
    let release_slow = Arc::new(Notify::new());
    let settled_update: Arc<Mutex<Option<AgentToolUpdateCallback>>> = Arc::new(Mutex::new(None));
    let su = settled_update.clone();
    let settled = FnAgentTool::new(
        "settled_tool",
        "Settled Tool",
        "Captures progress callbacks",
        empty_schema(),
        move |_, _, _, on_update| {
            *su.lock() = Some(on_update);
            async {
                let mut result = text_result("done", json!({ "status": "done" }));
                result.terminate = Some(true);
                Ok(result)
            }
        },
    )
    .into_ref();
    let (ss, rs) = (slow_started.clone(), release_slow.clone());
    let slow = FnAgentTool::new(
        "slow_tool",
        "Slow Tool",
        "Keeps the agent run active",
        empty_schema(),
        move |_, _, _, _| {
            let ss = ss.clone();
            let rs = rs.clone();
            async move {
                ss.notify_one();
                rs.notified().await;
                let mut result = text_result("done", json!({ "status": "done" }));
                result.terminate = Some(true);
                Ok(result)
            }
        },
    )
    .into_ref();
    let agent = agent_with(AgentOptions {
        initial_state: Some(AgentInitialState {
            tools: Some(vec![settled, slow]),
            ..Default::default()
        }),
        stream_fn: Some(mock_stream_fn(|_, _, _| {
            respond(tool_use_message(vec![
                tool_call("call-1", "settled_tool", json!({})),
                tool_call("call-2", "slow_tool", json!({})),
            ]))
        })),
        ..Default::default()
    });
    let events = Arc::new(Mutex::new(Vec::<AgentEvent>::new()));
    let (e, se) = (events.clone(), settled_ended.clone());
    agent.subscribe_fn(move |event, _| {
        e.lock().push(event.clone());
        if let AgentEvent::ToolExecutionEnd { tool_call_id, .. } = event
            && tool_call_id == "call-1"
        {
            se.notify_one();
        }
    });

    let prompt = agent.prompt("run tools");
    slow_started.notified().await;
    settled_ended.notified().await;
    let count_before = events.lock().len();
    let callback = settled_update.lock().clone().unwrap();
    callback(AgentToolResult::text(
        "late",
        Some(json!({ "status": "late" })),
    ));
    sleep_ms(0).await;
    assert_eq!(events.lock().len(), count_before);

    release_slow.notify_one();
    prompt.await.unwrap();
    assert_eq!(
        events
            .lock()
            .iter()
            .filter(|e| matches!(e, AgentEvent::ToolExecutionUpdate { .. }))
            .count(),
        0
    );
}

#[tokio::test]
async fn updates_state_with_mutators() {
    let agent = agent_with(options_with(unused_stream_fn()));
    let mut new_model = create_model();
    new_model.id = "gemini-2.5-flash".into();
    agent.set_model(new_model.clone());
    assert_eq!(agent.model(), new_model);
    agent.set_thinking_level(ThinkingLevel::High);
    assert_eq!(agent.thinking_level(), ThinkingLevel::High);
    let tools = vec![create_tool("test")];
    agent.set_tools(tools.clone());
    assert_eq!(agent.tools().len(), 1);
    assert_eq!(agent.tools()[0].name(), "test");
    let messages = vec![user("Hello")];
    agent.set_messages(messages.clone());
    assert_eq!(agent.messages(), messages);
    let new_message: AgentMessage = assistant_text("Hi").into();
    agent.with_state_mut(|state| state.messages.push(new_message.clone()));
    assert_eq!(agent.messages().len(), 2);
    assert_eq!(agent.messages()[1], new_message);
    agent.set_messages(vec![]);
    assert!(agent.messages().is_empty());
}

#[tokio::test]
async fn supports_steering_and_follow_up_queues() {
    let agent = agent_with(options_with(unused_stream_fn()));
    let steering = user("Steering message");
    agent.steer(steering.clone());
    assert!(!agent.messages().contains(&steering));
    let follow_up = user("Follow-up message");
    agent.follow_up(follow_up.clone());
    assert!(!agent.messages().contains(&follow_up));
    assert!(agent.has_queued_messages());
}

#[tokio::test]
async fn abort_without_a_run_does_not_panic() {
    let agent = agent_with(options_with(unused_stream_fn()));
    agent.abort();
    assert!(agent.signal().is_none());
}

#[tokio::test]
async fn rejects_reset_while_processing_without_corrupting_the_transcript() {
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (s, r) = (started.clone(), release.clone());
    let agent = agent_with(options_with(mock_stream_fn(move |_, _, _| {
        let (sender, stream) = AssistantMessageEventStream::new();
        let (s, r) = (s.clone(), r.clone());
        tokio::spawn(async move {
            sender.push(AssistantMessageEvent::Start {
                partial: assistant_text(""),
            });
            s.notify_one();
            r.notified().await;
            push_terminal(&sender, assistant_text("Done"));
        });
        stream
    })));
    let prompt = agent.prompt("Hello");
    started.notified().await;
    assert!(agent.is_streaming());
    assert_eq!(roles(&agent.messages()), vec!["user"]);
    assert_eq!(
        agent.reset().unwrap_err().to_string(),
        "Agent is already processing. Wait for completion before resetting."
    );
    assert!(agent.is_streaming());
    assert_eq!(roles(&agent.messages()), vec!["user"]);
    release.notify_one();
    prompt.await.unwrap();
    assert!(!agent.is_streaming());
    assert_eq!(roles(&agent.messages()), vec!["user", "assistant"]);
}

#[tokio::test]
async fn rejects_prompt_while_streaming() {
    let agent = agent_with(options_with(abortable_stream_fn()));
    let first = agent.prompt("First message");
    sleep_ms(10).await;
    assert!(agent.is_streaming());
    assert_eq!(
        agent
            .prompt("Second message")
            .await
            .unwrap_err()
            .to_string(),
        "Agent is already processing a prompt. Use steer() or followUp() to queue messages, or wait for completion."
    );
    agent.abort();
    let _ = first.await;
}

#[tokio::test]
async fn rejects_continue_while_streaming() {
    let agent = agent_with(options_with(abortable_stream_fn()));
    let first = agent.prompt("First message");
    sleep_ms(10).await;
    assert!(agent.is_streaming());
    assert_eq!(
        agent.continue_run().await.unwrap_err().to_string(),
        "Agent is already processing. Wait for completion before continuing."
    );
    agent.abort();
    let _ = first.await;
}

#[tokio::test]
async fn continue_processes_queued_follow_ups_after_an_assistant_turn() {
    let agent = agent_with(options_with(mock_stream_fn(|_, _, _| {
        respond(assistant_text("Processed"))
    })));
    agent.set_messages(vec![
        AgentMessage::User(UserMessage::new(vec![UserContentBlock::text("Initial")], 1)),
        assistant_text("Initial response").into(),
    ]);
    agent.follow_up(AgentMessage::User(UserMessage::new(
        vec![UserContentBlock::text("Queued follow-up")],
        2,
    )));
    agent.continue_run().await.unwrap();
    let has_follow_up = agent.messages().iter().any(|m| match m {
        AgentMessage::User(UserMessage {
            content: UserContent::Text(text),
            ..
        }) => text == "Queued follow-up",
        AgentMessage::User(UserMessage {
            content: UserContent::Blocks(blocks),
            ..
        }) => blocks
            .iter()
            .any(|b| b.as_text().is_some_and(|t| t.text == "Queued follow-up")),
        _ => false,
    });
    assert!(has_follow_up);
    assert_eq!(agent.messages().last().unwrap().role(), "assistant");
}

async fn continue_keeps_steering_semantics(mode: QueueMode, expected_requests: usize) {
    let (stream_fn, requests) = user_texts_recorder();
    let agent = agent_with(AgentOptions {
        steering_mode: Some(mode.clone()),
        ..options_with(stream_fn)
    });
    agent.set_messages(vec![
        user("Initial"),
        assistant_text("Initial response").into(),
    ]);
    agent.steer(user("Steering 1"));
    agent.steer(user("Steering 2"));
    agent.continue_run().await.unwrap();
    let requests = requests.lock();
    assert_eq!(requests.len(), expected_requests);
    assert!(requests[0].contains(&"Steering 1".to_string()));
    if mode == QueueMode::OneAtATime {
        assert!(!requests[0].contains(&"Steering 2".to_string()));
        assert!(requests[1].contains(&"Steering 2".to_string()));
    } else {
        assert!(requests[0].contains(&"Steering 2".to_string()));
    }
}

#[tokio::test]
async fn continue_keeps_one_at_a_time_steering_for_assistant_tail() {
    continue_keeps_steering_semantics(QueueMode::OneAtATime, 2).await;
}

#[tokio::test]
async fn continue_keeps_all_steering_for_assistant_tail() {
    continue_keeps_steering_semantics(QueueMode::All, 1).await;
}

#[tokio::test]
async fn unrecognized_queue_mode_is_kept_and_drains_one_at_a_time() {
    // TS stores any mode string and only special-cases "all".
    let bogus = QueueMode::from_str_lossless("bogus");
    assert_eq!(serde_json::to_value(&bogus).unwrap(), serde_json::json!("bogus"));
    assert_eq!(serde_json::from_value::<QueueMode>(serde_json::json!("all")).unwrap(), QueueMode::All);
    let (stream_fn, requests) = user_texts_recorder();
    let agent = agent_with(AgentOptions {
        steering_mode: Some(bogus.clone()),
        ..options_with(stream_fn)
    });
    assert_eq!(agent.steering_mode(), bogus);
    agent.set_messages(vec![user("Initial"), assistant_text("Initial response").into()]);
    agent.steer(user("Steering 1"));
    agent.steer(user("Steering 2"));
    agent.continue_run().await.unwrap();
    let requests = requests.lock();
    assert_eq!(requests.len(), 2);
    assert!(!requests[0].contains(&"Steering 2".to_string()));
}

fn tool_then_text_stream_fn(final_text: &'static str) -> (StreamFn, Arc<Mutex<usize>>) {
    scripted_stream_fn(move |index| {
        if index == 0 {
            tool_use_message(vec![tool_call("tool-1", "noop", json!({}))])
        } else {
            assistant_text(final_text)
        }
    })
}

#[tokio::test]
async fn keeps_legacy_prepare_next_turn_signal_callback_behavior() {
    let saw_signal = Arc::new(AtomicBool::new(false));
    let s = saw_signal.clone();
    let (stream_fn, calls) = tool_then_text_stream_fn("done");
    let agent = agent_with(AgentOptions {
        initial_state: Some(AgentInitialState {
            tools: Some(vec![noop_tool("noop")]),
            ..Default::default()
        }),
        prepare_next_turn: Some(hooks::agent_prepare_next_turn(move |signal| {
            s.store(!signal.is_cancelled(), Ordering::SeqCst);
            async { None }
        })),
        ..options_with(stream_fn)
    });
    let run_signal = Arc::new(Mutex::new(None));
    let rs = run_signal.clone();
    agent.subscribe_fn(move |_, signal| *rs.lock() = Some(signal.clone()));
    agent.prompt("start").await.unwrap();
    assert_eq!(*calls.lock(), 2);
    assert!(saw_signal.load(Ordering::SeqCst));
}

#[tokio::test]
async fn forwards_finish_turn_through_agent_options_with_the_active_signal() {
    let saw_signal = Arc::new(AtomicBool::new(false));
    let callback_roles = Arc::new(Mutex::new(Vec::<String>::new()));
    let (s, r) = (saw_signal.clone(), callback_roles.clone());
    let (stream_fn, calls) = tool_then_text_stream_fn("should not run");
    let agent = agent_with(AgentOptions {
        initial_state: Some(AgentInitialState {
            tools: Some(vec![noop_tool("noop")]),
            ..Default::default()
        }),
        finish_turn: Some(hooks::finish_turn(move |turn, signal| {
            s.store(!signal.is_cancelled(), Ordering::SeqCst);
            *r.lock() = roles(&turn.context.messages);
            async { Some(AgentTurnDecision::End) }
        })),
        ..options_with(stream_fn)
    });
    agent.prompt("start").await.unwrap();
    assert_eq!(*calls.lock(), 1);
    assert!(saw_signal.load(Ordering::SeqCst));
    assert_eq!(
        *callback_roles.lock(),
        vec!["system", "user", "assistant", "toolResult"]
    );
}

async fn rejects_queued_continuation(messages: Vec<AgentMessage>) {
    let agent = agent_with(AgentOptions {
        initial_state: Some(AgentInitialState {
            messages: Some(messages),
            ..Default::default()
        }),
        ..options_with(unused_stream_fn())
    });
    let steering = user("steering");
    let follow_up = user("follow-up");
    agent.steer(steering.clone());
    agent.follow_up(follow_up.clone());
    assert_eq!(
        agent.continue_run().await.unwrap_err().to_string(),
        "No messages to continue from"
    );
    assert_eq!(agent.peek_queued_messages(), vec![steering]);
    agent.clear_steering_queue();
    assert_eq!(agent.peek_queued_messages(), vec![follow_up]);
}

#[tokio::test]
async fn rejects_a_queued_continuation_from_empty_context() {
    rejects_queued_continuation(vec![]).await;
}

#[tokio::test]
async fn rejects_a_queued_continuation_from_system_only_context() {
    rejects_queued_continuation(vec![AgentMessage::System(system("system only", 1))]).await;
}

async fn defers_follow_up_on_first_continuation(messages: Vec<AgentMessage>) {
    let (stream_fn, requests) = user_texts_recorder();
    let agent = agent_with(AgentOptions {
        initial_state: Some(AgentInitialState {
            messages: Some(messages),
            ..Default::default()
        }),
        ..options_with(stream_fn)
    });
    agent.follow_up(user("follow-up"));
    agent.continue_run().await.unwrap();
    let requests = requests.lock();
    assert_eq!(requests.len(), 2);
    assert!(!requests[0].contains(&"follow-up".to_string()));
    assert!(requests[1].contains(&"follow-up".to_string()));
}

#[tokio::test]
async fn defers_follow_up_input_on_the_first_continuation_from_a_user_tail() {
    defers_follow_up_on_first_continuation(vec![user("existing user")]).await;
}

#[tokio::test]
async fn defers_follow_up_input_on_the_first_continuation_from_a_tool_result_tail() {
    defers_follow_up_on_first_continuation(vec![
        user("existing user"),
        tool_use_message(vec![tool_call("call-1", "noop", json!({}))]).into(),
        AgentMessage::ToolResult(ToolResultMessage {
            tool_call_id: "call-1".into(),
            tool_name: "noop".into(),
            content: vec![UserContentBlock::text("done")],
            is_error: false,
            timestamp: 1,
            ..Default::default()
        }),
    ])
    .await;
}

async fn polls_steering_at_continuation_startup(mode: QueueMode, expected_requests: usize) {
    let (stream_fn, requests) = user_texts_recorder();
    let agent = agent_with(AgentOptions {
        initial_state: Some(AgentInitialState {
            messages: Some(vec![user("existing")]),
            ..Default::default()
        }),
        steering_mode: Some(mode.clone()),
        ..options_with(stream_fn)
    });
    agent.steer(user("first"));
    agent.steer(user("second"));
    agent.continue_run().await.unwrap();
    let requests = requests.lock();
    assert_eq!(requests.len(), expected_requests);
    assert!(requests[0].contains(&"first".to_string()));
    if mode == QueueMode::OneAtATime {
        assert!(!requests[0].contains(&"second".to_string()));
        assert!(requests[1].contains(&"second".to_string()));
    } else {
        assert!(requests[0].contains(&"second".to_string()));
    }
}

#[tokio::test]
async fn polls_one_at_a_time_steering_at_continuation_startup() {
    polls_steering_at_continuation_startup(QueueMode::OneAtATime, 2).await;
}

#[tokio::test]
async fn polls_all_steering_at_continuation_startup() {
    polls_steering_at_continuation_startup(QueueMode::All, 1).await;
}

#[tokio::test]
async fn keeps_steering_ahead_of_follow_up_from_a_non_assistant_tail() {
    let (stream_fn, requests) = user_texts_recorder();
    let agent = agent_with(AgentOptions {
        initial_state: Some(AgentInitialState {
            messages: Some(vec![user("existing")]),
            ..Default::default()
        }),
        ..options_with(stream_fn)
    });
    agent.steer(user("steering"));
    agent.follow_up(user("follow-up"));
    agent.continue_run().await.unwrap();
    let requests = requests.lock();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains(&"steering".to_string()));
    assert!(!requests[0].contains(&"follow-up".to_string()));
    assert!(requests[1].contains(&"follow-up".to_string()));
}

async fn keeps_queues_on_failed_response(stop_reason: StopReason) {
    let queued_during_response = user("steering");
    let follow_up = user("follow-up");
    let agent = agent_with(AgentOptions {
        finish_turn: Some(hooks::finish_turn(|_, _| async {
            Some(AgentTurnDecision::Continue)
        })),
        ..options_with(mock_stream_fn(move |_, _, _| {
            let mut message = assistant_text(stop_reason.as_str());
            message.stop_reason = stop_reason;
            message.error_message = Some(stop_reason.as_str().into());
            respond(message)
        }))
    });
    agent.follow_up(follow_up.clone());
    let weak = agent.downgrade();
    let q = queued_during_response.clone();
    agent.subscribe_fn(move |event, _| {
        if let AgentEvent::MessageEnd {
            message: AgentMessage::Assistant(_),
        } = event
            && let Some(agent) = weak.upgrade()
        {
            agent.steer(q.clone());
        }
    });
    agent.prompt("start").await.unwrap();
    assert_eq!(agent.peek_queued_messages(), vec![queued_during_response]);
    agent.clear_steering_queue();
    assert_eq!(agent.peek_queued_messages(), vec![follow_up]);
}

#[tokio::test]
async fn keeps_queues_on_an_error_response_even_when_finish_turn_continues() {
    keeps_queues_on_failed_response(StopReason::Error).await;
}

#[tokio::test]
async fn keeps_queues_on_an_aborted_response_even_when_finish_turn_continues() {
    keeps_queues_on_failed_response(StopReason::Aborted).await;
}

#[tokio::test]
async fn keeps_queues_when_finish_turn_ends_the_run() {
    let queued_during_response = user("steering");
    let follow_up = user("follow-up");
    let agent = agent_with(AgentOptions {
        finish_turn: Some(hooks::finish_turn(|_, _| async {
            Some(AgentTurnDecision::End)
        })),
        ..options_with(done_stream_fn())
    });
    agent.follow_up(follow_up.clone());
    let weak = agent.downgrade();
    let q = queued_during_response.clone();
    agent.subscribe_fn(move |event, _| {
        if let AgentEvent::MessageEnd {
            message: AgentMessage::Assistant(_),
        } = event
            && let Some(agent) = weak.upgrade()
        {
            agent.steer(q.clone());
        }
    });
    agent.prompt("start").await.unwrap();
    assert_eq!(agent.peek_queued_messages(), vec![queued_during_response]);
    agent.clear_steering_queue();
    assert_eq!(agent.peek_queued_messages(), vec![follow_up]);
}

#[tokio::test]
async fn previews_the_next_selected_queued_messages_without_consuming_them() {
    let agent = agent_with(AgentOptions {
        steering_mode: Some(QueueMode::OneAtATime),
        follow_up_mode: Some(QueueMode::All),
        ..options_with(unused_stream_fn())
    });
    let first = user("first steering");
    let second = user("second steering");
    let follow_up = user("follow-up");
    agent.steer(first.clone());
    agent.steer(second);
    agent.follow_up(follow_up.clone());
    assert_eq!(agent.peek_queued_messages(), vec![first.clone()]);
    assert_eq!(agent.peek_queued_messages(), vec![first]);
    agent.clear_steering_queue();
    assert_eq!(agent.peek_queued_messages(), vec![follow_up]);
}

#[tokio::test]
async fn forwards_provider_stream_event_observers() {
    let provider_events = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let p = provider_events.clone();
    let agent = agent_with(AgentOptions {
        on_provider_stream_event: Some(Arc::new(move |data, _| {
            p.lock().push(data);
            Box::pin(async {})
        })),
        ..options_with(mock_stream_fn(|model, _, options| {
            if let Some(observer) = &options.on_provider_stream_event {
                futures::executor::block_on(observer(json!({ "request_cost": 0.01 }), model.clone()));
            }
            respond(assistant_text("ok"))
        }))
    });
    agent.prompt("hello").await.unwrap();
    assert_eq!(
        *provider_events.lock(),
        vec![json!({ "request_cost": 0.01 })]
    );
}

#[tokio::test]
async fn forwards_session_id_to_stream_options() {
    let received = Arc::new(Mutex::new(None::<String>));
    let r = received.clone();
    let agent = agent_with(AgentOptions {
        session_id: Some("session-abc".into()),
        ..options_with(mock_stream_fn(move |_, _, options| {
            *r.lock() = options.session_id.clone();
            respond(assistant_text("ok"))
        }))
    });
    agent.prompt("hello").await.unwrap();
    assert_eq!(received.lock().as_deref(), Some("session-abc"));
    agent.set_session_id(Some("session-def".into()));
    assert_eq!(agent.session_id().as_deref(), Some("session-def"));
    agent.prompt("hello again").await.unwrap();
    assert_eq!(received.lock().as_deref(), Some("session-def"));
}

#[tokio::test]
async fn attributes_requests_to_pi_by_default_and_merges_a_configured_identity() {
    let received = Arc::new(Mutex::new(Vec::<Option<ClientIdentity>>::new()));
    let r = received.clone();
    let agent = agent_with(options_with(mock_stream_fn(move |_, _, options| {
        r.lock().push(options.identity.clone());
        respond(assistant_text("ok"))
    })));
    agent.prompt("hello").await.unwrap();
    let acme = ClientAttribution {
        name: "acme".into(),
        url: "https://acme.example".into(),
        categories: None,
    };
    let short = Some(UserAgentMode::Named(UserAgentNamed::Short));
    agent.set_identity(Some(ClientIdentity {
        user_agent: short.clone(),
        attribution: Some(Some(acme.clone())),
        version: None,
    }));
    agent.prompt("hello again").await.unwrap();
    agent.set_identity(Some(ClientIdentity {
        user_agent: short.clone(),
        ..Default::default()
    }));
    agent.prompt("and again").await.unwrap();
    assert_eq!(
        *received.lock(),
        vec![
            Some(ClientIdentity {
                attribution: Some(Some(pi_attribution())),
                ..Default::default()
            }),
            Some(ClientIdentity {
                attribution: Some(Some(acme)),
                user_agent: short.clone(),
                version: None
            }),
            Some(ClientIdentity {
                attribution: Some(Some(pi_attribution())),
                user_agent: short,
                version: None
            }),
        ]
    );
}

/// Rust-specific: listener replacements at `message_end` reach the loop's own copy (TS mutates the
/// shared message object in place).
#[tokio::test]
async fn message_end_replacements_reach_tool_execution_state_and_later_events() {
    let executed = Arc::new(Mutex::new(Vec::<String>::new()));
    let (stream_fn, _) = scripted_stream_fn(|index| {
        if index == 0 {
            assistant_tool_calls(
                vec![tool_call("call-1", "echo", json!({ "value": "original" }))],
                StopReason::ToolUse,
            )
        } else {
            assistant_text("done")
        }
    });
    let agent = agent_with(AgentOptions {
        initial_state: Some(AgentInitialState {
            tools: Some(vec![echo_tool(executed.clone())]),
            ..Default::default()
        }),
        ..options_with(stream_fn)
    });
    let handle = agent.clone();
    let turn_end_texts = Arc::new(Mutex::new(Vec::<String>::new()));
    let agent_end_results = Arc::new(Mutex::new(Vec::<String>::new()));
    let (te, ae) = (turn_end_texts.clone(), agent_end_results.clone());
    let _sub = agent.subscribe_fn(move |event, _| match event {
        AgentEvent::MessageEnd {
            message: AgentMessage::Assistant(message),
        } if message.tool_calls().next().is_some() => {
            let mut replaced = message.clone();
            replaced.content = vec![pi_ai::AssistantContent::ToolCall(tool_call(
                "call-1",
                "echo",
                json!({ "value": "replaced" }),
            ))].into();
            handle.replace_event_message(AgentMessage::Assistant(replaced));
        }
        AgentEvent::MessageEnd {
            message: AgentMessage::ToolResult(message),
        } => {
            let mut replaced = message.clone();
            replaced.content = vec![pi_ai::ToolResultContent::Text(pi_ai::TextContent::new(
                "patched result",
            ))];
            handle.replace_event_message(AgentMessage::ToolResult(replaced));
        }
        AgentEvent::TurnEnd { tool_results, .. } => {
            for result in tool_results {
                te.lock().push(
                    result
                        .content
                        .iter()
                        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
                        .collect(),
                );
            }
        }
        AgentEvent::AgentEnd { messages } => {
            for message in messages {
                if let AgentMessage::ToolResult(result) = message {
                    ae.lock().push(
                        result
                            .content
                            .iter()
                            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
                            .collect(),
                    );
                }
            }
        }
        _ => {}
    });

    agent.prompt("go").await.unwrap();

    assert_eq!(*executed.lock(), vec!["replaced".to_string()]);
    assert_eq!(*turn_end_texts.lock(), vec!["patched result".to_string()]);
    assert_eq!(*agent_end_results.lock(), vec!["patched result".to_string()]);
    let state_result = agent
        .messages()
        .into_iter()
        .find_map(|m| match m {
            AgentMessage::ToolResult(r) => Some(r),
            _ => None,
        })
        .unwrap();
    assert_eq!(state_result.content[0].as_text().unwrap().text, "patched result");
}

/// Concurrency regression: a listener may unsubscribe itself (and re-enter the agent) while the
/// agent is emitting to it; emission works on a snapshot and holds no lock during listeners.
#[tokio::test]
async fn listener_can_unsubscribe_itself_and_reenter_the_agent_during_emit() {
    let agent = agent_with(options_with(done_stream_fn()));
    let first = Arc::new(AtomicUsize::new(0));
    let other = Arc::new(AtomicUsize::new(0));
    let slot: Arc<Mutex<Option<pi_agent::agent::Subscription>>> = Arc::new(Mutex::new(None));
    let weak = agent.downgrade();
    let (f, s) = (first.clone(), slot.clone());
    let subscription = agent.subscribe_fn(move |_, _| {
        f.fetch_add(1, Ordering::SeqCst);
        if let Some(agent) = weak.upgrade() {
            let _ = agent.messages();
            let _ = agent.is_streaming();
            agent.set_tools(agent.tools());
        }
        let own = s.lock().take();
        if let Some(own) = own {
            own.unsubscribe();
        }
    });
    *slot.lock() = Some(subscription);
    let o = other.clone();
    agent.subscribe_fn(move |_, _| {
        o.fetch_add(1, Ordering::SeqCst);
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), agent.prompt("hello"))
        .await
        .expect("agent emit deadlocked")
        .unwrap();
    assert_eq!(first.load(Ordering::SeqCst), 1);
    assert!(other.load(Ordering::SeqCst) > 1);
}
