//! Port of `test/agent-session-concurrent.test.ts`, on the faux harness.
//!
//! The TS file swaps the session's private extension runner for a stub in two cases; the port uses
//! real extension handlers with the same timing instead (`tool_call` snapshots and a slow
//! `message_end` handler).

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use pi_agent::types::{AgentMessage, AgentToolResult, FnAgentTool};
use pi_ai::types::{AssistantMessage, Message, StopReason, UserContent};
use pi_coding_agent::core::agent_session::{
    AgentSessionEvent, AgentSessionEventListener, QueuedInputDisposition,
};
use pi_coding_agent::core::extensions::types::{
    ExtensionAPIRef, InputEvent, InputSource, MessageEndEvent, SendUserMessageOptions,
    StreamingBehavior, ToolCallEvent, on_event,
};
use pi_coding_agent::core::session_manager::SessionEntry;
use serde_json::{Value, json};

use crate::common::*;
use crate::support::*;

/// A stream that starts and only ends once the request is aborted (TS `checkAbort` loop).
fn install_hanging_stream(harness: &Harness, steer_reply: Option<&'static str>) -> Arc<Mutex<bool>> {
    let saw_steer = Arc::new(Mutex::new(false));
    let s = saw_steer.clone();
    harness.session.agent().set_stream_fn(Some(Arc::new(move |model, context, options| {
        let (sender, stream) = pi_ai::AssistantMessageEventStream::new();
        let partial = |text: &str| -> AssistantMessage {
            let mut m = faux_text_message(text);
            m.api = model.api.clone();
            m.provider = model.provider.clone();
            m.model = model.id.clone();
            m
        };
        let users: Vec<String> = context
            .messages
            .iter()
            .filter(|m| matches!(m, Message::User(_)))
            .map(llm_text)
            .collect();
        if let Some(steer) = steer_reply
            && users.iter().any(|u| u == steer)
        {
            *s.lock() = true;
            let done = partial("Steered");
            sender.push(pi_ai::AssistantMessageEvent::Start { partial: partial("") });
            sender.push(pi_ai::AssistantMessageEvent::Done {
                reason: StopReason::Stop,
                message: done,
            });
            return Box::pin(async move { Ok(stream) });
        }
        let start = partial("");
        // Like TS, the error event carries `createAssistantMessage("Aborted")`, whose stop reason
        // is "stop"; the loop therefore keeps going and delivers queued steering.
        let aborted = partial("Aborted");
        let signal = options.signal.clone();
        tokio::spawn(async move {
            sender.push(pi_ai::AssistantMessageEvent::Start { partial: start });
            if let Some(signal) = signal {
                signal.cancelled().await;
            }
            sender.push(pi_ai::AssistantMessageEvent::Error {
                reason: StopReason::Aborted,
                error: aborted,
            });
        });
        Box::pin(async move { Ok(stream) })
    })));
    saw_steer
}

async fn wait_for_streaming(harness: &Harness) {
    for _ in 0..1000 {
        if harness.session.is_streaming() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("Timed out waiting for streaming");
}

async fn streaming_harness(options: HarnessOptions) -> (Arc<Harness>, tokio::task::JoinHandle<anyhow::Result<()>>) {
    let harness = Arc::new(create_harness(options).await.unwrap());
    install_hanging_stream(&harness, None);
    let session = harness.session.clone();
    let first = tokio::spawn(async move { session.prompt("First message", None).await });
    wait_for_streaming(&harness).await;
    (harness, first)
}

#[tokio::test]
async fn throws_when_prompt_is_called_while_streaming() {
    let (harness, first) = streaming_harness(HarnessOptions::default()).await;

    let error = harness.session.prompt("Second message", None).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message."
    );

    harness.session.abort().await.unwrap();
    let _ = first.await;
    harness.cleanup();
}

