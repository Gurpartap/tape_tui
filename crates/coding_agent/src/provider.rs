use crate::tools::ToolExecutor;

pub use agent_provider::{CancelSignal, ProviderProfile, RunEvent, RunRequest};

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
        cancel: CancelSignal,
        emit: &mut dyn FnMut(RunEvent),
        tools: &mut dyn ToolExecutor,
    ) -> Result<(), String>;
}
