//! Minimal provider-agnostic contract for executing a single model run.
//!
//! This crate intentionally defines only shared run lifecycle types.
//! It excludes provider transport details, protocol payloads, tool-calling
//! contracts, and multi-run orchestration concerns.

use std::sync::{atomic::AtomicBool, Arc};

/// Identifier for one provider run.
pub type RunId = u64;

/// Shared cancellation flag for a run.
pub type CancelSignal = Arc<AtomicBool>;

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
    use super::RunEvent;

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
        assert!(
            !RunEvent::Chunk {
                run_id: 1,
                text: "hello".to_string(),
            }
            .is_terminal()
        );
        assert!(RunEvent::Finished { run_id: 1 }.is_terminal());
        assert!(
            RunEvent::Failed {
                run_id: 1,
                error: "boom".to_string(),
            }
            .is_terminal()
        );
        assert!(RunEvent::Cancelled { run_id: 1 }.is_terminal());
    }
}
