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
                "## Mocked coding agent run\n".to_string(),
                "- Reading context and **validating constraints**.\n".to_string(),
                "- **Analyzing** prompt requirements and workspace layout.\n".to_string(),
                "- Reviewing files and command expectations.\n".to_string(),
                "- Tracking execution as deterministic steps in `app.rs` and `tui.rs`.\n".to_string(),
                "\n`rendered` output is now markdown-aware.\n".to_string(),
                "### Streaming plan\n".to_string(),
                "- Prepare minimal edits.\n".to_string(),
                "- Preserve runtime behavior.\n".to_string(),
                "- Keep inline-first and teardown-safe changes.\n".to_string(),
                "### Notes\n".to_string(),
                "- Use bold for key actions and backticks for commands.\n".to_string(),
                "- Emit status updates as list items.\n".to_string(),
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
