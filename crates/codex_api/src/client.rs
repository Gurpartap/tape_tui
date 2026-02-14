use std::future::Future;
use std::sync::{atomic::AtomicBool, atomic::Ordering, Arc};
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, Response, StatusCode};

use crate::config::CodexApiConfig;
use crate::error::{parse_error_message, CodexApiError};
use crate::events::{CodexResponseStatus, CodexStreamEvent};
use crate::headers::build_headers;
use crate::payload::CodexRequest;
use crate::retry::is_retryable_http_error;
use crate::retry::{retry_delay_ms, MAX_RETRIES};
use crate::sse::SseStreamParser;
use crate::url::normalize_codex_url;

/// Optional cancellation signal shared across request and stream loops.
pub type CancellationSignal = Arc<AtomicBool>;

const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Debug)]
pub struct CodexApiClient {
    http: Client,
    config: CodexApiConfig,
}

#[derive(Debug, Clone)]
pub struct StreamResult {
    pub events: Vec<CodexStreamEvent>,
    pub terminal: Option<CodexResponseStatus>,
}

impl CodexApiClient {
    pub fn new(config: CodexApiConfig) -> Result<Self, CodexApiError> {
        let mut builder = Client::builder();
        if let Some(timeout) = config.timeout {
            builder = builder.timeout(timeout);
        }
        let http = builder.build().map_err(CodexApiError::from)?;
        Ok(Self { http, config })
    }

    pub fn config(&self) -> &CodexApiConfig {
        &self.config
    }

    pub fn normalized_endpoint(&self) -> String {
        normalize_codex_url(&self.config.base_url)
    }

    pub fn build_headers(&self, user_agent: Option<&str>) -> Result<HeaderMap, CodexApiError> {
        let headers = build_headers(&self.config, user_agent)?;
        let mut out = HeaderMap::new();
        for (key, value) in headers {
            out.insert(
                HeaderName::from_bytes(key.as_bytes()).map_err(|_| {
                    CodexApiError::InvalidBaseUrl(format!("invalid header key: {key}"))
                })?,
                HeaderValue::from_str(&value).map_err(|_| {
                    CodexApiError::InvalidBaseUrl(format!("invalid header value for {key}"))
                })?,
            );
        }
        Ok(out)
    }

    pub fn build_request(
        &self,
        request: &CodexRequest,
    ) -> Result<reqwest::RequestBuilder, CodexApiError> {
        validate_request_payload_shape(request)?;

        let headers = self.build_headers(self.config.user_agent.as_deref())?;
        let payload = self.request_with_transport_defaults(request);
        Ok(self
            .http
            .post(self.normalized_endpoint())
            .headers(headers)
            .json(&payload))
    }