#[tokio::test]
async fn allows_steer_and_follow_up_while_streaming() {
    for steer in [true, false] {
        let (harness, first) = streaming_harness(HarnessOptions::default()).await;
        let disposition = if steer {
            harness.session.steer("Steering message", None, None).await.unwrap()
        } else {
            harness.session.follow_up("Follow-up message", None, None).await.unwrap()
        };
        assert_eq!(disposition, QueuedInputDisposition::Queued);
        assert_eq!(harness.session.pending_message_count(), 1);
        harness.session.abort().await.unwrap();
        let _ = first.await;
        harness.cleanup();
    }
}

#[tokio::test]
async fn queues_extension_origin_steering_messages_while_streaming() {
    let api = Arc::new(Mutex::new(None::<ExtensionAPIRef>));
    let last_source = Arc::new(Mutex::new(None::<InputSource>));
    let (a, l) = (api.clone(), last_source.clone());
    let harness = Arc::new(
        create_harness(HarnessOptions {
            extension_factories: vec![
                ext(move |pi| {
                    *a.lock() = Some(pi);
                    Ok(())
                }),
                ext(move |pi| {
                    let l = l.clone();
                    on_event::<InputEvent, _, _>(&*pi, move |event, _ctx| {
                        let l = l.clone();
                        async move {
                            *l.lock() = Some(event.source);
                            Ok(None)
                        }
                    })?;
                    Ok(())
                }),
            ],
            ..Default::default()
        })
        .await
        .unwrap(),
    );
    let saw_steer = install_hanging_stream(&harness, Some("Steer from extension"));
    let queue_events = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
    let q = queue_events.clone();
    let listener: AgentSessionEventListener = Arc::new(move |event| {
        if let AgentSessionEvent::QueueUpdate { steering, .. } = &event {
            q.lock().push(steering.clone());
        }
    });
    harness.session.subscribe(&listener);
    let session = harness.session.clone();
    let first = tokio::spawn(async move { session.prompt("First message", None).await });
    wait_for_streaming(&harness).await;

    let pi = api.lock().clone().unwrap();
    pi.send_user_message(
        UserContent::Text("Steer from extension".into()),
        SendUserMessageOptions {
            deliver_as: Some(StreamingBehavior::Steer),
            expand_prompt_templates: None,
        },
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(25)).await;

    assert_eq!(harness.session.pending_message_count(), 1);
    assert!(harness.session.get_steering_messages().contains(&"Steer from extension".to_string()));
    assert_eq!(*last_source.lock(), Some(InputSource::Extension));
    assert!(
        queue_events
            .lock()
            .iter()
            .any(|steering| steering.contains(&"Steer from extension".to_string()))
    );

    harness.session.abort().await.unwrap();
    let _ = first.await;
    assert!(*saw_steer.lock());
    harness.cleanup();
}

#[tokio::test]
async fn allows_prompt_after_previous_completes() {
    let harness = create_harness(HarnessOptions::default()).await.unwrap();
    harness.set_responses(vec![text("Done"), text("Done")]);

    harness.session.prompt("First message", None).await.unwrap();
    assert!(!harness.session.is_streaming());
    harness.session.prompt("Second message", None).await.unwrap();
    harness.cleanup();
}

fn dummy_tool() -> pi_agent::types::AgentToolRef {
    FnAgentTool::new(
        "dummy",
        "dummy",
        "Dummy tool",
        json!({"type": "object", "properties": {"q": {"type": "string"}}, "required": ["q"]}),
        |_id, params: Value, _s, _u| async move {
            let q = params["q"].as_str().unwrap_or_default().to_string();
            Ok(AgentToolResult::text(format!("result:{q}"), Some(json!({}))))
        },
    )
    .into_ref()
}

