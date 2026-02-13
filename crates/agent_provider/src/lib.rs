//! Minimal provider-agnostic contract for executing a single model run.
//!
//! This crate intentionally defines only shared run lifecycle and host-mediated
//! tool-calling contract types. It excludes provider transport details,
//! protocol payloads, and multi-run orchestration concerns.

use std::fmt;
use std::sync::{atomic::AtomicBool, Arc};

use serde_json::Value;

/// Identifier for one provider run.
pub type RunId = u64;

/// Shared cancellation flag for a run.
pub type CancelSignal = Arc<AtomicBool>;

/// Error returned while constructing/configuring a provider before any run starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderInitError {
    message: String,
}

impl ProviderInitError {
    /// Creates a new provider initialization error.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the underlying error message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ProviderInitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ProviderInitError {}

impl From<String> for ProviderInitError {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

impl From<&str> for ProviderInitError {
    fn from(message: &str) -> Self {
        Self::new(message)
    }
}

/// Provider-neutral model-facing message history item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunMessage {
    UserText {
        text: String,
    },
    AssistantText {
        text: String,
    },
    ToolCall {
        call_id: String,
        tool_name: String,
        arguments: Value,
    },
    ToolResult {
        call_id: String,
        tool_name: String,
        content: Value,
        is_error: bool,
    },
}

/// Input required to start a provider run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRequest {
    pub run_id: RunId,
    pub messages: Vec<RunMessage>,
    pub instructions: String,
}

/// Generic host-mediated tool definition exposed by a provider.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
}

/// Provider request envelope for one host tool call.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallRequest {
    pub call_id: String,
    pub tool_name: String,
    pub arguments: Value,
}

/// Host tool call result returned back to providers.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    pub call_id: String,
    pub tool_name: String,
    pub is_error: bool,
    pub content: Value,
}

impl ToolResult {
    /// Constructs a successful tool result.
    #[must_use]
    pub fn success(
        call_id: impl Into<String>,
        tool_name: impl Into<String>,
        content: impl Into<Value>,
    ) -> Self {
        Self {
            call_id: call_id.into(),
            tool_name: tool_name.into(),
            is_error: false,
            content: content.into(),
        }
    }

    /// Constructs a tool error result.
    #[must_use]
    pub fn error(
        call_id: impl Into<String>,
        tool_name: impl Into<String>,
        content: impl Into<Value>,
    ) -> Self {
        Self {
            call_id: call_id.into(),
            tool_name: tool_name.into(),
            is_error: true,
            content: content.into(),
        }
    }
}

/// Provider-emitted lifecycle event for a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEvent {
    Started { run_id: RunId },
    Chunk { run_id: RunId, text: String },
    Finished { run_id: RunId },
    Failed { run_id: RunId, error: String },
    Cancelled { run_id: RunId },
}

impl RunEvent {
    /// Returns the run identifier associated with this event.
    #[must_use]
    pub fn run_id(&self) -> RunId {
        match self {
            Self::Started { run_id }
            | Self::Chunk { run_id, .. }
            | Self::Finished { run_id }
            | Self::Failed { run_id, .. }
            | Self::Cancelled { run_id } => *run_id,
        }
    }

    /// Returns true when this event terminates the run lifecycle.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Finished { .. } | Self::Failed { .. } | Self::Cancelled { .. }
        )
    }
}

/// Immutable metadata describing a run provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderProfile {
    pub provider_id: String,
    pub model_id: String,
    pub thinking_level: Option<String>,
}