    fn request_with_transport_defaults(&self, request: &CodexRequest) -> CodexRequest {
        let mut payload = request.clone();
        payload.store = false;
        payload.stream = true;
        if payload.text.verbosity.trim().is_empty() {
            payload.text.verbosity = "medium".to_owned();
        }
        payload.include = vec!["reasoning.encrypted_content".to_owned()];
        payload.tool_choice = Some("auto".to_owned());
        payload.parallel_tool_calls = true;
        if payload.prompt_cache_key.is_none() {
            if let Some(session_id) = self
                .config
                .session_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                payload.prompt_cache_key = Some(session_id.to_string());
            }
        }
        if let Some(reasoning) = payload.reasoning.as_mut() {
            if let Some(effort) = reasoning.effort.clone() {
                reasoning.effort = Some(clamp_reasoning_effort(&payload.model, &effort));
                if reasoning.summary.is_none() {
                    reasoning.summary = Some("auto".to_owned());
                }
            }
        }
        payload
    }

    pub async fn send_with_retry(
        &self,
        request: &CodexRequest,
        cancellation: Option<&CancellationSignal>,
    ) -> Result<Response, CodexApiError> {
        let mut last_status: Option<StatusCode> = None;
        let mut last_error = None;

        for attempt in 0..=MAX_RETRIES {
            if is_cancelled(cancellation) {
                return Err(CodexApiError::Cancelled);
            }

            let response = self.build_request(request)?.send();
            let response = await_or_cancel(response, cancellation)
                .await?
                .map_err(CodexApiError::from);

            match response {
                Ok(response) => {
                    if response.status().is_success() {
                        return Ok(response);
                    }

                    last_status = Some(response.status());
                    let status = response.status();
                    let body = await_or_cancel(response.text(), cancellation)
                        .await?
                        .unwrap_or_else(|_| {
                            status
                                .canonical_reason()
                                .unwrap_or("request failed")
                                .to_string()
                        });
                    let message = parse_error_message(status, &body);
                    last_error = Some(message.clone());
                    let should_retry_status = is_retryable_http_error(status.as_u16(), &body);
                    let should_retry_message = !has_usage_limit_message(&message);

                    if attempt < MAX_RETRIES && (should_retry_status || should_retry_message) {
                        await_or_cancel(tokio::time::sleep(retry_delay_ms(attempt)), cancellation)
                            .await?;
                        continue;
                    }

                    return Err(CodexApiError::Status(status, message));
                }
                Err(error) => {
                    let message = error.to_string();
                    last_error = Some(message.clone());
                    if attempt < MAX_RETRIES && !has_usage_limit_message(&message) {
                        await_or_cancel(tokio::time::sleep(retry_delay_ms(attempt)), cancellation)
                            .await?;
                        continue;
                    }
                    return Err(CodexApiError::RetryExhausted {
                        status: last_status,
                        last_error,
                    });
                }
            }
        }

        Err(CodexApiError::RetryExhausted {
            status: last_status,
            last_error,
        })
    }

    pub async fn stream_with_handler<F>(
        &self,
        request: &CodexRequest,
        cancellation: Option<&CancellationSignal>,
        mut on_event: F,
    ) -> Result<Option<CodexResponseStatus>, CodexApiError>
    where
        F: FnMut(CodexStreamEvent),
    {
        let response = self.send_with_retry(request, cancellation).await?;
        let mut bytes = response.bytes_stream();
        let mut parser = SseStreamParser::default();
        let mut terminal = None;

        loop {
            let Some(chunk) = await_or_cancel(bytes.next(), cancellation).await? else {
                break;
            };
            if is_cancelled(cancellation) {
                return Err(CodexApiError::Cancelled);
            }
            let chunk = chunk.map_err(CodexApiError::from)?;
            for event in parser.feed(&chunk) {
                process_stream_event(event, &mut terminal, &mut on_event)?;
            }
        }

        if is_cancelled(cancellation) {
            return Err(CodexApiError::Cancelled);
        }

        Ok(terminal.flatten())
    }

    pub async fn stream(
        &self,
        request: &CodexRequest,
        cancellation: Option<&CancellationSignal>,
    ) -> Result<StreamResult, CodexApiError> {
        let mut events = Vec::new();
        let terminal = self
            .stream_with_handler(request, cancellation, |event| {
                events.push(event);
            })
            .await?;

        Ok(StreamResult { events, terminal })
    }
}

fn validate_request_payload_shape(request: &CodexRequest) -> Result<(), CodexApiError> {
    if request.input.is_array() {
        return Ok(());
    }

    Err(CodexApiError::InvalidRequestPayload(format!(
        "'input' must be a JSON array/list, got {}",
        value_type_name(&request.input)
    )))
}

fn value_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

fn process_stream_event<F>(
    event: CodexStreamEvent,
    terminal: &mut Option<Option<CodexResponseStatus>>,
    on_event: &mut F,
) -> Result<(), CodexApiError>
where
    F: FnMut(CodexStreamEvent),
{
    if let Some(error) = stream_failure_from_event(&event) {
        return Err(error);
    }

    if let Some(next_terminal) = terminal_status_from_event(&event) {
        *terminal = Some(next_terminal);
    }

    on_event(event);
    Ok(())
}

#[cfg(test)]
fn terminal_status(events: &[CodexStreamEvent]) -> Option<CodexResponseStatus> {
    let mut terminal = None;
    for event in events {
        if let Some(next_terminal) = terminal_status_from_event(event) {
            terminal = Some(next_terminal);
        }
    }

    terminal.flatten()
}

fn terminal_status_from_event(event: &CodexStreamEvent) -> Option<Option<CodexResponseStatus>> {
    match event {
        CodexStreamEvent::ResponseCompleted { status } => Some(*status),
        CodexStreamEvent::ResponseFailed { .. } => Some(Some(CodexResponseStatus::Failed)),
        CodexStreamEvent::Error { .. } => Some(Some(CodexResponseStatus::Failed)),
        _ => None,
    }
}

