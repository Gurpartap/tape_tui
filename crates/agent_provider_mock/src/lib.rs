//! Deterministic mock implementation of the shared `agent_provider` contract.
//!
//! This crate contains no transport/protocol logic and is intended for local
//! development and contract-level integration testing.

use std::sync::atomic::Ordering;
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use agent_provider::{
    CancelSignal, ProviderProfile, RunEvent, RunProvider, RunRequest, ToolCallRequest, ToolResult,
};

/// Stable provider identifier used for explicit startup selection.
pub const MOCK_PROVIDER_ID: &str = "mock";

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectionState {
    model_index: usize,
    thinking_index: usize,
}

/// Deterministic mock provider used by `coding_agent` tests and local runs.
#[derive(Debug)]
pub struct MockProvider {
    chunks: Vec<String>,
    model_ids: Vec<String>,
    thinking_levels: Vec<Option<String>>,
    selection: Mutex<SelectionState>,
}

impl MockProvider {
    /// Creates a mock provider with caller-provided chunks and default profile options.
    #[must_use]
    pub fn new(chunks: Vec<String>) -> Self {
        Self::with_profile_options(
            chunks,
            vec!["mock".to_string(), "mock-alt".to_string()],
            vec![Some("balanced".to_string()), Some("deep".to_string())],
        )
    }

    /// Creates a mock provider with explicit profile cycling options.
    #[must_use]
    pub fn with_profile_options(
        chunks: Vec<String>,
        model_ids: Vec<String>,
        thinking_levels: Vec<Option<String>>,
    ) -> Self {
        let model_ids = sanitize_model_ids(model_ids);
        let thinking_levels = sanitize_thinking_levels(thinking_levels);

        Self {
            chunks,
            model_ids,
            thinking_levels,
            selection: Mutex::new(SelectionState {
                model_index: 0,
                thinking_index: 0,
            }),
        }
    }

    fn profile_for_selection(&self, selection: &SelectionState) -> ProviderProfile {
        ProviderProfile {
            provider_id: MOCK_PROVIDER_ID.to_string(),
            model_id: self.model_ids[selection.model_index].clone(),
            thinking_level: self.thinking_levels[selection.thinking_index].clone(),
        }
    }

    const RUN_DELAY_MS: u64 = 200;
    const TOKEN_DELAY_MS: u64 = 50;
}

impl Default for MockProvider {
    fn default() -> Self {
        Self::new(vec![
            "# Mocked README (Markdown Feature Showcase)\n".to_string(),
            "A streaming demonstration of **major Markdown features** while keeping `MockProvider` output deterministic.\n".to_string(),
            "\n".to_string(),
            "## 1. Text styling\n".to_string(),
            "- **bold**, *italic*, and ~~strikethrough~~ examples.\n".to_string(),
            "- Inline code with backticks: `cargo run -p coding_agent`.\n".to_string(),
            "- A link to [Tape TUI](https://github.com) and plain URL `https://example.com`.\n"
                .to_string(),
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
        ])
    }
}

impl RunProvider for MockProvider {
    fn profile(&self) -> ProviderProfile {
        let selection = lock_unpoisoned(&self.selection);
        self.profile_for_selection(&selection)
    }

    fn cycle_model(&self) -> Result<ProviderProfile, String> {
        let mut selection = lock_unpoisoned(&self.selection);
        selection.model_index = (selection.model_index + 1) % self.model_ids.len();
        Ok(self.profile_for_selection(&selection))
    }

    fn cycle_thinking_level(&self) -> Result<ProviderProfile, String> {
        let mut selection = lock_unpoisoned(&self.selection);
        selection.thinking_index = (selection.thinking_index + 1) % self.thinking_levels.len();
        Ok(self.profile_for_selection(&selection))
    }

