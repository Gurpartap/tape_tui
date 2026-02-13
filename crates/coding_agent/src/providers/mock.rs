use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use crate::provider::{CancelSignal, ProviderProfile, RunEvent, RunProvider, RunRequest};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockProvider {
    chunks: Vec<String>,
}

impl MockProvider {
    pub fn new(chunks: Vec<String>) -> Self {
        Self { chunks }
    }

    const RUN_DELAY_MS: u64 = 200;
    const TOKEN_DELAY_MS: u64 = 50;
}

impl Default for MockProvider {
    fn default() -> Self {
        Self {
            chunks: vec![
                "# Mocked README (Markdown Feature Showcase)\n".to_string(),
                "A streaming demonstration of **major Markdown features** while keeping `MockProvider` output deterministic.\n".to_string(),
                "\n".to_string(),
                "## 1. Text styling\n".to_string(),
                "- **bold**, *italic*, and ~~strikethrough~~ examples.\n".to_string(),
                "- Inline code with backticks: `cargo run -p coding_agent`.\n".to_string(),
                "- A link to [Tape TUI](https://github.com) and plain URL `https://example.com`.\n".to_string(),
                "\n".to_string(),
                "## 2. Headings and structure\n".to_string(),
                "### Level 3 heading\n".to_string(),
                "#### Level 4 heading\n".to_string(),
                "##### Level 5 heading\n".to_string(),
                "###### Level 6 heading\n".to_string(),
                "\n".to_string(),
                "## 3. Lists and checkboxes\n".to_string(),
                "- Unordered item one\n".to_string(),
                "  - Nested unordered item\n".to_string(),
                "1. Ordered step one\n".to_string(),
                "2. Ordered step two\n".to_string(),
                "- [x] Completed task from checklist\n".to_string(),
                "- [ ] Pending task from checklist\n".to_string(),
                "\n".to_string(),
                "## 4. Blockquotes\n".to_string(),
                "> The markdown renderer should preserve quote formatting.\n".to_string(),
                "> It can span multiple lines in a single quoted block.\n".to_string(),
                "\n".to_string(),
                "## 5. Code\n".to_string(),
                "```rust\n".to_string(),
                "fn main() {\n".to_string(),
                "    println!(\"Hello, Markdown\");\n".to_string(),
                "}\n".to_string(),
                "```\n".to_string(),
                "\n".to_string(),
                "## 6. Table\n".to_string(),
                "| Feature | Supported |\n".to_string(),
                "| --- | --- |\n".to_string(),
                "| Headings | yes |\n".to_string(),
                "| Lists | yes |\n".to_string(),
                "| Code blocks | yes |\n".to_string(),
                "| Task list | yes |\n".to_string(),
                "\n".to_string(),
                "---\n".to_string(),
                "\n".to_string(),
                "## 7. Closing\n".to_string(),
                "- Keep output compact and deterministic.\n".to_string(),
                "- Preserve markdown rendering boundaries.\n".to_string(),
                "- Stream completes with cleanup and a final status.\n".to_string(),
                "Completed successfully.\n".to_string(),
            ],
        }
    }
}

impl RunProvider for MockProvider {
    fn profile(&self) -> ProviderProfile {
        ProviderProfile {
            provider_id: "mock".to_string(),
            model_id: "mock".to_string(),
            thinking_level: Some("balanced".to_string()),
        }
    }

    fn run(
        &self,
        req: RunRequest,
        cancel: CancelSignal,
        emit: &mut dyn FnMut(RunEvent),
    ) -> Result<(), String> {
        let run_id = req.run_id;
        let _ = req.prompt;

        emit(RunEvent::Started { run_id });
        thread::sleep(Duration::from_millis(MockProvider::RUN_DELAY_MS));

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
                    thread::sleep(Duration::from_millis(MockProvider::TOKEN_DELAY_MS));
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
                thread::sleep(Duration::from_millis(MockProvider::TOKEN_DELAY_MS));
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
