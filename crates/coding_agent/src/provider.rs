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

pub trait RunProvider: Send + Sync + 'static {
    fn run(
        &self,
        req: RunRequest,
        cancel: Arc<AtomicBool>,
        emit: &mut dyn FnMut(RunEvent),
        tools: &mut dyn ToolExecutor,
    ) -> Result<(), String>;
}