fn message_roles(harness: &Harness) -> Vec<String> {
    harness
        .session_manager
        .get_entries()
        .iter()
        .filter_map(|e| match e {
            SessionEntry::Message(m) => Some(m.message.role().to_string()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn waits_for_queued_agent_events_before_emitting_tool_call() {
    let snapshots = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
    let manager_slot = Arc::new(Mutex::new(None::<Arc<pi_coding_agent::core::session_manager::SessionManager>>));
    let (s, m) = (snapshots.clone(), manager_slot.clone());
    let harness = create_harness(HarnessOptions {
        tools: Some(vec![dummy_tool()]),
        extension_factories: vec![ext(move |pi| {
            let (s, m) = (s.clone(), m.clone());
            on_event::<ToolCallEvent, _, _>(&*pi, move |_event, ctx| {
                let (s, m) = (s.clone(), m.clone());
                async move {
                    let manager = m.lock().clone().unwrap_or(ctx.session_manager()?);
                    s.lock().push(
                        manager
                            .get_entries()
                            .iter()
                            .filter_map(|e| match e {
                                SessionEntry::Message(m) => Some(m.message.role().to_string()),
                                _ => None,
                            })
                            .collect(),
                    );
                    Ok(None)
                }
            })?;
            Ok(())
        })],
        ..Default::default()
    })
    .await
    .unwrap();
    harness.set_responses(vec![
        message(
            vec![tool_call("dummy", json!({"q": "x"})), tool_call("dummy", json!({"q": "y"}))],
            Some(StopReason::ToolUse),
        )
        .into(),
        text("done"),
    ]);

    harness.session.prompt("hi", None).await.unwrap();
    harness.session.agent().wait_for_idle().await;

    assert_eq!(
        *snapshots.lock(),
        vec![
            vec!["system".to_string(), "user".into(), "assistant".into()],
            vec!["system".to_string(), "user".into(), "assistant".into()],
        ]
    );
    harness.cleanup();
}

#[tokio::test]
async fn persists_message_end_events_in_order_with_slow_extension_handlers() {
    let harness = create_harness(HarnessOptions {
        tools: Some(vec![dummy_tool()]),
        extension_factories: vec![ext(|pi| {
            on_event::<MessageEndEvent, _, _>(&*pi, |event, _ctx| async move {
                if matches!(event.message, AgentMessage::Assistant(_)) {
                    tokio::time::sleep(Duration::from_millis(40)).await;
                }
                Ok(None)
            })?;
            Ok(())
        })],
        ..Default::default()
    })
    .await
    .unwrap();
    harness.set_responses(vec![
        message(
            vec![faux_text("calling tool"), tool_call("dummy", json!({"q": "x"}))],
            Some(StopReason::ToolUse),
        )
        .into(),
        text("done"),
    ]);

    harness.session.prompt("hi", None).await.unwrap();
    harness.session.agent().wait_for_idle().await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert_eq!(message_roles(&harness), ["system", "user", "assistant", "toolResult", "assistant"]);
    harness.cleanup();
}

/// RPC `abort` answers once the aborted run settles (TS resolves its idle-wait promise at that
/// transition), even when a client already started the next prompt before the waiter ran.
#[tokio::test]
async fn idle_waiters_resolve_at_the_settle_even_if_a_new_prompt_starts() {
    let (harness, first) = streaming_harness(HarnessOptions::default()).await;
    let session = harness.session.clone();
    let wait = session.wait_for_idle();
    tokio::pin!(wait);
    assert!(
        futures::poll!(&mut wait).is_pending(),
        "the session is streaming"
    );

    // The aborted run settles while the waiter is not polled; the next prompt then starts.
    harness.session.agent().abort();
    first.await.unwrap().unwrap();
    assert!(harness.session.is_idle());
    let next_session = harness.session.clone();
    let second = tokio::spawn(async move { next_session.prompt("Next message", None).await });
    wait_for_streaming(&harness).await;

    tokio::time::timeout(Duration::from_secs(2), &mut wait)
        .await
        .expect("the waiter resolves at the settle it was waiting for")
        .unwrap();
    harness.session.abort().await.unwrap();
    let _ = second.await;
    harness.cleanup();
}
