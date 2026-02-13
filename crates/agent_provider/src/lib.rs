//! Minimal provider-agnostic contract for executing a single model run.
//!
//! This crate intentionally defines only shared run lifecycle types.
//! It excludes provider transport details, protocol payloads, tool-calling
//! contracts, and multi-run orchestration concerns.

use std::fmt;
use std::sync::{atomic::AtomicBool, Arc};

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

/// Input required to start a provider run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRequest {
    pub run_id: RunId,
    pub prompt: String,
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
    fn run(
        &self,
        req: RunRequest,
        cancel: CancelSignal,
        emit: &mut dyn FnMut(RunEvent),
    ) -> Result<(), String>;
}

#[cfg(test)]
mod tests {
    use super::{
        CancelSignal, ProviderInitError, ProviderProfile, RunEvent, RunProvider, RunRequest,
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
