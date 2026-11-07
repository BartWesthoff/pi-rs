use pi_ai::*;
use serde_json::json;

#[test]
fn assistant_message_roundtrip_matches_ts_shape() {
    let raw = json!({
        "role": "assistant",
        "content": [
            {"type": "thinking", "thinking": "hmm", "thinkingSignature": "sig"},
            {"type": "text", "text": "hi"},
            {"type": "toolCall", "id": "c1", "name": "read", "arguments": {"path": "a.ts"}}
        ],
        "api": "anthropic-messages",
        "provider": "anthropic",
        "model": "claude",
        "usage": {"input": 1, "output": 2, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 3,
                  "cost": {"input": 0.5, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0.5}},
        "stopReason": "toolUse",
        "timestamp": 1700000000000i64
    });
    let message: Message = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(serde_json::to_value(&message).unwrap(), raw);
    assert_eq!(serde_json::to_string(&message).unwrap(), serde_json::to_string(&raw).unwrap());
}

#[test]
fn lax_null_content_and_preserved_null_details() {
    let user: Message = serde_json::from_value(json!({"role": "user", "content": null, "timestamp": 1})).unwrap();
    assert_eq!(user.as_user().unwrap().content, UserContent::Blocks(vec![]));
    let tr: Message = serde_json::from_value(json!({
        "role": "toolResult", "toolCallId": "c", "toolName": "t", "isError": false, "timestamp": 1, "details": null
    }))
    .unwrap();
    let tr = tr.as_tool_result().unwrap();
    assert!(tr.content.is_empty());
    assert_eq!(tr.details, Some(serde_json::Value::Null));
    let out = serde_json::to_value(tr).unwrap();
    assert_eq!(out["details"], serde_json::Value::Null);
}

#[test]
fn events_serialize_with_ts_discriminants() {
    let partial = AssistantMessage::default();
    let event = AssistantMessageEvent::ToolCallDelta { content_index: 2, delta: "{".into(), partial };
    let v = serde_json::to_value(&event).unwrap();
    assert_eq!(v["type"], "toolcall_delta");
    assert_eq!(v["contentIndex"], 2);
    let back: AssistantMessageEvent = serde_json::from_value(v).unwrap();
    assert_eq!(back.type_name(), "toolcall_delta");
}

#[test]
fn toolcall_end_carries_the_tagged_tool_call_block() {
    // TS passes the content block itself, so `toolCall` keeps its `type` discriminant.
    let tool_call = ToolCall {
        id: "call_1".into(),
        name: "read".into(),
        arguments: json!({"path": "a.txt"}).as_object().unwrap().clone(),
        ..Default::default()
    };
    let event = AssistantMessageEvent::ToolCallEnd {
        content_index: 1,
        tool_call: tool_call.clone(),
        partial: AssistantMessage::default(),
    };
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(
        value["toolCall"],
        json!({"type": "toolCall", "id": "call_1", "name": "read", "arguments": {"path": "a.txt"}})
    );
    let text = serde_json::to_string(&value["toolCall"]).unwrap();
    assert!(
        text.starts_with(r#"{"type":"toolCall","id":"call_1""#),
        "{text}"
    );
    let AssistantMessageEvent::ToolCallEnd {
        tool_call: back, ..
    } = serde_json::from_value(value).unwrap()
    else {
        panic!("toolcall_end round trip");
    };
    assert_eq!(back, tool_call);
}

#[test]
fn tool_constrained_sampling_false() {
    let tool: Tool = serde_json::from_value(json!({
        "name": "t", "description": "d", "parameters": {"type": "object"}, "constrainedSampling": false
    }))
    .unwrap();
    assert_eq!(tool.constrained_sampling, Some(ConstrainedSampling::Disabled));
    assert_eq!(serde_json::to_value(&tool).unwrap()["constrainedSampling"], json!(false));
    let tool: Tool = serde_json::from_value(json!({
        "name": "t", "description": "d", "parameters": {},
        "constrainedSampling": {"type": "grammar", "variants": {"openai_lark": "x"}}
    }))
    .unwrap();
    assert!(matches!(tool.constrained_sampling, Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::Grammar { .. }))));
}

#[test]
fn any_model_dispatches_on_type() {
    let m: AnyModel = serde_json::from_value(json!({
        "id": "x", "name": "X", "api": "openrouter-images", "provider": "openrouter", "baseUrl": "u",
        "input": ["text"], "output": ["image"], "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
        "type": "image"
    }))
    .unwrap();
    assert_eq!(m.model_type(), ModelType::Image);
}

#[tokio::test]
async fn event_stream_delivers_events_and_result() {
    let (tx, mut stream) = AssistantMessageEventStream::new();
    let msg = AssistantMessage { timestamp: utils::now_ms(), ..Default::default() };
    tx.push(AssistantMessageEvent::Start { partial: msg.clone() });
    tx.push(AssistantMessageEvent::Done { reason: StopReason::Stop, message: msg.clone() });
    tx.push(AssistantMessageEvent::Start { partial: msg.clone() }); // ignored after done
    let mut types = vec![];
    while let Some(e) = stream.next_event().await {
        types.push(e.type_name());
    }
    assert_eq!(types, vec!["start", "done"]);
    let result = stream.result().await;
    assert!(result.duration_ms.is_some());
}
