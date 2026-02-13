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
