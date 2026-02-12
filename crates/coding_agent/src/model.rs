use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::app::RunId;
use crate::tools::ToolExecutor;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRequest {
    pub run_id: RunId,
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEvent {
    Started { run_id: RunId },
    Chunk { run_id: RunId, text: String },
    Finished { run_id: RunId },
    Failed { run_id: RunId, error: String },
    Cancelled { run_id: RunId },
}

pub trait ModelBackend: Send + Sync + 'static {
    fn run(
        &self,
        req: RunRequest,
        cancel: Arc<AtomicBool>,
        emit: &mut dyn FnMut(RunEvent),
        tools: &mut dyn ToolExecutor,
    ) -> Result<(), String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockBackend {
    chunks: Vec<String>,
}

impl MockBackend {
    pub fn new(chunks: Vec<String>) -> Self {
        Self { chunks }
    }
}

impl Default for MockBackend {
    fn default() -> Self {
        Self {
            chunks: vec![
                "Mock response: planning...\n".to_string(),
                "Mock response: applying edits...\n".to_string(),
                "Mock response: done.".to_string(),
            ],
        }
    }
}

impl ModelBackend for MockBackend {
    fn run(
        &self,
        req: RunRequest,
        cancel: Arc<AtomicBool>,
        emit: &mut dyn FnMut(RunEvent),
        _tools: &mut dyn ToolExecutor,
    ) -> Result<(), String> {
        let run_id = req.run_id;
        let _ = req.prompt;

        emit(RunEvent::Started { run_id });

        if cancel.load(Ordering::SeqCst) {
            emit(RunEvent::Cancelled { run_id });
            return Ok(());
        }

        for chunk in &self.chunks {
            if cancel.load(Ordering::SeqCst) {
                emit(RunEvent::Cancelled { run_id });
                return Ok(());
            }

            emit(RunEvent::Chunk {
                run_id,
                text: chunk.clone(),
            });
        }

        if cancel.load(Ordering::SeqCst) {
            emit(RunEvent::Cancelled { run_id });
        } else {
            emit(RunEvent::Finished { run_id });
        }

        Ok(())
    }
}
