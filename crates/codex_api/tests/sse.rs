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
        assert_eq!(*status, Some(CodexResponseStatus::Completed));
    } else {
        panic!("first event should be completed");
    }

    if let CodexStreamEvent::ResponseCompleted { status } = &events[1] {
        assert_eq!(*status, Some(CodexResponseStatus::InProgress));
    } else {
        panic!("second event should be done alias");
    }

    assert!(matches!(events[2], CodexStreamEvent::ResponseFailed { .. }));
}

#[test]
fn sse_parser_maps_function_call_output_item_to_tool_call_event() {
    let payload = concat!(
        "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n"
    );

    let events = SseStreamParser::parse_frames(payload);
    assert_eq!(events.len(), 3);

    assert!(matches!(
        events.first(),
        Some(CodexStreamEvent::OutputItemDone { id: Some(id), .. }) if id == "fc_1"
    ));
    assert!(matches!(
        events.get(1),
        Some(CodexStreamEvent::ToolCallRequested {
            id: Some(id),
            call_id: Some(call_id),
            tool_name: Some(tool_name),
            arguments: Some(serde_json::Value::String(arguments)),
        }) if id == "fc_1"
            && call_id == "call_1"
            && tool_name == "read"
            && arguments == "{\"path\":\"README.md\"}"
    ));
}

#[test]
fn sse_parser_preserves_malformed_function_call_payload_for_explicit_handling() {
    let payload = concat!(
        "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_2\",\"name\":\"bash\",\"arguments\":42}}\n\n"
    );

    let events = SseStreamParser::parse_frames(payload);
    assert_eq!(events.len(), 2);

    assert!(matches!(
        events.get(1),
        Some(CodexStreamEvent::ToolCallRequested {
            id: Some(id),
            call_id: None,
            tool_name: Some(tool_name),
            arguments: Some(serde_json::Value::Number(number)),
        }) if id == "fc_2" && tool_name == "bash" && number.as_i64() == Some(42)
    ));
}

#[test]
fn sse_parser_keeps_unknown_typed_events_and_ignores_malformed() {
    let payload = concat!(
        "data: {\"type\":\"unknown.event\",\"foo\":\"bar\"}\n\n",
        "data: {broken-json\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"x\"}\n\n"
    );

    let events = SseStreamParser::parse_frames(payload);
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[0],
        CodexStreamEvent::Unknown { ref event_type, .. } if event_type == "unknown.event"
    ));
    assert!(matches!(
        events[1],
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
fn sse_parser_preserves_split_multibyte_utf8_across_chunks() {
    let mut parser = SseStreamParser::default();
    let template = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"🙂\"}\n\n";
    let (prefix, suffix) = template
        .split_once("🙂")
        .expect("template includes multibyte character");

    let mut first_chunk = prefix.as_bytes().to_vec();
    first_chunk.extend_from_slice(&[0xF0, 0x9F]);
    let mut second_chunk = vec![0x99, 0x82];
    second_chunk.extend_from_slice(suffix.as_bytes());

    assert!(parser.feed(&first_chunk).is_empty());
    let events = parser.feed(&second_chunk);
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events.first(),
        Some(CodexStreamEvent::OutputTextDelta { delta }) if delta == "🙂"
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

#[test]
fn sse_parser_keeps_unknown_completion_status_as_none() {
    let payload =
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"mystery\"}}\n\n";
    let events = SseStreamParser::parse_frames(payload);
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events.first(),
        Some(CodexStreamEvent::ResponseCompleted { status: None })
    ));
}

#[test]
fn sse_parser_error_without_code_or_message_keeps_serialized_event_fallback() {
    let events = SseStreamParser::parse_frames("data: {\"type\":\"error\"}\n\n");
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events.first(),
        Some(CodexStreamEvent::Error { code: None, message: Some(message) })
            if message == "{\"type\":\"error\"}"
    ));
}

#[test]
fn sse_parser_error_with_empty_code_and_message_uses_serialized_fallback() {
    let events = SseStreamParser::parse_frames(
        "data: {\"type\":\"error\",\"code\":\"\",\"message\":\"\"}\n\n",
    );
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events.first(),
        Some(CodexStreamEvent::Error { code: None, message: Some(message) })
            if message.contains("\"type\":\"error\"")
    ));
}

#[test]
fn sse_parser_response_failed_with_empty_message_maps_to_none() {
    let events = SseStreamParser::parse_frames(
        "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"\"}}}\n\n",
    );
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events.first(),
        Some(CodexStreamEvent::ResponseFailed { message: None })
    ));
}