    fn run(
        &self,
        req: RunRequest,
        cancel: CancelSignal,
        _execute_tool: &mut dyn FnMut(ToolCallRequest) -> ToolResult,
        emit: &mut dyn FnMut(RunEvent),
    ) -> Result<(), String> {
        let run_id = req.run_id;
        let _ = req.prompt;
        let _ = req.instructions;

        emit(RunEvent::Started { run_id });
        thread::sleep(Duration::from_millis(Self::RUN_DELAY_MS));

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
                    thread::sleep(Duration::from_millis(Self::TOKEN_DELAY_MS));
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
                thread::sleep(Duration::from_millis(Self::TOKEN_DELAY_MS));
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

fn sanitize_model_ids(model_ids: Vec<String>) -> Vec<String> {
    let mut sanitized: Vec<String> = model_ids
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();

    if sanitized.is_empty() {
        sanitized.push("mock".to_string());
    }

    sanitized
}

fn sanitize_thinking_levels(thinking_levels: Vec<Option<String>>) -> Vec<Option<String>> {
    let mut sanitized: Vec<Option<String>> = thinking_levels
        .into_iter()
        .map(|value| {
            value.and_then(|level| {
                let trimmed = level.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            })
        })
        .collect();

    if sanitized.is_empty() {
        sanitized.push(Some("balanced".to_string()));
    }

    sanitized
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use super::*;

    fn collect_events(provider: &MockProvider, cancel: CancelSignal) -> Vec<RunEvent> {
        let mut events = Vec::new();
        provider
            .run(
                RunRequest {
                    run_id: 7,
                    prompt: "test".to_string(),
                    instructions: "system instructions".to_string(),
                },
                cancel,
                &mut |_call| ToolResult::error("unused", "unused", "not used in mock tests"),
                &mut |event| events.push(event),
            )
            .expect("mock run should succeed");
        events
    }

    #[test]
    fn profile_exposes_explicit_mock_provider_identity() {
        let profile = MockProvider::new(Vec::new()).profile();

        assert_eq!(profile.provider_id, MOCK_PROVIDER_ID);
        assert_eq!(profile.model_id, "mock");
        assert_eq!(profile.thinking_level.as_deref(), Some("balanced"));
    }

    #[test]
    fn cycle_hooks_rotate_model_and_thinking_profiles() {
        let provider = MockProvider::new(Vec::new());
        let initial = provider.profile();

        let model_switched = provider
            .cycle_model()
            .expect("model cycling should be supported");
        assert_ne!(model_switched.model_id, initial.model_id);

        let thinking_switched = provider
            .cycle_thinking_level()
            .expect("thinking cycling should be supported");
        assert_ne!(thinking_switched.thinking_level, initial.thinking_level);
    }

    #[test]
    fn run_emits_started_chunks_and_finished() {
        let provider = MockProvider::new(vec!["one two".to_string()]);
        let cancel = Arc::new(AtomicBool::new(false));

        let events = collect_events(&provider, cancel);

        assert!(matches!(
            events.first(),
            Some(RunEvent::Started { run_id: 7 })
        ));
        assert!(matches!(
            events.last(),
            Some(RunEvent::Finished { run_id: 7 })
        ));
        assert!(events
            .iter()
            .any(|event| matches!(event, RunEvent::Chunk { text, .. } if !text.is_empty())));
    }

    #[test]
    fn run_emits_cancelled_when_cancel_is_set() {
        let provider = MockProvider::new(vec!["ignored".to_string()]);
        let cancel = Arc::new(AtomicBool::new(true));

        let events = collect_events(&provider, cancel);

        assert!(matches!(
            events.first(),
            Some(RunEvent::Started { run_id: 7 })
        ));
        assert!(matches!(
            events.last(),
            Some(RunEvent::Cancelled { run_id: 7 })
        ));
    }

    #[test]
    fn empty_profile_options_fallback_to_safe_defaults() {
        let provider = MockProvider::with_profile_options(Vec::new(), Vec::new(), Vec::new());
        let profile = provider.profile();

        assert_eq!(profile.model_id, "mock");
        assert_eq!(profile.thinking_level.as_deref(), Some("balanced"));
    }
}
