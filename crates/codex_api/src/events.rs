use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Canonical terminal state mapped from Codex responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexResponseStatus {
    Completed,
    Incomplete,
    Failed,
    Cancelled,
    Queued,
    InProgress,
}

impl CodexResponseStatus {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "completed" => Self::Completed,
            "incomplete" => Self::Incomplete,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "queued" => Self::Queued,
            "in_progress" => Self::InProgress,
            _ => return None,
        })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Incomplete => "incomplete",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Queued => "queued",
            Self::InProgress => "in_progress",
        }
    }
}

/// Stream event emitted by the parser after normalization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CodexStreamEvent {
    #[serde(rename = "response.output_text.delta")]
    OutputTextDelta { delta: String },
    #[serde(rename = "response.reasoning_summary_text.delta")]
    ReasoningSummaryTextDelta { delta: String },
    #[serde(rename = "response.output_item.done")]
    OutputItemDone {
        id: Option<String>,
        status: Option<CodexResponseStatus>,
    },
    /// Normalized function-tool call event extracted from output item payloads.
    #[serde(rename = "response.output_item.function_call")]
    ToolCallRequested {
        id: Option<String>,
        call_id: Option<String>,
        tool_name: Option<String>,
        arguments: Option<Value>,
    },
    #[serde(rename = "response.completed")]
    ResponseCompleted {
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<CodexResponseStatus>,
    },
    #[serde(rename = "response.failed")]
    ResponseFailed { message: Option<String> },
    #[serde(rename = "error")]
    Error {
        code: Option<String>,
        message: Option<String>,
    },
    /// Unknown event type retained for parity-safe passthrough behavior.
    #[serde(rename = "unknown")]
    Unknown { event_type: String, payload: Value },
}

#[derive(Debug, Clone, Default)]
pub struct CodexEventAccumulator {
    pub output_text: String,
    pub reasoning_text: String,
    pub completed: bool,
}
