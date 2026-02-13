use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::app::RunId;
use crate::runtime::RunEvent;
use crate::tools::ToolExecutor;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRequest {
    pub run_id: RunId,
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderProfile {
    pub provider_id: String,
    pub model_id: String,
    pub thinking_label: Option<String>,
}

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
