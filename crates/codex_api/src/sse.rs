use serde_json::Value;

use crate::events::{CodexResponseStatus, CodexStreamEvent};

/// Incremental parser for SSE text streams.
#[derive(Debug, Default)]
pub struct SseStreamParser {
    buffer: Vec<u8>,
}

impl SseStreamParser {
    /// Feed arbitrary bytes into the parser and drain complete events.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<CodexStreamEvent> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();

        while let Some((split, separator_len)) = find_frame_separator(&self.buffer) {
            let frame = self.buffer[..split].to_vec();
            self.buffer.drain(0..split + separator_len);

            if let Some(payload) = extract_data_payload(&frame) {
                if payload == "[DONE]" || payload.is_empty() {
                    continue;
                }

                if let Ok(value) = serde_json::from_str::<Value>(&payload) {
                    events.extend(map_event(value));
                }
            }
        }

        events
    }

    /// Parse a complete SSE payload string in one shot.
    pub fn parse_frames(input: &str) -> Vec<CodexStreamEvent> {
        let mut parser = Self::default();
        parser.feed(input.as_bytes())
    }

    pub fn is_empty_buffer(&self) -> bool {
        self.buffer.iter().all(|byte| byte.is_ascii_whitespace())
    }
}

fn find_frame_separator(buffer: &[u8]) -> Option<(usize, usize)> {
    for index in 0..buffer.len() {
        if index + 1 < buffer.len() && &buffer[index..index + 2] == b"\n\n" {
            return Some((index, 2));
        }
        if index + 3 < buffer.len() && &buffer[index..index + 4] == b"\r\n\r\n" {
            return Some((index, 4));
        }
    }
    None
}

fn extract_data_payload(frame: &[u8]) -> Option<String> {
    let frame = std::str::from_utf8(frame).ok()?;
    let data_lines: Vec<&str> = frame
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .collect();

    if data_lines.is_empty() {
        None
    } else {
        Some(data_lines.join("\n"))
    }
}

fn map_event(value: Value) -> Vec<CodexStreamEvent> {
    let Some(event_type) = value
        .get("type")
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned)
    else {
        return Vec::new();
    };

    match event_type.as_str() {
        "response.output_text.delta" => {
            let delta = value
                .get("delta")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            vec![CodexStreamEvent::OutputTextDelta {
                delta: delta.to_owned(),
            }]
        }
        "response.reasoning_summary_text.delta" => {
            let delta = value
                .get("delta")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            vec![CodexStreamEvent::ReasoningSummaryTextDelta {
                delta: delta.to_owned(),
            }]
        }
        "response.output_item.done" => {
            let id = value
                .get("item")
                .and_then(|item| item.get("id"))
                .and_then(|value| value.as_str())
                .map(ToString::to_string);
            let status = value
                .get("item")
                .and_then(|item| item.get("status"))
                .and_then(|value| value.as_str())
                .and_then(CodexResponseStatus::parse);

            let mut events = vec![CodexStreamEvent::OutputItemDone {
                id: id.clone(),
                status,
            }];

            if value
                .get("item")
                .and_then(|item| item.get("type"))
                .and_then(|value| value.as_str())
                == Some("function_call")
            {
                let call_id = value
                    .get("item")
                    .and_then(|item| item.get("call_id"))
                    .and_then(|value| value.as_str())
                    .map(ToString::to_string);
                let tool_name = value
                    .get("item")
                    .and_then(|item| item.get("name"))
                    .and_then(|value| value.as_str())
                    .map(ToString::to_string);
                let arguments = value
                    .get("item")
                    .and_then(|item| item.get("arguments"))
                    .cloned();

                events.push(CodexStreamEvent::ToolCallRequested {
                    id,
                    call_id,
                    tool_name,
                    arguments,
                });
            }

            events
        }
        "response.completed" | "response.done" => {
            let status = value
                .get("response")
                .and_then(|response| response.get("status"))
                .and_then(|status| status.as_str())
                .and_then(CodexResponseStatus::parse);

            // Keep alias handling explicit so callers receive normalized completion.
            vec![CodexStreamEvent::ResponseCompleted { status }]
        }
        "response.failed" => {
            let message = value
                .get("response")
                .and_then(|response| response.get("error"))
                .and_then(|error| error.get("message"))
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
                .map(ToString::to_string);
            vec![CodexStreamEvent::ResponseFailed { message }]
        }
        "error" => {
            let code = value
                .get("code")
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
                .map(ToString::to_string);
            let message = value
                .get("message")
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .or_else(|| {
                    if code.is_none() {
                        serde_json::to_string(&value).ok()
                    } else {
                        None
                    }
                });
            vec![CodexStreamEvent::Error { code, message }]
        }
        _ => vec![CodexStreamEvent::Unknown {
            event_type,
            payload: value,
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::SseStreamParser;
    use crate::events::{CodexResponseStatus, CodexStreamEvent};

    #[test]
    fn parse_sse_frames_incrementally() {
        let mut parser = SseStreamParser::default();
        let mut events = Vec::new();

        events.extend(
            parser.feed(b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello\"}\n\n"),
        );
        assert_eq!(events.len(), 1);

        events.extend(parser.feed(b"data: [DONE]\n\n"));
        assert_eq!(events.len(), 1);
        assert!(parser.is_empty_buffer());
    }

    #[test]
    fn parse_function_call_output_item_emits_ordered_tool_call_events() {
        let payload = concat!(
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"status\":\"in_progress\",\"call_id\":\"call_1\",\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"}}\n\n"
        );

        let events = SseStreamParser::parse_frames(payload);
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events.first(),
            Some(CodexStreamEvent::OutputItemDone {
                id: Some(id),
                status: Some(CodexResponseStatus::InProgress),
            }) if id == "fc_1"
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
    fn parse_function_call_output_item_preserves_non_object_arguments() {
        let payload = concat!(
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_bad\",\"call_id\":\"call_bad\",\"name\":\"bash\",\"arguments\":17}}\n\n"
        );

        let events = SseStreamParser::parse_frames(payload);
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events.get(1),
            Some(CodexStreamEvent::ToolCallRequested {
                id: Some(id),
                call_id: Some(call_id),
                tool_name: Some(tool_name),
                arguments: Some(serde_json::Value::Number(number)),
            }) if id == "fc_bad"
                && call_id == "call_bad"
                && tool_name == "bash"
                && number.as_i64() == Some(17)
        ));
    }
}