/// Provider interface for executing one run request.
pub trait RunProvider: Send + Sync + 'static {
    /// Returns provider/model identity metadata.
    fn profile(&self) -> ProviderProfile;

    /// Returns provider-specific host-mediated tool definitions available during runs.
    fn tool_definitions(&self) -> Vec<ToolDefinition> {
        Vec::new()
    }

    /// Cycles to the next model selection for future runs.
    ///
    /// Providers may return an error when model cycling is unsupported.
    fn cycle_model(&self) -> Result<ProviderProfile, String> {
        Err("Model cycling is not supported by this provider".to_string())
    }

    /// Cycles to the next thinking-level selection for future runs.
    ///
    /// Providers may return an error when thinking-level cycling is unsupported.
    fn cycle_thinking_level(&self) -> Result<ProviderProfile, String> {
        Err("Thinking-level cycling is not supported by this provider".to_string())
    }

    /// Executes a run request and emits lifecycle events in provider order.
    ///
    /// Providers can synchronously request host tool execution through `execute_tool`.
    /// The callback is deterministic and serial from the caller perspective.
    fn run(
        &self,
        req: RunRequest,
        cancel: CancelSignal,
        execute_tool: &mut dyn FnMut(ToolCallRequest) -> ToolResult,
        emit: &mut dyn FnMut(RunEvent),
    ) -> Result<(), String>;
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        CancelSignal, ProviderInitError, ProviderProfile, RunEvent, RunMessage, RunProvider,
        RunRequest, ToolCallRequest, ToolDefinition, ToolResult,
    };

    struct MinimalProvider;

    impl RunProvider for MinimalProvider {
        fn profile(&self) -> ProviderProfile {
            ProviderProfile {
                provider_id: "minimal".to_string(),
                model_id: "minimal-model".to_string(),
                thinking_level: None,
            }
        }

        fn run(
            &self,
            req: RunRequest,
            _cancel: CancelSignal,
            _execute_tool: &mut dyn FnMut(ToolCallRequest) -> ToolResult,
            emit: &mut dyn FnMut(RunEvent),
        ) -> Result<(), String> {
            emit(RunEvent::Started { run_id: req.run_id });
            emit(RunEvent::Finished { run_id: req.run_id });
            Ok(())
        }
    }

    #[test]
    fn run_event_run_id_returns_event_run_id() {
        let run_id = 42;
        let events = [
            RunEvent::Started { run_id },
            RunEvent::Chunk {
                run_id,
                text: "partial".to_string(),
            },
            RunEvent::Finished { run_id },
            RunEvent::Failed {
                run_id,
                error: "failure".to_string(),
            },
            RunEvent::Cancelled { run_id },
        ];

        for event in events {
            assert_eq!(event.run_id(), run_id);
        }
    }

    #[test]
    fn run_event_terminal_detection_matches_lifecycle() {
        assert!(!RunEvent::Started { run_id: 1 }.is_terminal());
        assert!(!RunEvent::Chunk {
            run_id: 1,
            text: "hello".to_string(),
        }
        .is_terminal());
        assert!(RunEvent::Finished { run_id: 1 }.is_terminal());
        assert!(RunEvent::Failed {
            run_id: 1,
            error: "boom".to_string(),
        }
        .is_terminal());
        assert!(RunEvent::Cancelled { run_id: 1 }.is_terminal());
    }

    #[test]
    fn provider_init_error_preserves_message() {
        let error = ProviderInitError::new("missing token");
        assert_eq!(error.message(), "missing token");
        assert_eq!(error.to_string(), "missing token");
    }

    #[test]
    fn run_request_carries_message_history_and_instructions() {
        let request = RunRequest {
            run_id: 7,
            messages: vec![RunMessage::UserText {
                text: "implement tests".to_string(),
            }],
            instructions: "system instructions".to_string(),
        };

        assert_eq!(request.run_id, 7);
        assert_eq!(
            request.messages,
            vec![RunMessage::UserText {
                text: "implement tests".to_string(),
            }]
        );
        assert_eq!(request.instructions, "system instructions");
    }

    #[test]
    fn default_tool_definitions_are_empty() {
        let provider = MinimalProvider;
        assert!(provider.tool_definitions().is_empty());
    }

    #[test]
    fn tool_result_constructors_set_error_flag_and_content() {
        let success = ToolResult::success("call-1", "bash", json!({"output": "ok"}));
        assert_eq!(
            success,
            ToolResult {
                call_id: "call-1".to_string(),
                tool_name: "bash".to_string(),
                is_error: false,
                content: json!({"output": "ok"}),
            }
        );

        let error = ToolResult::error("call-2", "read", "missing file");
        assert_eq!(
            error,
            ToolResult {
                call_id: "call-2".to_string(),
                tool_name: "read".to_string(),
                is_error: true,
                content: json!("missing file"),
            }
        );
    }

    #[test]
    fn tool_definition_and_call_request_are_provider_neutral_json_envelopes() {
        let definition = ToolDefinition {
            name: "read".to_string(),
            description: Some("Reads UTF-8 text from a path".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }),
        };

        let call = ToolCallRequest {
            call_id: "call-42".to_string(),
            tool_name: definition.name.clone(),
            arguments: json!({ "path": "README.md" }),
        };

        assert_eq!(definition.name, "read");
        assert_eq!(call.call_id, "call-42");
        assert_eq!(call.arguments["path"], "README.md");
    }

    #[test]
    fn default_model_cycle_hook_reports_unsupported() {
        let provider = MinimalProvider;
        let error = provider
            .cycle_model()
            .expect_err("minimal provider should not support model cycling");

        assert_eq!(error, "Model cycling is not supported by this provider");
    }

    #[test]
    fn default_thinking_cycle_hook_reports_unsupported() {
        let provider = MinimalProvider;
        let error = provider
            .cycle_thinking_level()
            .expect_err("minimal provider should not support thinking-level cycling");

        assert_eq!(
            error,
            "Thinking-level cycling is not supported by this provider"
        );
    }
}
