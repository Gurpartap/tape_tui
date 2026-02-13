use std::fmt;

use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::Error as JsonError;

#[derive(Debug)]
pub enum CodexApiError {
    MissingAccessToken,
    MissingAccountId,
    InvalidBaseUrl(String),
    UrlNormalization(String),
    Request(reqwest::Error),
    Status(StatusCode, String),
    SseChunk(String),
    MalformedSse(String),
    Serde(JsonError),
    UsageLimit {
        message: String,
    },
    RetryExhausted {
        status: Option<StatusCode>,
        last_error: Option<String>,
    },
    StreamFailed {
        code: Option<String>,
        message: String,
    },
    Cancelled,
    JoinError(String),
    Unknown(String),
}

#[derive(Debug, Deserialize)]
pub(crate) struct ErrorPayload {
    #[serde(rename = "error")]
    pub value: Option<ErrorPayloadFields>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ErrorPayloadFields {
    pub message: Option<String>,
    pub code: Option<String>,
    #[serde(rename = "type")]
    pub type_: Option<String>,
    pub plan_type: Option<String>,
    pub resets_at: Option<u64>,
}

impl ErrorPayloadFields {
    pub fn usage_limit_message(&self, status: StatusCode) -> Option<String> {
        let code = self
            .code
            .as_deref()
            .and_then(non_empty_string)
            .or_else(|| self.type_.as_deref().and_then(non_empty_string))
            .unwrap_or("");
        if !matches_usage_limit(code, status) {
            return None;
        }

        let plan = self
            .plan_type
            .as_deref()
            .and_then(non_empty_string)
            .map(|value| format!(" ({} plan)", value.to_ascii_lowercase()))
            .unwrap_or_default();
        let mins = self
            .resets_at
            .filter(|value| *value > 0)
            .and_then(|reset_sec| (reset_sec as i64).checked_mul(1000))
            .and_then(|reset_millis| (reset_millis).checked_sub(current_epoch_ms()))
            .map(|delta| (delta.max(0) as f64 / 60_000f64).round() as i64);
        let retry_hint = mins
            .map(|value| format!(" Try again in ~{value} min."))
            .unwrap_or_default();

        Some(
            format!("You have hit your ChatGPT usage limit{plan}.{retry_hint}")
                .trim()
                .to_string(),
        )
    }

    pub fn message_or_fallback(&self) -> Option<String> {
        let explicit = self
            .message
            .as_deref()
            .and_then(non_empty_string)?;
        Some(explicit.to_owned())
    }
}

impl fmt::Display for ErrorPayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(value) = &self.value {
            let message = value.message.as_deref().unwrap_or("unknown error");
            write!(f, "{message}")
        } else {
            write!(f, "unknown error")
        }
    }
}

impl fmt::Display for CodexApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingAccessToken => write!(f, "access token is required"),
            Self::MissingAccountId => write!(f, "account id is required"),
            Self::InvalidBaseUrl(value) => write!(f, "invalid base URL: {value}"),
            Self::UrlNormalization(message) => write!(f, "URL normalization failed: {message}"),
            Self::Request(error) => write!(f, "request error: {error}"),
            Self::Status(status, message) => write!(f, "HTTP {status} {message}"),
            Self::SseChunk(message) => write!(f, "SSE chunk parse failure: {message}"),
            Self::MalformedSse(message) => write!(f, "malformed SSE event: {message}"),
            Self::Serde(error) => write!(f, "serialization error: {error}"),
            Self::UsageLimit { message } => write!(f, "{message}"),
            Self::RetryExhausted { status, last_error } => {
                let status = status
                    .map(|status| status.as_u16().to_string())
                    .unwrap_or_else(|| "n/a".to_owned());
                write!(f, "retry exhausted after max attempts (status: {status}, last_error: {last_error:?})")
            }
            Self::StreamFailed { code, message } => match code {
                Some(code) if !code.trim().is_empty() => {
                    write!(f, "stream failed ({code}): {message}")
                }
                _ => write!(f, "stream failed: {message}"),
            },
            Self::Cancelled => write!(f, "request was cancelled"),
            Self::JoinError(message) => write!(f, "stream join failure: {message}"),
            Self::Unknown(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for CodexApiError {}

impl From<reqwest::Error> for CodexApiError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<JsonError> for CodexApiError {
    fn from(error: JsonError) -> Self {
        Self::Serde(error)
    }
}

pub fn parse_error_message(status: StatusCode, body: &str) -> String {
    let parsed = match serde_json::from_str::<ErrorPayload>(body) {
        Ok(payload) => payload,
        Err(_) => {
            return if body.is_empty() {
                status
                    .canonical_reason()
                    .unwrap_or("request failed")
                    .to_string()
            } else {
                body.to_string()
            };
        }
    };

    if let Some(error) = parsed.value {
        if let Some(message) = error.usage_limit_message(status) {
            return message;
        }
        if let Some(message) = error.message_or_fallback() {
            return message;
        }
    }

    if body.is_empty() {
        status
            .canonical_reason()
            .unwrap_or("request failed")
            .to_string()
    } else {
        body.to_string()
    }
}

fn matches_usage_limit(code: &str, status: StatusCode) -> bool {
    matches!(status, StatusCode::TOO_MANY_REQUESTS)
        || code.eq_ignore_ascii_case("usage_limit_reached")
        || code.eq_ignore_ascii_case("usage_not_included")
        || code.eq_ignore_ascii_case("rate_limit_exceeded")
}

fn current_epoch_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(now.as_millis()).unwrap_or(i64::MAX)
}

fn non_empty_string(value: &str) -> Option<&str> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}
