use crate::commands::{parse_slash_command, SlashCommand};

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
pub struct InputHistory {
    pub entries: Vec<String>,
    pub cursor: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    pub mode: Mode,
    pub input: String,
    pub transcript: Vec<Message>,
    pub history: InputHistory,
    pub should_exit: bool,
    cancelling_run: Option<RunId>,
}

pub trait HostOps {
    fn start_run(&mut self, prompt: String) -> Result<RunId, String>;
    fn cancel_run(&mut self, run_id: RunId);
    fn request_render(&mut self);
    fn request_stop(&mut self);
}

const HELP_TEXT: &str = "Commands: /help, /clear, /cancel, /quit";
const ERROR_RUN_ALREADY_ACTIVE: &str = "Run already active";

impl App {
    pub fn new() -> Self {
        Self {
            mode: Mode::Idle,
            input: String::new(),
            transcript: Vec::new(),
            history: InputHistory::default(),
            should_exit: false,
            cancelling_run: None,
        }
    }

    pub fn on_input_replace(&mut self, text: String) {
        self.input = text;
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

        self.history.entries.push(prompt.clone());
        self.history.cursor = None;
        self.transcript.push(Message {
            role: Role::User,
            content: prompt.clone(),
            streaming: false,
            run_id: None,
        });

        match host.start_run(prompt) {
            Ok(run_id) => {
                self.mode = Mode::Running { run_id };
            }
            Err(error) => {
                if error == ERROR_RUN_ALREADY_ACTIVE {
                    self.push_system("Run already in progress. Use /cancel to stop it.".to_string());
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
        if self.input.trim().is_empty() {
            self.on_quit(host);
        } else {
            self.input.clear();
            host.request_render();
        }
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
        if !self.is_active_run(run_id) {
            return;
        }

        if self.is_cancelling(run_id) {
            return;
        }

        if let Some(message) = self
            .transcript
            .iter_mut()
            .rev()
            .find(|message| message.role == Role::Assistant && message.streaming && message.run_id == Some(run_id))
        {
            message.content.push_str(chunk);
        } else {
            self.transcript.push(Message {
                role: Role::Assistant,
                content: chunk.to_string(),
                streaming: true,
                run_id: Some(run_id),
            });
        }
    }

    pub fn on_run_finished(&mut self, run_id: RunId) {
        if !self.should_apply_run_event(run_id) {
            return;
        }

        if self.is_cancelling(run_id) {
            self.finalize_stream(run_id);
            self.finalize_cancelled_run(run_id);
            return;
        }

        if !self.is_active_run(run_id) {
            return;
        }

        self.finalize_stream(run_id);
        self.mode = Mode::Idle;
    }

    pub fn on_run_failed(&mut self, run_id: RunId, error: &str) {
        if !self.should_apply_run_event(run_id) {
            return;
        }

        if self.is_cancelling(run_id) {
            self.finalize_stream(run_id);
            self.finalize_cancelled_run(run_id);
            return;
        }

        if !self.is_active_run(run_id) {
            return;
        }

        self.finalize_stream(run_id);
        self.mode = Mode::Error(error.to_string());
        self.push_system(format!("Run failed: {error}"));
    }

    pub fn on_run_cancelled(&mut self, run_id: RunId) {
        if !self.should_apply_run_event(run_id) || !self.is_cancelling(run_id) {
            return;
        }

        self.finalize_stream(run_id);
        self.finalize_cancelled_run(run_id);
    }

    fn should_apply_run_event(&self, run_id: RunId) -> bool {
        !self.should_exit
            && (self.is_active_run(run_id) || self.is_cancelling(run_id))
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
    use super::*;

    fn assistant_message(content: &str, streaming: bool, run_id: Option<RunId>) -> Message {
        Message {
            role: Role::Assistant,
            content: content.to_string(),
            streaming,
            run_id,
        }
    }

    #[test]
    fn finalize_stream_merges_assistant_chunks_for_same_run() {
        let mut app = App::new();
        app.transcript.push(assistant_message("first ", true, Some(42)));
        app.transcript.push(Message {
            role: Role::User,
            content: "ignored".to_string(),
            streaming: false,
            run_id: None,
        });
        app.transcript.push(assistant_message("second", true, Some(42)));

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
        app.transcript.push(assistant_message("seed", false, Some(7)));
        app.transcript.push(assistant_message("newer", true, Some(7)));
        app.on_run_started(7);
        assert_eq!(
            app.transcript
                .iter()
                .filter(|message| message.role == Role::Assistant && message.run_id == Some(7))
                .count(),
            1
        );
    }
}
