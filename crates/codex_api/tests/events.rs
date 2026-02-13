use codex_api::events::{CodexResponseStatus, CodexStreamEvent};

#[test]
fn events_status_from_str_parity() {
    assert_eq!(
        CodexResponseStatus::parse("completed"),
        Some(CodexResponseStatus::Completed)
    );
    assert_eq!(
        CodexResponseStatus::parse("in_progress"),
        Some(CodexResponseStatus::InProgress)
    );
    assert_eq!(
        CodexResponseStatus::parse("queued"),
        Some(CodexResponseStatus::Queued)
    );
    assert_eq!(CodexResponseStatus::parse("unknown"), None);
}

#[test]
fn events_variant_shapes_stable() {
    let event = CodexStreamEvent::ResponseCompleted {
        status: Some(CodexResponseStatus::Completed),
    };
    assert_eq!(status_string(&event), Some("completed"));
}

#[test]
fn tool_call_variant_preserves_optional_fields() {
    let event = CodexStreamEvent::ToolCallRequested {
        id: Some("fc_1".to_string()),
        call_id: Some("call_1".to_string()),
        tool_name: Some("read".to_string()),
        arguments: Some(serde_json::json!({"path": "README.md"})),
    };

    let json = serde_json::to_value(&event).expect("serialize tool call event");
    assert_eq!(json["type"], "response.output_item.function_call");
    assert_eq!(json["call_id"], "call_1");
    assert_eq!(json["tool_name"], "read");
    assert_eq!(json["arguments"]["path"], "README.md");
}

fn status_string(event: &CodexStreamEvent) -> Option<&'static str> {
    match event {
        CodexStreamEvent::ResponseCompleted { status } => status.map(|value| value.as_str()),
        _ => None,
    }
}
