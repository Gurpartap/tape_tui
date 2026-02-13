use std::sync::{atomic::AtomicBool, atomic::Ordering, Arc};

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
        let headers = self.build_headers(self.config.user_agent.as_deref())?;
        Ok(self
            .http
            .post(self.normalized_endpoint())
            .headers(headers)
            .json(request))
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

            let response = self
                .build_request(request)?
                .send()
                .await
                .map_err(CodexApiError::from);

            match response {
                Ok(response) => {
                    if response.status().is_success() {
                        return Ok(response);
                    }

                    last_status = Some(response.status());
                    let status = response.status();
                    let body = response
                        .text()
                        .await
                        .unwrap_or_else(|_| String::from("request failed"));
                    let message = parse_error_message(status, &body);
                    let should_retry = is_retryable_http_error(status.as_u16(), &body);

                    if attempt < MAX_RETRIES && should_retry {
                        last_error = Some(message);
                        tokio::time::sleep(retry_delay_ms(attempt)).await;
                        continue;
                    }

                    return Err(CodexApiError::Status(status, message));
                }
                Err(error) => {
                    let message = error.to_string();
                    last_error = Some(message.clone());
                    if attempt < MAX_RETRIES {
                        tokio::time::sleep(retry_delay_ms(attempt)).await;
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

    pub async fn stream(
        &self,
        request: &CodexRequest,
        cancellation: Option<&CancellationSignal>,
    ) -> Result<StreamResult, CodexApiError> {
        let response = self.send_with_retry(request, cancellation).await?;
        let mut bytes = response.bytes_stream();
        let mut parser = SseStreamParser::default();
        let mut events = Vec::new();

        while let Some(chunk) = bytes.next().await {
            if is_cancelled(cancellation) {
                return Err(CodexApiError::Cancelled);
            }
            let chunk = chunk.map_err(CodexApiError::from)?;
            events.extend(parser.feed(&chunk));
        }

        if is_cancelled(cancellation) {
            return Err(CodexApiError::Cancelled);
        }

        let terminal = terminal_status(&events);
        Ok(StreamResult { events, terminal })
    }
}

fn terminal_status(events: &[CodexStreamEvent]) -> Option<CodexResponseStatus> {
    events
        .iter()
        .rev()
        .find_map(|event| match event {
            CodexStreamEvent::ResponseCompleted { status } => Some(*status),
            CodexStreamEvent::ResponseFailed { .. } => Some(CodexResponseStatus::Failed),
            CodexStreamEvent::Error { .. } => Some(CodexResponseStatus::Failed),
            _ => None,
        })
        .or(Some(CodexResponseStatus::Incomplete))
}

fn is_cancelled(cancel: Option<&CancellationSignal>) -> bool {
    cancel.is_some_and(|token| token.load(Ordering::Acquire))
}
