use crate::commands::{parse_slash_command, SlashCommand};
use crate::provider::RunMessage;

pub type RunId = u64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Idle,
    Running { run_id: RunId },
    Error(String),
    Exiting,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    System,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub role: Role,
    pub content: String,
    pub streaming: bool,
    pub run_id: Option<RunId>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct InputHistory {
    entries: Vec<String>,
    cursor: Option<usize>,
    draft: Option<String>,
}

impl InputHistory {
    fn entries(&self) -> &[String] {
        &self.entries
    }

    fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    fn record_entry(&mut self, text: String) {
        self.entries.push(text);
        self.cursor = None;
        self.draft = None;
    }

    fn reset_navigation(&mut self) {
        self.cursor = None;
        self.draft = None;
    }

    fn previous(&mut self, current_input: &str) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }

        if self.cursor.is_some_and(|index| index >= self.entries.len()) {
            self.cursor = None;
        }

        if self.cursor.is_none() {
            self.draft = Some(current_input.to_string());
        }

        let new_cursor = match self.cursor {
            Some(index) if index > 0 => index - 1,
            Some(index) => index,
            None => self.entries.len() - 1,
        };

        self.cursor = Some(new_cursor);
        Some(self.entries[new_cursor].clone())
    }

    fn next(&mut self) -> Option<String> {
        let current = self.cursor?;

        if current >= self.entries.len() || current + 1 >= self.entries.len() {
            self.cursor = None;
            return Some(self.draft.take().unwrap_or_default());
        }

        let next = current + 1;
        self.cursor = Some(next);
        Some(self.entries[next].clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingRunMemory {
    run_id: RunId,
    entries: Vec<RunMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    pub mode: Mode,
    pub input: String,
    pub transcript: Vec<Message>,
    conversation: Vec<RunMessage>,
    pending_run_memory: Option<PendingRunMemory>,
    history: InputHistory,
    pub should_exit: bool,
    cancelling_run: Option<RunId>,
    system_instructions: String,
}

pub trait HostOps {
    fn start_run(
        &mut self,
        messages: Vec<RunMessage>,
        instructions: String,
    ) -> Result<RunId, String>;
    fn cancel_run(&mut self, run_id: RunId);
    fn request_render(&mut self);
    fn request_stop(&mut self);
}

const HELP_TEXT: &str = "Commands: /help, /clear, /cancel, /quit";
const ERROR_RUN_ALREADY_ACTIVE: &str = "Run already active";
pub const SYSTEM_INSTRUCTIONS_ENV_VAR: &str = "CODING_AGENT_SYSTEM_INSTRUCTIONS";
pub const DEFAULT_SYSTEM_INSTRUCTIONS: &str =
    "You are a careful coding agent. Follow user requests exactly, keep output deterministic, and fail explicitly when constraints cannot be satisfied.";

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

pub fn system_instructions_from_env() -> String {
    let from_env = std::env::var(SYSTEM_INSTRUCTIONS_ENV_VAR).ok();
    sanitize_system_instructions(from_env)
}

fn sanitize_system_instructions(raw: Option<String>) -> String {
    let Some(value) = raw else {
        return DEFAULT_SYSTEM_INSTRUCTIONS.to_string();
    };

    let trimmed = value.trim();
    if trimmed.is_empty() {
        DEFAULT_SYSTEM_INSTRUCTIONS.to_string()
    } else {
        trimmed.to_string()
    }
}

impl App {
    pub fn new() -> Self {
        Self::with_system_instructions(None)
    }

    pub fn with_system_instructions(system_instructions: Option<String>) -> Self {
        Self {
            mode: Mode::Idle,
            input: String::new(),
            transcript: Vec::new(),
            conversation: Vec::new(),
            pending_run_memory: None,
            history: InputHistory::default(),
            should_exit: false,
            cancelling_run: None,
            system_instructions: sanitize_system_instructions(system_instructions),
        }
    }

    pub fn system_instructions(&self) -> &str {
        &self.system_instructions
    }

    /// Returns model-facing conversation messages retained across turns.
    pub fn conversation_messages(&self) -> &[RunMessage] {
        &self.conversation
    }

    /// Returns tool-call arguments for a run/call identifier when present in
    /// pending run memory (active run) or committed conversation history.
    pub fn tool_call_arguments(&self, run_id: RunId, call_id: &str) -> Option<&serde_json::Value> {
        if let Some(pending) = self.pending_run_memory.as_ref() {
            if pending.run_id == run_id {
                if let Some(arguments) =
                    pending.entries.iter().rev().find_map(|entry| match entry {
                        RunMessage::ToolCall {
                            call_id: pending_call_id,
                            arguments,
                            ..
                        } if pending_call_id == call_id => Some(arguments),
                        _ => None,
                    })
                {
                    return Some(arguments);
                }
            }
        }

        self.conversation
            .iter()
            .rev()
            .find_map(|entry| match entry {
                RunMessage::ToolCall {
                    call_id: conversation_call_id,
                    arguments,
                    ..
                } if conversation_call_id == call_id => Some(arguments),
                _ => None,
            })
    }

    fn run_messages_with_pending_user_prompt(&self, prompt: &str) -> Vec<RunMessage> {
        let mut messages = self.conversation.clone();
        messages.push(RunMessage::UserText {
            text: prompt.to_string(),
        });
        messages
    }

    fn rollback_submitted_user_turn(&mut self, prompt: &str) {
        self.rollback_last_history_entry_if_matches(prompt);
        self.rollback_last_transcript_user_message_if_matches(prompt);
        self.rollback_last_conversation_user_message_if_matches(prompt);
    }

    fn rollback_last_history_entry_if_matches(&mut self, prompt: &str) {
        if self
            .history
            .entries
            .last()
            .is_some_and(|entry| entry == prompt)
        {
            self.history.entries.pop();
        }
    }

    fn rollback_last_transcript_user_message_if_matches(&mut self, prompt: &str) {
        if self.transcript.last().is_some_and(|message| {
            message.role == Role::User
                && message.content == prompt
                && !message.streaming
                && message.run_id.is_none()
        }) {
            self.transcript.pop();
        }
    }

    fn rollback_last_conversation_user_message_if_matches(&mut self, prompt: &str) {
        if self.conversation.last().is_some_and(
            |message| matches!(message, RunMessage::UserText { text } if text == prompt),
        ) {
            self.conversation.pop();
        }
    }

    pub fn on_input_replace(&mut self, text: String) {
        self.input = text;
        self.history.reset_navigation();
    }

    /// Returns submitted prompt history in chronological order.
    pub fn history_entries(&self) -> &[String] {
        self.history.entries()
    }

    /// Returns the current history navigation cursor, if active.
    pub fn history_cursor(&self) -> Option<usize> {
        self.history.cursor()
    }

    /// Appends an entry to prompt history and resets any active history navigation state.
    pub fn push_history_entry(&mut self, text: impl Into<String>) {
        self.history.record_entry(text.into());
    }

    /// Moves to the previous history entry and replaces the active input when possible.
    pub fn on_input_history_previous(&mut self) {
        if let Some(previous) = self.history.previous(&self.input) {
            self.input = previous;
        }
    }

    /// Moves to the next history entry (or draft) and replaces the active input when possible.
    pub fn on_input_history_next(&mut self) {
        if let Some(next) = self.history.next() {
            self.input = next;
        }
    }

    /// Appends a system message to transcript without mutating control state.
    pub fn push_system_message(&mut self, content: impl Into<String>) {
        self.push_system(content.into());
    }

    pub fn on_submit(&mut self, host: &mut dyn HostOps) {
        let submitted = std::mem::take(&mut self.input);
        let prompt = submitted.trim().to_string();

        if prompt.is_empty() {
            host.request_render();
            return;
        }

        if let Some(command) = parse_slash_command(&prompt) {
            match command {
                SlashCommand::Help => {
                    self.push_system(HELP_TEXT.to_string());
                    host.request_render();
                }
                SlashCommand::Clear => {
                    self.transcript.clear();
                    self.conversation.clear();
                    self.pending_run_memory = None;
                    self.push_system("Transcript cleared".to_string());
                    host.request_render();
                }
                SlashCommand::Cancel => {
                    self.on_cancel(host);
                }
                SlashCommand::Quit => {
                    self.on_quit(host);
                }
                SlashCommand::Unknown(command) => {
                    self.push_system(format!("Unknown command: {command}"));
                    host.request_render();
                }
            }

            return;
        }

        if matches!(self.mode, Mode::Running { .. }) {
            self.push_system("Run already in progress. Use /cancel to stop it.".to_string());
            host.request_render();
            return;
        }

        if self.cancelling_run.is_some() {
            self.push_system("Cancelling active run, please wait.".to_string());
            host.request_render();
            return;
        }

        let run_messages = self.run_messages_with_pending_user_prompt(&prompt);

        self.push_history_entry(prompt.clone());
        self.transcript.push(Message {
            role: Role::User,
            content: prompt.clone(),
            streaming: false,
            run_id: None,
        });
        self.conversation.push(RunMessage::UserText {
            text: prompt.clone(),
        });

        match host.start_run(run_messages, self.system_instructions.clone()) {
            Ok(run_id) => {
                self.mode = Mode::Running { run_id };
            }
            Err(error) => {
                if error == ERROR_RUN_ALREADY_ACTIVE {
                    self.rollback_submitted_user_turn(&prompt);
                    self.push_system(
                        "Run already in progress. Use /cancel to stop it.".to_string(),
                    );
                } else {
                    self.mode = Mode::Error(error.clone());
                    self.push_system(format!("Failed to start run: {error}"));
                }
            }
        }

        host.request_render();
    }

    pub fn on_cancel(&mut self, host: &mut dyn HostOps) {
        if self.cancelling_run.is_some() {
            host.request_render();
            return;
        }

        if let Mode::Running { run_id } = self.mode {
            self.cancelling_run = Some(run_id);
            self.finalize_stream(run_id);
            self.mode = Mode::Idle;
            self.push_system("Run cancelled".to_string());
            host.cancel_run(run_id);
        } else {
            self.push_system("No active run".to_string());
        }

        host.request_render();
    }

    pub fn on_control_c(&mut self, host: &mut dyn HostOps) {
        if !self.input.is_empty() {
            self.on_input_replace(String::new());
            host.request_render();
            return;
        }

        if matches!(self.mode, Mode::Running { .. }) {
            self.on_cancel(host);
            return;
        }

        self.on_quit(host);
    }

    pub fn on_quit(&mut self, host: &mut dyn HostOps) {
        self.mode = Mode::Exiting;
        self.should_exit = true;
        host.request_stop();
        host.request_render();
    }

    pub fn on_run_started(&mut self, run_id: RunId) {
        if !self.is_active_run(run_id) {
            return;
        }

        if self.is_cancelling(run_id) || self.has_assistant_for_run(run_id) {
            return;
        }

        self.transcript.push(Message {
            role: Role::Assistant,
            content: String::new(),
            streaming: true,
            run_id: Some(run_id),
        });
    }

    pub fn on_run_chunk(&mut self, run_id: RunId, chunk: &str) {
        if !self.is_active_run(run_id) && !self.is_cancelling(run_id) {
            return;
        }

        let stream_active = !self.is_cancelling(run_id);

        if let Some(message) = self
            .transcript
            .iter_mut()
            .rev()
            .find(|message| message.role == Role::Assistant && message.run_id == Some(run_id))
        {
            message.content.push_str(chunk);
            if !stream_active {
                message.streaming = false;
            }
        } else {
            self.transcript.push(Message {
                role: Role::Assistant,
                content: chunk.to_string(),
                streaming: stream_active,
                run_id: Some(run_id),
            });
        }

        self.append_pending_assistant_chunk(run_id, chunk);
    }

    pub fn on_tool_call_started(
        &mut self,
        run_id: RunId,
        call_id: &str,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) {
        if !self.should_apply_run_event(run_id) {
            return;
        }

        self.append_pending_tool_call(run_id, call_id, tool_name, arguments);
        self.push_tool(run_id, format!("Tool {tool_name} ({call_id}) started"));
    }

    pub fn on_tool_call_finished(
        &mut self,
        run_id: RunId,
        tool_name: &str,
        call_id: &str,
        is_error: bool,
        content: &serde_json::Value,
        content_text: &str,
    ) {
        if !self.should_apply_run_event(run_id) {
            return;
        }

        self.append_pending_tool_result(run_id, tool_name, call_id, is_error, content);

        let mut message = format!(
            "Tool {tool_name} ({call_id}) {}",
            if is_error { "failed" } else { "completed" }
        );

        if is_error && !content_text.is_empty() {
            message.push_str(": ");
            message.push_str(content_text);
        }

        self.push_tool(run_id, message);
    }

    pub fn on_run_finished(&mut self, run_id: RunId) {
        if !self.should_apply_run_event(run_id) {
            return;
        }

        if self.is_cancelling(run_id) {
            self.finalize_stream(run_id);
            self.discard_pending_run_memory(run_id);
            self.finalize_cancelled_run(run_id);
            return;
        }

        if !self.is_active_run(run_id) {
            return;
        }

        self.finalize_stream(run_id);
        self.commit_pending_run_memory(run_id);
        self.mode = Mode::Idle;
    }

    pub fn on_run_failed(&mut self, run_id: RunId, error: &str) {
        if !self.should_apply_run_event(run_id) {
            return;
        }

        if self.is_cancelling(run_id) {
            self.finalize_stream(run_id);
            self.discard_pending_run_memory(run_id);
            self.finalize_cancelled_run(run_id);
            return;
        }

        if !self.is_active_run(run_id) {
            return;
        }

        self.finalize_stream(run_id);
        self.discard_pending_run_memory(run_id);
        self.mode = Mode::Error(error.to_string());
        self.push_system(format!("Run failed: {error}"));
    }

    pub fn on_run_cancelled(&mut self, run_id: RunId) {
        if !self.should_apply_run_event(run_id) || !self.is_cancelling(run_id) {
            return;
        }

        self.finalize_stream(run_id);
        self.discard_pending_run_memory(run_id);
        self.finalize_cancelled_run(run_id);
    }

    fn ensure_pending_run_memory(&mut self, run_id: RunId) -> &mut PendingRunMemory {
        if self.pending_run_memory.is_none() {
            self.pending_run_memory = Some(PendingRunMemory {
                run_id,
                entries: Vec::new(),
            });
        }

        let pending = self
            .pending_run_memory
            .as_mut()
            .expect("pending run memory must be initialized");
        assert_eq!(
            pending.run_id, run_id,
            "pending run memory belongs to run {}, cannot append event for run {run_id}",
            pending.run_id
        );

        pending
    }

    fn append_pending_assistant_chunk(&mut self, run_id: RunId, chunk: &str) {
        if chunk.is_empty() {
            return;
        }

        let pending = self.ensure_pending_run_memory(run_id);
        if let Some(RunMessage::AssistantText { text }) = pending.entries.last_mut() {
            text.push_str(chunk);
            return;
        }

        pending.entries.push(RunMessage::AssistantText {
            text: chunk.to_string(),
        });
    }

    fn append_pending_tool_call(
        &mut self,
        run_id: RunId,
        call_id: &str,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) {
        let pending = self.ensure_pending_run_memory(run_id);
        pending.entries.push(RunMessage::ToolCall {
            call_id: call_id.to_string(),
            tool_name: tool_name.to_string(),
            arguments: arguments.clone(),
        });
    }

    fn append_pending_tool_result(
        &mut self,
        run_id: RunId,
        tool_name: &str,
        call_id: &str,
        is_error: bool,
        content: &serde_json::Value,
    ) {
        let pending = self.ensure_pending_run_memory(run_id);
        pending.entries.push(RunMessage::ToolResult {
            call_id: call_id.to_string(),
            tool_name: tool_name.to_string(),
            content: content.clone(),
            is_error,
        });
    }

    fn commit_pending_run_memory(&mut self, run_id: RunId) {
        let Some(pending) = self.pending_run_memory.take() else {
            return;
        };

        assert_eq!(
            pending.run_id, run_id,
            "pending run memory belongs to run {}, cannot commit run {run_id}",
            pending.run_id
        );

        self.conversation.extend(pending.entries);
    }

    fn discard_pending_run_memory(&mut self, run_id: RunId) {
        let Some(pending) = self.pending_run_memory.take() else {
            return;
        };

        assert_eq!(
            pending.run_id, run_id,
            "pending run memory belongs to run {}, cannot discard run {run_id}",
            pending.run_id
        );
    }

    fn should_apply_run_event(&self, run_id: RunId) -> bool {
        !self.should_exit && (self.is_active_run(run_id) || self.is_cancelling(run_id))
    }

    fn is_active_run(&self, run_id: RunId) -> bool {
        matches!(self.mode, Mode::Running { run_id: active } if active == run_id)
    }

    fn finalize_stream(&mut self, run_id: RunId) {
        let mut first_index = None;
        let mut merged = String::new();

        for (index, message) in self.transcript.iter().enumerate() {
            if message.role == Role::Assistant && message.run_id == Some(run_id) {
                if first_index.is_none() {
                    first_index = Some(index);
                }
                merged.push_str(&message.content);
            }
        }

        let Some(first_index) = first_index else {
            return;
        };

        self.transcript[first_index].content = merged;
        self.transcript[first_index].streaming = false;

        let mut index = self.transcript.len();
        while index > first_index + 1 {
            index -= 1;
            if self.transcript[index].role == Role::Assistant
                && self.transcript[index].run_id == Some(run_id)
            {
                self.transcript.remove(index);
            }
        }
    }

    fn has_assistant_for_run(&self, run_id: RunId) -> bool {
        self.transcript
            .iter()
            .any(|message| message.role == Role::Assistant && message.run_id == Some(run_id))
    }

    fn is_cancelling(&self, run_id: RunId) -> bool {
        self.cancelling_run == Some(run_id)
    }

    fn finalize_cancelled_run(&mut self, run_id: RunId) {
        if !self.is_cancelling(run_id) {
            return;
        }

        self.cancelling_run = None;
        self.mode = Mode::Idle;
        self.finalize_stream(run_id);
    }

    fn push_tool(&mut self, run_id: RunId, content: String) {
        self.transcript.push(Message {
            role: Role::Tool,
            content,
            streaming: false,
            run_id: Some(run_id),
        });
    }

    fn push_system(&mut self, content: String) {
        self.transcript.push(Message {
            role: Role::System,
            content,
            streaming: false,
            run_id: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, MutexGuard, OnceLock};

    use super::*;

    struct EnvVarGuard {
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(value: Option<&str>) -> Self {
            let previous = std::env::var(SYSTEM_INSTRUCTIONS_ENV_VAR).ok();
            match value {
                Some(value) => std::env::set_var(SYSTEM_INSTRUCTIONS_ENV_VAR, value),
                None => std::env::remove_var(SYSTEM_INSTRUCTIONS_ENV_VAR),
            }

            Self { previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(SYSTEM_INSTRUCTIONS_ENV_VAR, value),
                None => std::env::remove_var(SYSTEM_INSTRUCTIONS_ENV_VAR),
            }
        }
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        match mutex.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn assistant_message(content: &str, streaming: bool, run_id: Option<RunId>) -> Message {
        Message {
            role: Role::Assistant,
            content: content.to_string(),
            streaming,
            run_id,
        }
    }

    #[test]
    fn system_instructions_env_falls_back_to_default_when_unset_or_blank() {
        let _env_serialization = lock_unpoisoned(env_lock());

        {
            let _guard = EnvVarGuard::set(None);
            assert_eq!(system_instructions_from_env(), DEFAULT_SYSTEM_INSTRUCTIONS);
        }

        {
            let _guard = EnvVarGuard::set(Some("   \n\t"));
            assert_eq!(system_instructions_from_env(), DEFAULT_SYSTEM_INSTRUCTIONS);
        }
    }

    #[test]
    fn system_instructions_env_uses_trimmed_override_when_set() {
        let _env_serialization = lock_unpoisoned(env_lock());
        let _guard = EnvVarGuard::set(Some("  custom system instruction  "));

        assert_eq!(system_instructions_from_env(), "custom system instruction");
    }

    #[test]
    fn finalize_stream_merges_assistant_chunks_for_same_run() {
        let mut app = App::new();
        app.transcript
            .push(assistant_message("first ", true, Some(42)));
        app.transcript.push(Message {
            role: Role::User,
            content: "ignored".to_string(),
            streaming: false,
            run_id: None,
        });
        app.transcript
            .push(assistant_message("second", true, Some(42)));

        app.finalize_stream(42);

        assert_eq!(app.transcript.len(), 2);
        assert_eq!(
            app.transcript
                .iter()
                .filter(|message| message.role == Role::Assistant && message.run_id == Some(42))
                .count(),
            1
        );
        let merged = app
            .transcript
            .iter()
            .find(|message| message.role == Role::Assistant && message.run_id == Some(42))
            .expect("merged assistant message exists");
        assert_eq!(merged.content, "first second");
        assert!(!merged.streaming);
    }

    #[test]
    fn on_run_started_does_not_duplicate_assistant_entry_for_active_run() {
        let mut app = App::new();
        app.mode = Mode::Running { run_id: 7 };
        app.transcript
            .push(assistant_message("seed", false, Some(7)));
        app.transcript
            .push(assistant_message("newer", true, Some(7)));

        let assistant_count_before = app
            .transcript
            .iter()
            .filter(|message| message.role == Role::Assistant && message.run_id == Some(7))
            .count();

        app.on_run_started(7);
        assert_eq!(
            app.transcript
                .iter()
                .filter(|message| message.role == Role::Assistant && message.run_id == Some(7))
                .count(),
            assistant_count_before
        );
    }

    #[test]
    fn tool_timeline_messages_are_scoped_to_active_run() {
        let mut app = App::new();
        app.mode = Mode::Running { run_id: 9 };

        app.on_tool_call_started(
            9,
            "call-1",
            "read",
            &serde_json::json!({ "path": "README.md" }),
        );
        app.on_tool_call_finished(
            9,
            "read",
            "call-1",
            true,
            &serde_json::json!("missing file"),
            "missing file",
        );
        app.on_tool_call_started(999, "stale", "bash", &serde_json::json!({}));
        app.on_run_finished(9);

        let tool_messages: Vec<_> = app
            .transcript
            .iter()
            .filter(|message| message.role == Role::Tool)
            .collect();

        assert_eq!(tool_messages.len(), 2);
        assert_eq!(tool_messages[0].content, "Tool read (call-1) started");
        assert_eq!(
            tool_messages[1].content,
            "Tool read (call-1) failed: missing file"
        );
        assert_eq!(tool_messages[0].run_id, Some(9));
        assert_eq!(tool_messages[1].run_id, Some(9));
        assert_eq!(
            app.conversation_messages(),
            &[
                RunMessage::ToolCall {
                    call_id: "call-1".to_string(),
                    tool_name: "read".to_string(),
                    arguments: serde_json::json!({ "path": "README.md" }),
                },
                RunMessage::ToolResult {
                    call_id: "call-1".to_string(),
                    tool_name: "read".to_string(),
                    content: serde_json::json!("missing file"),
                    is_error: true,
                }
            ]
        );
    }

    #[test]
    fn tool_timeline_messages_ignore_stale_run_while_cancelling() {
        let mut app = App::new();
        app.mode = Mode::Idle;
        app.cancelling_run = Some(14);

        app.on_tool_call_started(99, "stale-start", "write", &serde_json::json!({}));
        app.on_tool_call_started(
            14,
            "call-2",
            "bash",
            &serde_json::json!({ "command": "pwd" }),
        );
        app.on_tool_call_finished(
            99,
            "write",
            "stale-finish",
            true,
            &serde_json::json!("stale"),
            "stale",
        );
        app.on_tool_call_finished(
            14,
            "bash",
            "call-2",
            false,
            &serde_json::json!("ignored success content"),
            "ignored success content",
        );
        app.on_run_cancelled(14);

        let tool_messages: Vec<_> = app
            .transcript
            .iter()
            .filter(|message| message.role == Role::Tool)
            .collect();

        assert_eq!(tool_messages.len(), 2);
        assert_eq!(tool_messages[0].content, "Tool bash (call-2) started");
        assert_eq!(tool_messages[1].content, "Tool bash (call-2) completed");
        assert!(tool_messages
            .iter()
            .all(|message| message.run_id == Some(14)));
        assert!(app.conversation_messages().is_empty());
    }

    #[test]
    fn tool_call_arguments_resolve_pending_then_conversation_history() {
        let mut app = App::new();
        app.mode = Mode::Running { run_id: 77 };

        app.on_tool_call_started(
            77,
            "call-pending",
            "bash",
            &serde_json::json!({ "command": "pwd" }),
        );

        assert_eq!(
            app.tool_call_arguments(77, "call-pending"),
            Some(&serde_json::json!({ "command": "pwd" }))
        );

        app.on_run_finished(77);
        assert_eq!(app.mode, Mode::Idle);
        assert_eq!(
            app.tool_call_arguments(77, "call-pending"),
            Some(&serde_json::json!({ "command": "pwd" }))
        );
        assert_eq!(app.tool_call_arguments(77, "missing"), None);
    }

    #[test]
    fn input_history_up_down_recall() {
        let mut app = App::new();
        app.history.entries.push("first command".to_string());
        app.history.entries.push("second command".to_string());
        app.history.entries.push("third command".to_string());
        app.input = "edited".to_string();

        app.on_input_history_previous();
        assert_eq!(app.input, "third command");
        assert_eq!(app.history.cursor, Some(2));

        app.on_input_history_previous();
        assert_eq!(app.input, "second command");
        assert_eq!(app.history.cursor, Some(1));

        app.on_input_history_previous();
        assert_eq!(app.input, "first command");
        assert_eq!(app.history.cursor, Some(0));

        app.on_input_history_previous();
        assert_eq!(app.input, "first command");
        assert_eq!(app.history.cursor, Some(0));
    }

    #[test]
    fn input_history_down_returns_blank_after_newest() {
        let mut app = App::new();
        app.history.entries.push("one".to_string());
        app.history.entries.push("two".to_string());
        app.on_input_history_previous();
        assert_eq!(app.input, "two");
        assert_eq!(app.history.cursor, Some(1));

        app.on_input_history_next();
        assert_eq!(app.input, "");
        assert_eq!(app.history.cursor, None);
    }

    #[test]
    fn input_history_down_returns_live_draft_after_newest() {
        let mut app = App::new();
        app.input = "editing draft".to_string();
        app.history.entries.push("foo".to_string());
        app.history.entries.push("bar".to_string());

        app.on_input_history_previous();
        assert_eq!(app.input, "bar");

        app.on_input_history_next();
        assert_eq!(app.input, "editing draft");
        assert_eq!(app.history.cursor, None);
        assert!(app.history.draft.is_none());
    }

    #[test]
    fn input_history_previous_no_entries_is_noop() {
        let mut app = App::new();
        app.input = "draft text".to_string();

        app.on_input_history_previous();
        assert_eq!(app.input, "draft text");
        assert_eq!(app.history.cursor, None);
        assert_eq!(app.history.draft, None);
    }

    #[test]
    fn input_history_next_without_active_cursor_is_noop() {
        let mut app = App::new();
        app.input = "draft text".to_string();
        app.history.draft = Some("stale draft".to_string());

        app.on_input_history_next();
        assert_eq!(app.input, "draft text");
        assert_eq!(app.history.cursor, None);
        assert_eq!(app.history.draft, Some("stale draft".to_string()));
    }

    #[test]
    fn input_history_previous_sanitizes_stale_cursor() {
        let mut app = App::new();
        app.history.entries.push("only".to_string());
        app.history.cursor = Some(10);

        app.on_input_history_previous();

        assert_eq!(app.input, "only");
        assert_eq!(app.history.cursor, Some(0));
    }

    #[test]
    fn input_history_next_sanitizes_stale_cursor() {
        let mut app = App::new();
        app.history.entries.push("only".to_string());
        app.history.cursor = Some(10);

        app.on_input_history_next();

        assert_eq!(app.input, "");
        assert_eq!(app.history.cursor, None);
        assert!(app.history.draft.is_none());
    }

    #[test]
    fn input_change_resets_history_cursor() {
        let mut app = App::new();
        app.history.cursor = Some(0);
        app.history.draft = Some("stale draft".to_string());
        app.on_input_replace("hello".to_string());

        assert_eq!(app.history.cursor, None);
        assert_eq!(app.history.draft, None);
        assert_eq!(app.input, "hello");
    }

    #[test]
    fn run_chunk_during_cancelling_appends_without_restoring_streaming() {
        let mut app = App::new();
        let run_id = 5;
        app.mode = Mode::Idle;
        app.cancelling_run = Some(run_id);
        app.transcript
            .push(assistant_message("first", false, Some(run_id)));

        app.on_run_chunk(run_id, " second");

        let assistant_messages: Vec<_> = app
            .transcript
            .iter()
            .filter(|message| message.role == Role::Assistant && message.run_id == Some(run_id))
            .collect();
        assert_eq!(assistant_messages.len(), 1);
        assert_eq!(assistant_messages[0].content, "first second");
        assert!(!assistant_messages[0].streaming);
    }

    #[test]
    fn submit_failure_keeps_user_turn_in_model_facing_history() {
        struct FailingHost;

        impl HostOps for FailingHost {
            fn start_run(
                &mut self,
                _messages: Vec<RunMessage>,
                _instructions: String,
            ) -> Result<RunId, String> {
                Err("transport unavailable".to_string())
            }

            fn cancel_run(&mut self, _run_id: RunId) {}

            fn request_render(&mut self) {}

            fn request_stop(&mut self) {}
        }

        let mut app = App::new();
        let mut host = FailingHost;

        app.on_input_replace("retry this".to_string());
        app.on_submit(&mut host);

        assert_eq!(app.mode, Mode::Error("transport unavailable".to_string()));
        assert_eq!(
            app.conversation_messages(),
            &[RunMessage::UserText {
                text: "retry this".to_string(),
            }]
        );
    }

    #[test]
    fn failed_run_does_not_persist_assistant_or_tool_messages_in_model_history() {
        let mut app = App::new();
        let run_id = 17;
        app.mode = Mode::Running { run_id };

        app.on_run_started(run_id);
        app.on_run_chunk(run_id, "partial");
        app.on_tool_call_started(
            run_id,
            "call-fail",
            "read",
            &serde_json::json!({ "path": "README.md" }),
        );
        app.on_tool_call_finished(
            run_id,
            "read",
            "call-fail",
            true,
            &serde_json::json!("missing file"),
            "missing file",
        );
        app.on_run_failed(run_id, "boom");

        assert!(app.conversation_messages().iter().all(|message| {
            !matches!(
                message,
                RunMessage::AssistantText { .. }
                    | RunMessage::ToolCall { .. }
                    | RunMessage::ToolResult { .. }
            )
        }));
    }

    #[test]
    fn cancelled_run_does_not_persist_assistant_or_tool_messages_in_model_history() {
        let mut app = App::new();
        let run_id = 23;
        app.mode = Mode::Running { run_id };

        app.on_run_started(run_id);
        app.on_run_chunk(run_id, "partial");
        app.mode = Mode::Idle;
        app.cancelling_run = Some(run_id);
        app.on_tool_call_started(
            run_id,
            "call-cancel",
            "bash",
            &serde_json::json!({ "command": "pwd" }),
        );
        app.on_tool_call_finished(
            run_id,
            "bash",
            "call-cancel",
            true,
            &serde_json::json!("cancelled"),
            "cancelled",
        );
        app.on_run_cancelled(run_id);

        assert!(app.conversation_messages().iter().all(|message| {
            !matches!(
                message,
                RunMessage::AssistantText { .. }
                    | RunMessage::ToolCall { .. }
                    | RunMessage::ToolResult { .. }
            )
        }));
    }
}
