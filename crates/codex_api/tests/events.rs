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

fn status_string(event: &CodexStreamEvent) -> Option<&'static str> {
    match event {
        CodexStreamEvent::ResponseCompleted { status } => status.map(|value| value.as_str()),
        _ => None,
    }
}
