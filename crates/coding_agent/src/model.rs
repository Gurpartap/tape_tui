use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::app::RunId;
use crate::runtime::RunEvent;
use crate::tools::ToolExecutor;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRequest {
    pub run_id: RunId,
    pub prompt: String,
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

    const RUN_DELAY_MS: u64 = 200;
    const TOKEN_DELAY_MS: u64 = 50;
}

impl Default for MockBackend {
    fn default() -> Self {
        Self {
            chunks: vec![
                "Processing the request with the active workspace context. ".to_string(),
                "Analyzing prompt constraints and required actions. ".to_string(),
                "Reviewing files and checking command expectations. ".to_string(),
                "Collecting relevant execution context. ".to_string(),
                "Preparing a concise step-by-step plan. ".to_string(),
                "Applying minimal edits and preserving current behavior. ".to_string(),
                "Validating terminal interaction and cursor semantics. ".to_string(),
                "Updating streamed output progressively. ".to_string(),
                "Applying edits and patching components. ".to_string(),
                "Writing only the necessary changes. ".to_string(),
                "Keeping flow deterministic and inline-first. ".to_string(),
                "Preserving structured rendering and command behavior. ".to_string(),
                "Refreshing status visibility and command guidance. ".to_string(),
                "Verifying command semantics and run lifecycle transitions. ".to_string(),
                "Coordinating render wakeups where needed. ".to_string(),
                "Finalizing summary and follow-up details. ".to_string(),
                "Completed.\n".to_string(),
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
        thread::sleep(Duration::from_millis(MockBackend::RUN_DELAY_MS));

        if cancel.load(Ordering::SeqCst) {
            emit(RunEvent::Cancelled { run_id });
            return Ok(());
        }

        for chunk in &self.chunks {
            if cancel.load(Ordering::SeqCst) {
                emit(RunEvent::Cancelled { run_id });
                return Ok(());
            }

            let mut pending_token = String::new();
            for ch in chunk.chars() {
                pending_token.push(ch);

                if matches!(ch, ' ' | '\n') {
                    emit(RunEvent::Chunk {
                        run_id,
                        text: std::mem::take(&mut pending_token),
                    });
                    thread::sleep(Duration::from_millis(MockBackend::TOKEN_DELAY_MS));
                }
            }

            if !pending_token.is_empty() {
                if cancel.load(Ordering::SeqCst) {
                    emit(RunEvent::Cancelled { run_id });
                    return Ok(());
                }

                emit(RunEvent::Chunk {
                    run_id,
                    text: pending_token,
                });
                thread::sleep(Duration::from_millis(MockBackend::TOKEN_DELAY_MS));
            }
        }

        if cancel.load(Ordering::SeqCst) {
            emit(RunEvent::Cancelled { run_id });
        } else {
            emit(RunEvent::Finished { run_id });
        }

        Ok(())
    }
}
