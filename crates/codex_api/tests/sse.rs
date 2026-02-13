use codex_api::{events::CodexResponseStatus, events::CodexStreamEvent, SseStreamParser};

#[test]
fn sse_framing_parses_done_and_deltas() {
    let payload = concat!(
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hel\"}\n\n",
        "data: [DONE]\n\n",
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"ok\"}\n\n"
    );

    let events = SseStreamParser::parse_frames(payload);
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[0],
        CodexStreamEvent::OutputTextDelta { .. }
    ));
    assert!(matches!(
        events[1],
        CodexStreamEvent::ReasoningSummaryTextDelta { .. }
    ));
}

#[test]
fn sse_parser_maps_done_alias_and_failed() {
    let payload = concat!(
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
        "data: {\"type\":\"response.done\",\"response\":{\"status\":\"in_progress\"}}\n\n",
        "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"boom\"}}}\n\n"
    );

    let events = SseStreamParser::parse_frames(payload);
    assert_eq!(events.len(), 3);

    if let CodexStreamEvent::ResponseCompleted { status } = &events[0] {
        assert_eq!(*status, CodexResponseStatus::Completed);
    } else {
        panic!("first event should be completed");
    }

    if let CodexStreamEvent::ResponseCompleted { status } = &events[1] {
        assert_eq!(*status, CodexResponseStatus::InProgress);
    } else {
        panic!("second event should be done alias");
    }

    assert!(matches!(events[2], CodexStreamEvent::ResponseFailed { .. }));
}

#[test]
fn sse_parser_ignores_unknown_and_malformed() {
    let payload = concat!(
        "data: {\"type\":\"unknown.event\",\"foo\":\"bar\"}\n\n",
        "data: {broken-json\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"x\"}\n\n"
    );

    let events = SseStreamParser::parse_frames(payload);
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0],
        CodexStreamEvent::OutputTextDelta { .. }
    ));
}

#[test]
fn sse_parser_handles_split_frames_incrementally() {
    let mut parser = SseStreamParser::default();
    assert!(parser
        .feed(b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"abc\"")
        .is_empty());
    let mut events = parser.feed(b"}\n\n");
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events.pop(),
        Some(CodexStreamEvent::OutputTextDelta { .. })
    ));
}

#[test]
fn sse_parser_skips_empty_data_frames() {
    let payload = concat!(
        "data: \n\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"done\"}\n\n"
    );
    let events = SseStreamParser::parse_frames(payload);
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0],
        CodexStreamEvent::OutputTextDelta { .. }
    ));
}

#[test]
fn sse_parser_ignores_incomplete_trailing_bytes() {
    let mut parser = SseStreamParser::default();
    assert!(parser
        .feed(b"data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"nope\"")
        .is_empty());
    assert!(!parser.is_empty_buffer());
}
