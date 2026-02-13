use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::app::RunId;
use crate::runtime::RunEvent;
use crate::tools::ToolExecutor;

/// Immutable input for a single provider run invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRequest {
    /// Unique identifier for the active run.
    pub run_id: RunId,
    /// User prompt captured at submit time.
    pub prompt: String,
}

/// Provider + model metadata rendered in the TUI footer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderProfile {
    /// Stable provider identifier (for example `mock`).
    pub provider_id: String,
    /// Concrete model identifier selected by the provider.
    pub model_id: String,
    /// Optional thinking mode label exposed by the provider/model pair.
    pub thinking_label: Option<String>,
}

/// Contract for run providers used by `coding_agent`.
///
/// Implementations should emit [`RunEvent`] values for exactly one run lifecycle:
///
/// 1. All events must use `req.run_id`.
/// 2. Exactly one terminal event must be emitted (`Finished`, `Failed`, or `Cancelled`).
/// 3. Cancellation should be observed promptly and finalized with `RunEvent::Cancelled`.
///
/// Providers should communicate through emitted events and tool execution only;
/// runtime and rendering side effects are owned by `RuntimeController` + `App`.
pub trait RunProvider: Send + Sync + 'static {
    fn profile(&self) -> ProviderProfile;

    fn run(
        &self,
        req: RunRequest,
        cancel: Arc<AtomicBool>,
        emit: &mut dyn FnMut(RunEvent),
        tools: &mut dyn ToolExecutor,
    ) -> Result<(), String>;
}
