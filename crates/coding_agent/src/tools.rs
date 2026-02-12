#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCall {
    Bash {
        command: String,
        timeout_sec: Option<u64>,
        cwd: Option<String>,
    },
    ReadFile {
        path: String,
    },
    EditFile {
        path: String,
        old_text: String,
        new_text: String,
    },
    WriteFile {
        path: String,
        content: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    pub ok: bool,
    pub content: String,
}

impl ToolOutput {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            ok: true,
            content: content.into(),
        }
    }

    pub fn fail(content: impl Into<String>) -> Self {
        Self {
            ok: false,
            content: content.into(),
        }
    }
}

pub trait ToolExecutor {
    fn execute(&mut self, call: ToolCall) -> ToolOutput;
}
