use codex_api::events::{CodexResponseStatus, CodexStreamEvent};

#[test]
fn stream_terminal_status_is_reported_from_completed_events() {
    let events = vec![
        CodexStreamEvent::OutputTextDelta {
            delta: "hello".to_owned(),
        },
        CodexStreamEvent::ResponseCompleted {
            status: CodexResponseStatus::Completed,
        },
    ];

    assert_eq!(
        terminal_status(&events),
        Some(CodexResponseStatus::Completed)
    );
}

#[test]
fn stream_terminal_status_is_failed_when_error_seen() {
    let events = vec![CodexStreamEvent::Error {
        code: Some("x".to_owned()),
        message: Some("bad".to_owned()),
    }];

    assert_eq!(terminal_status(&events), Some(CodexResponseStatus::Failed));
}

#[test]
fn stream_terminal_status_respects_queued_and_in_progress() {
    let queued = vec![CodexStreamEvent::ResponseCompleted {
        status: CodexResponseStatus::Queued,
    }];
    let in_progress = vec![CodexStreamEvent::ResponseCompleted {
        status: CodexResponseStatus::InProgress,
    }];
    let incomplete = vec![CodexStreamEvent::ResponseCompleted {
        status: CodexResponseStatus::Incomplete,
    }];

    assert_eq!(terminal_status(&queued), Some(CodexResponseStatus::Queued));
    assert_eq!(
        terminal_status(&in_progress),
        Some(CodexResponseStatus::InProgress)
    );
    assert_eq!(
        terminal_status(&incomplete),
        Some(CodexResponseStatus::Incomplete)
    );
}

#[test]
fn stream_terminal_status_defaults_to_incomplete_without_terminal_event() {
    let events = vec![CodexStreamEvent::OutputTextDelta {
        delta: "hello".to_owned(),
    }];

    assert_eq!(
        terminal_status(&events),
        Some(CodexResponseStatus::Incomplete)
    );
}

fn terminal_status(events: &[CodexStreamEvent]) -> Option<CodexResponseStatus> {
    events.iter().rev().find_map(|event| match event {
        CodexStreamEvent::ResponseCompleted { status } => Some(*status),
        CodexStreamEvent::ResponseFailed { .. } => Some(CodexResponseStatus::Failed),
        CodexStreamEvent::Error { .. } => Some(CodexResponseStatus::Failed),
        _ => None,
    })
    .or(Some(CodexResponseStatus::Incomplete))
}