fn stream_failure_from_event(event: &CodexStreamEvent) -> Option<CodexApiError> {
    match event {
        CodexStreamEvent::ResponseFailed { message } => Some(CodexApiError::StreamFailed {
            code: None,
            message: message
                .clone()
                .unwrap_or_else(|| "Codex response failed".to_owned()),
        }),
        CodexStreamEvent::Error { code, message } => Some(CodexApiError::StreamFailed {
            code: code.clone(),
            message: format!(
                "Codex error: {}",
                message
                    .clone()
                    .or_else(|| code.clone())
                    .unwrap_or_else(|| r#"{"type":"error"}"#.to_owned())
            ),
        }),
        _ => None,
    }
}

fn is_cancelled(cancel: Option<&CancellationSignal>) -> bool {
    cancel.is_some_and(|token| token.load(Ordering::Acquire))
}

fn has_usage_limit_message(message: &str) -> bool {
    message.contains("usage limit")
}

fn clamp_reasoning_effort(model_id: &str, effort: &str) -> String {
    let id = model_id.rsplit('/').next().unwrap_or(model_id);
    if (id.starts_with("gpt-5.2") || id.starts_with("gpt-5.3")) && effort == "minimal" {
        return "low".to_owned();
    }
    if id == "gpt-5.1" && effort == "xhigh" {
        return "high".to_owned();
    }
    if id == "gpt-5.1-codex-mini" {
        return if matches!(effort, "high" | "xhigh") {
            "high".to_owned()
        } else {
            "medium".to_owned()
        };
    }
    effort.to_owned()
}

async fn await_or_cancel<F>(
    future: F,
    cancellation: Option<&CancellationSignal>,
) -> Result<F::Output, CodexApiError>
where
    F: Future,
{
    if cancellation.is_none() {
        return Ok(future.await);
    }

    let mut future = Box::pin(future);

    loop {
        if is_cancelled(cancellation) {
            return Err(CodexApiError::Cancelled);
        }

        if let Ok(output) = tokio::time::timeout(CANCEL_POLL_INTERVAL, &mut future).await {
            if is_cancelled(cancellation) {
                return Err(CodexApiError::Cancelled);
            }
            return Ok(output);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{process_stream_event, terminal_status};
    use crate::events::{CodexResponseStatus, CodexStreamEvent};
    use crate::sse::SseStreamParser;

    #[test]
    fn terminal_status_reports_completed_status() {
        let events = vec![
            CodexStreamEvent::OutputTextDelta {
                delta: "hello".to_owned(),
            },
            CodexStreamEvent::ResponseCompleted {
                status: Some(CodexResponseStatus::Completed),
            },
        ];

        assert_eq!(
            terminal_status(&events),
            Some(CodexResponseStatus::Completed)
        );
    }

    #[test]
    fn terminal_status_reports_failed_status() {
        let events = vec![CodexStreamEvent::Error {
            code: Some("x".to_owned()),
            message: Some("bad".to_owned()),
        }];

        assert_eq!(terminal_status(&events), Some(CodexResponseStatus::Failed));
    }

    #[test]
    fn terminal_status_respects_non_completed_terminal_values() {
        let queued = vec![CodexStreamEvent::ResponseCompleted {
            status: Some(CodexResponseStatus::Queued),
        }];
        let in_progress = vec![CodexStreamEvent::ResponseCompleted {
            status: Some(CodexResponseStatus::InProgress),
        }];
        let incomplete = vec![CodexStreamEvent::ResponseCompleted {
            status: Some(CodexResponseStatus::Incomplete),
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
    fn terminal_status_is_none_when_missing_terminal_event() {
        let events = vec![CodexStreamEvent::OutputTextDelta {
            delta: "hello".to_owned(),
        }];

        assert_eq!(terminal_status(&events), None);
    }

    #[test]
    fn terminal_status_treats_unknown_completed_status_as_none() {
        let events = vec![CodexStreamEvent::ResponseCompleted { status: None }];
        assert_eq!(terminal_status(&events), None);
    }

    #[test]
    fn process_stream_event_emits_output_deltas_in_parser_order() {
        let frames = concat!(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"A\"}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"B\"}\n\n",
        );
        let mut parser = SseStreamParser::default();
        let parsed = parser.feed(frames.as_bytes());

        let mut terminal = None;
        let mut observed = Vec::new();
        for event in parsed {
            process_stream_event(event, &mut terminal, &mut |event| observed.push(event))
                .expect("output deltas should process successfully");
        }

        assert!(terminal.is_none());
        assert_eq!(
            observed,
            vec![
                CodexStreamEvent::OutputTextDelta {
                    delta: "A".to_string(),
                },
                CodexStreamEvent::OutputTextDelta {
                    delta: "B".to_string(),
                },
            ]
        );
    }

    #[test]
    fn process_stream_event_terminal_tracking_matches_terminal_status_scan() {
        let events = vec![
            CodexStreamEvent::OutputTextDelta {
                delta: "hello".to_owned(),
            },
            CodexStreamEvent::ResponseCompleted {
                status: Some(CodexResponseStatus::Completed),
            },
            CodexStreamEvent::ResponseCompleted { status: None },
        ];

        let mut terminal = None;
        let mut observed = Vec::new();
        for event in events.iter().cloned() {
            process_stream_event(event, &mut terminal, &mut |event| observed.push(event))
                .expect("events should process successfully");
        }

        assert_eq!(terminal.flatten(), terminal_status(&events));
        assert_eq!(observed, events);
    }
}
