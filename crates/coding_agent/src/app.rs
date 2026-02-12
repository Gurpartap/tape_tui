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
}

pub trait HostOps {
    fn start_run(&mut self, prompt: String) -> Result<RunId, String>;
    fn cancel_run(&mut self, run_id: RunId);
    fn request_render(&mut self);
    fn request_stop(&mut self);
}

impl App {
    pub fn new() -> Self {
        Self {
            mode: Mode::Idle,
            input: String::new(),
            transcript: Vec::new(),
            history: InputHistory::default(),
            should_exit: false,
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

        if prompt.starts_with('/') {
            let command = prompt.split_whitespace().next().unwrap_or(&prompt).to_string();
            self.push_system(format!("Unknown command: {command}"));
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
                self.mode = Mode::Error(error.clone());
                self.push_system(format!("Failed to start run: {error}"));
            }
        }

        host.request_render();
    }

    pub fn on_cancel(&mut self, host: &mut dyn HostOps) {
        if let Mode::Running { run_id } = self.mode {
            host.cancel_run(run_id);
        } else {
            self.push_system("No active run".to_string());
        }

        host.request_render();
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

        if !self
            .transcript
            .iter()
            .any(|message| message.role == Role::Assistant && message.streaming && message.run_id == Some(run_id))
        {
            self.transcript.push(Message {
                role: Role::Assistant,
                content: String::new(),
                streaming: true,
                run_id: Some(run_id),
            });
        }
    }

    pub fn on_run_chunk(&mut self, run_id: RunId, chunk: &str) {
        if !self.is_active_run(run_id) {
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
        if !self.is_active_run(run_id) {
            return;
        }

        self.finalize_stream(run_id);
        self.mode = Mode::Idle;
    }

    pub fn on_run_failed(&mut self, run_id: RunId, error: &str) {
        if !self.is_active_run(run_id) {
            return;
        }

        self.finalize_stream(run_id);
        self.mode = Mode::Error(error.to_string());
        self.push_system(format!("Run failed: {error}"));
    }

    pub fn on_run_cancelled(&mut self, run_id: RunId) {
        if !self.is_active_run(run_id) {
            return;
        }

        self.finalize_stream(run_id);
        self.mode = Mode::Idle;
        self.push_system("Run cancelled".to_string());
    }

    fn is_active_run(&self, run_id: RunId) -> bool {
        matches!(self.mode, Mode::Running { run_id: active } if active == run_id)
    }

    fn finalize_stream(&mut self, run_id: RunId) {
        if let Some(message) = self
            .transcript
            .iter_mut()
            .rev()
            .find(|message| message.role == Role::Assistant && message.run_id == Some(run_id))
        {
            message.streaming = false;
        }
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
