use serde_json::Value;

use crate::events::{CodexResponseStatus, CodexStreamEvent};

/// Incremental parser for SSE text streams.
#[derive(Debug, Default)]
pub struct SseStreamParser {
    buffer: String,
}

impl SseStreamParser {
    /// Feed arbitrary bytes into the parser and drain complete events.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<CodexStreamEvent> {
        self.buffer.push_str(&String::from_utf8_lossy(bytes));
        let mut events = Vec::new();

        while let Some(split) = self.buffer.find("\n\n") {
            let frame = self.buffer[..split].to_string();
            self.buffer.drain(0..split + 2);

            if let Some(payload) = extract_data_payload(&frame) {
                if payload == "[DONE]" || payload.is_empty() {
                    continue;
                }

                if let Ok(value) = serde_json::from_str::<Value>(&payload) {
                    if let Some(event) = map_event(value) {
                        events.push(event);
                    }
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
        self.buffer.trim().is_empty()
    }
}

fn extract_data_payload(frame: &str) -> Option<String> {
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

fn map_event(value: Value) -> Option<CodexStreamEvent> {
    let event_type = value.get("type")?.as_str()?;

    match event_type {
        "response.output_text.delta" => {
            let delta = value
                .get("delta")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            Some(CodexStreamEvent::OutputTextDelta {
                delta: delta.to_owned(),
            })
        }
        "response.reasoning_summary_text.delta" => {
            let delta = value
                .get("delta")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            Some(CodexStreamEvent::ReasoningSummaryTextDelta {
                delta: delta.to_owned(),
            })
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
            Some(CodexStreamEvent::OutputItemDone { id, status })
        }
        "response.completed" | "response.done" => {
            let status = value
                .get("response")
                .and_then(|response| response.get("status"))
                .and_then(|status| status.as_str())
                .and_then(CodexResponseStatus::parse)
                .unwrap_or(CodexResponseStatus::Completed);

            // Keep alias handling explicit so callers receive normalized completion.
            Some(CodexStreamEvent::ResponseCompleted { status })
        }
        "response.failed" => {
            let message = value
                .get("response")
                .and_then(|response| response.get("error"))
                .and_then(|error| error.get("message"))
                .and_then(|value| value.as_str())
                .map(ToString::to_string);
            Some(CodexStreamEvent::ResponseFailed { message })
        }
        "error" => {
            let code = value
                .get("code")
                .and_then(|value| value.as_str())
                .map(ToString::to_string);
            let message = value
                .get("message")
                .and_then(|value| value.as_str())
                .map(ToString::to_string);
            Some(CodexStreamEvent::Error { code, message })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::SseStreamParser;

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
}
