//! JSON/RPC wire projection of session events: ports of
//! `test/suite/regressions/7290-json-stream-linear.test.ts` and
//! `test/suite/regressions/7911-json-stream-usage.test.ts`.

mod support;

use pi_agent::types::AgentMessage;
use pi_coding_agent::core::agent_session::AgentSessionEvent;
use pi_coding_agent::modes::json_event::to_json_event_value;
use support::*;

/// Total wire bytes of the message updates of one streamed response.
async fn measure_update_bytes(text: &str) -> usize {
    let harness = create_harness(HarnessOptions::default()).await.unwrap();
    harness.set_responses(vec![faux_text_message(text).into()]);
    harness.session.prompt("respond", None).await.unwrap();

    let session_updates = harness.events_of_type("message_update");
    for update in &session_updates {
        assert!(update.get("message").is_some());
        assert!(update["assistantMessageEvent"].get("partial").is_some());
    }
    let updates: Vec<serde_json::Value> = harness
        .events()
        .iter()
        .filter(|event| matches!(event, AgentSessionEvent::MessageUpdate { .. }))
        .map(|event| to_json_event_value(event).unwrap())
        .collect();
    assert!(!updates.is_empty());
    for update in &updates {
        assert!(update.get("message").is_none());
        assert!(update["assistantMessageEvent"].get("partial").is_none());
    }
    harness.cleanup();
    updates.iter().map(|update| serde_json::to_string(update).unwrap().len()).sum()
}

// #7290
#[tokio::test]
async fn emits_delta_only_message_updates_whose_size_scales_linearly() {
    let small = measure_update_bytes(&"x".repeat(2_000)).await;
    let large = measure_update_bytes(&"x".repeat(4_000)).await;
    assert!(large > small);
    assert!((large as f64) / (small as f64) < 2.2, "{small} -> {large}");
}

// #7911
#[tokio::test]
async fn includes_cumulative_usage_without_cumulative_message_snapshots() {
    let harness = create_harness(HarnessOptions::default()).await.unwrap();
    harness.set_responses(vec![faux_text_message("hello").into()]);
    harness.session.prompt("respond", None).await.unwrap();

    // #7290's delta-only wire projection dropped this fixed-size metadata with the snapshots.
    let (update, usage) = harness
        .events()
        .into_iter()
        .find_map(|event| match &event {
            AgentSessionEvent::MessageUpdate { message: AgentMessage::Assistant(message), .. }
                if message.usage.total_tokens > 0 =>
            {
                let usage = message.usage.clone();
                Some((event, usage))
            }
            _ => None,
        })
        .expect("an assistant update with populated usage");
    let wire = to_json_event_value(&update).unwrap();
    assert_eq!(wire["usage"], serde_json::to_value(&usage).unwrap());
    assert!(wire.get("message").is_none());
    assert!(wire["assistantMessageEvent"].get("partial").is_none());
    harness.cleanup();
}

/// The JSON/RPC wire form of a tool-call turn keeps the discriminants TS objects carry:
/// `toolcall_end.toolCall.type` and `turn_end.toolResults[].role`.
#[tokio::test]
async fn tool_call_turns_keep_ts_discriminants_on_the_wire() {
    let harness = create_harness(HarnessOptions::default()).await.unwrap();
    harness.set_responses(vec![
        pi_ai::providers::faux::FauxResponseStep::from(faux_assistant_message(
            vec![faux_tool_call(
                "read",
                serde_json::json!({ "path": "missing.txt" })
                    .as_object()
                    .unwrap()
                    .clone(),
                Some("call_1"),
            )],
            FauxMessageOptions {
                stop_reason: Some(pi_ai::types::StopReason::ToolUse),
                ..Default::default()
            },
        )),
        faux_text_message("done").into(),
    ]);
    harness.session.prompt("read it", None).await.unwrap();
    let wire: Vec<serde_json::Value> = harness
        .events()
        .iter()
        .map(|event| to_json_event_value(event).unwrap())
        .collect();
    let tool_call_end = wire
        .iter()
        .find(|event| {
            event["type"] == "message_update"
                && event["assistantMessageEvent"]["type"] == "toolcall_end"
        })
        .expect("toolcall_end update");
    let tool_call = &tool_call_end["assistantMessageEvent"]["toolCall"];
    assert_eq!(tool_call["type"], "toolCall");
    assert_eq!(tool_call["id"], "call_1");
    assert_eq!(tool_call["name"], "read");
    let turn_end = wire
        .iter()
        .find(|event| {
            event["type"] == "turn_end"
                && event["toolResults"]
                    .as_array()
                    .is_some_and(|r| !r.is_empty())
        })
        .expect("turn_end with tool results");
    assert_eq!(turn_end["toolResults"][0]["role"], "toolResult");
    assert_eq!(turn_end["toolResults"][0]["toolCallId"], "call_1");
    harness.cleanup();
}
