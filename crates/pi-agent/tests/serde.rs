//! JSON compatibility of agent messages and events.

use pi_agent::{AgentEvent, AgentMessage, AgentToolResult, CustomMessage};
use pi_ai::{ToolResultMessage, UserContentBlock};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[test]
fn custom_messages_round_trip_exactly() {
    let raw = r#"{"role":"bashExecution","command":"ls","output":"a\nb","exitCode":0,"cancelled":false,"truncated":false,"timestamp":1700000000000,"excludeFromContext":true}"#;
    let message: AgentMessage = serde_json::from_str(raw).unwrap();
    assert_eq!(message.role(), "bashExecution");
    assert_eq!(message.timestamp(), 1700000000000);
    assert!(message.is_custom());
    assert_eq!(serde_json::to_string(&message).unwrap(), raw);
}

#[test]
fn llm_messages_round_trip() {
    let raw = r#"{"role":"user","content":"hello","timestamp":5}"#;
    let message: AgentMessage = serde_json::from_str(raw).unwrap();
    assert!(matches!(message, AgentMessage::User(_)));
    assert_eq!(serde_json::to_string(&message).unwrap(), raw);

    let raw = r#"{"role":"toolResult","toolCallId":"c1","toolName":"bash","content":[{"type":"text","text":"ok"}],"details":null,"isError":false,"timestamp":7}"#;
    let message: AgentMessage = serde_json::from_str(raw).unwrap();
    assert_eq!(serde_json::to_string(&message).unwrap(), raw);
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BranchSummary {
    role: String,
    summary: String,
    from_id: String,
    timestamp: i64,
}

#[test]
fn custom_messages_convert_to_typed_values() {
    let typed = BranchSummary {
        role: "branchSummary".into(),
        summary: "s".into(),
        from_id: "abc".into(),
        timestamp: 3,
    };
    let message = AgentMessage::custom(&typed).unwrap();
    assert_eq!(message.role(), "branchSummary");
    assert_eq!(
        message.to_value(),
        json!({ "role": "branchSummary", "summary": "s", "fromId": "abc", "timestamp": 3 })
    );
    let back: BranchSummary = message.as_custom().unwrap().to_typed().unwrap();
    assert_eq!(back, typed);
    assert_eq!(CustomMessage::from_typed(&typed).unwrap().timestamp(), 3);
}

#[test]
fn events_use_typescript_shapes() {
    let event = AgentEvent::ToolExecutionEnd {
        tool_call_id: "c1".into(),
        tool_name: "bash".into(),
        result: AgentToolResult {
            content: vec![UserContentBlock::text("ok")],
            details: Some(json!({})),
            ..Default::default()
        },
        is_error: false,
        duration_ms: None,
    };
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "type": "tool_execution_end",
            "toolCallId": "c1",
            "toolName": "bash",
            "result": { "content": [{ "type": "text", "text": "ok" }], "details": {} },
            "isError": false
        })
    );
    let event = AgentEvent::TurnEnd {
        message: AgentMessage::from_value(json!({ "role": "custom", "timestamp": 1 })).unwrap(),
        tool_results: vec![],
    };
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({ "type": "turn_end", "message": { "role": "custom", "timestamp": 1 }, "toolResults": [] })
    );
    // TS tool result messages keep their `role` discriminant in `turn_end`.
    let event = AgentEvent::TurnEnd {
        message: AgentMessage::from_value(json!({ "role": "custom", "timestamp": 1 })).unwrap(),
        tool_results: vec![ToolResultMessage {
            tool_call_id: "c1".into(),
            tool_name: "bash".into(),
            content: vec![UserContentBlock::text("ok")],
            timestamp: 2,
            ..Default::default()
        }],
    };
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(
        value["toolResults"],
        json!([{ "role": "toolResult", "toolCallId": "c1", "toolName": "bash", "content": [{ "type": "text", "text": "ok" }], "isError": false, "timestamp": 2 }])
    );
    assert!(
        serde_json::to_string(&value["toolResults"][0])
            .unwrap()
            .starts_with(r#"{"role":"toolResult","toolCallId":"c1""#)
    );
    let AgentEvent::TurnEnd { tool_results, .. } = serde_json::from_value(value).unwrap() else {
        panic!("turn_end round trip")
    };
    assert_eq!(tool_results[0].tool_call_id, "c1");
    assert_eq!(
        serde_json::to_value(AgentEvent::AgentStart).unwrap(),
        json!({ "type": "agent_start" })
    );
    let parsed: AgentEvent = serde_json::from_value(json!({
        "type": "tool_execution_start", "toolCallId": "x", "toolName": "y", "args": { "a": 1 }
    }))
    .unwrap();
    assert_eq!(parsed.type_name(), "tool_execution_start");
    let value: Value = serde_json::to_value(&parsed).unwrap();
    assert_eq!(value["args"], json!({ "a": 1 }));
}
