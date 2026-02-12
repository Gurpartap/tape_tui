use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

use wait_timeout::ChildExt;

const DEFAULT_BASH_TIMEOUT_SEC: u64 = 30;
const DEFAULT_BASH_MAX_OUTPUT_BYTES: usize = 100 * 1024;
const DEFAULT_READ_MAX_BYTES: usize = 200 * 1024;

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

#[derive(Debug, Clone)]
pub struct BuiltinToolExecutor {
    workspace_root: PathBuf,
    default_bash_timeout_sec: u64,
    bash_max_output_bytes: usize,
    read_max_bytes: usize,
}

impl BuiltinToolExecutor {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Result<Self, String> {
        let workspace_root = workspace_root.into();
        let canonical_root = workspace_root
            .canonicalize()
            .map_err(|err| format!("Failed to resolve workspace root: {err}"))?;

        if !canonical_root.is_dir() {
            return Err("Workspace root must be a directory".to_string());
        }

        Ok(Self {
            workspace_root: canonical_root,
            default_bash_timeout_sec: DEFAULT_BASH_TIMEOUT_SEC,
            bash_max_output_bytes: DEFAULT_BASH_MAX_OUTPUT_BYTES,
            read_max_bytes: DEFAULT_READ_MAX_BYTES,
        })
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    fn execute_bash(
        &self,
        command: String,
        timeout_sec: Option<u64>,
        cwd: Option<String>,
    ) -> ToolOutput {
        let timeout = timeout_sec.unwrap_or(self.default_bash_timeout_sec);
        let mut command_builder = Command::new("bash");
        command_builder
            .arg("-lc")
            .arg(command)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(cwd) = cwd {
            let cwd_path = match self.resolve_existing_path(&cwd) {
                Ok(path) => path,
                Err(error) => return ToolOutput::fail(format!("Invalid bash cwd: {error}")),
            };

            if !cwd_path.is_dir() {
                return ToolOutput::fail("Invalid bash cwd: expected a directory".to_string());
            }

            command_builder.current_dir(cwd_path);
        }

        let mut child = match command_builder.spawn() {
            Ok(child) => child,
            Err(error) => {
                return ToolOutput::fail(format!("Failed to launch bash command: {error}"));
            }
        };

        let wait_result = child.wait_timeout(Duration::from_secs(timeout));

        let (timed_out, status) = match wait_result {
            Ok(Some(status)) => (false, status),
            Ok(None) => {
                let _ = child.kill();
                let status = match child.wait() {
                    Ok(status) => status,
                    Err(error) => {
                        return ToolOutput::fail(format!(
                            "Command timed out after {timeout}s and wait failed: {error}"
                        ));
                    }
                };

                (true, status)
            }
            Err(error) => {
                let _ = child.kill();
                return ToolOutput::fail(format!("Failed waiting for bash command: {error}"));
            }
        };

        let stdout = read_pipe_bytes(child.stdout.take());
        let stderr = read_pipe_bytes(child.stderr.take());

        let status_label = if timed_out {
            format!("timeout after {timeout}s")
        } else {
            format_exit_status(status)
        };

        let mut content = format!(
            "status: {status_label}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
        content = truncate_to_byte_limit(content, self.bash_max_output_bytes);

        ToolOutput {
            ok: !timed_out && status.success(),
            content,
        }
    }

    fn execute_read_file(&self, path: String) -> ToolOutput {
        let resolved = match self.resolve_existing_path(&path) {
            Ok(path) => path,
            Err(error) => return ToolOutput::fail(error),
        };

        let bytes = match fs::read(&resolved) {
            Ok(bytes) => bytes,
            Err(error) => {
                return ToolOutput::fail(format!("Failed to read file {}: {error}", resolved.display()));
            }
        };

        if bytes.len() > self.read_max_bytes {
            return ToolOutput::fail(format!(
                "File exceeds max read size ({} bytes > {} bytes)",
                bytes.len(),
                self.read_max_bytes
            ));
        }

        let content = match String::from_utf8(bytes) {
            Ok(content) => content,
            Err(_) => return ToolOutput::fail("File is not valid UTF-8 text".to_string()),
        };

        ToolOutput::ok(content)
    }

    fn execute_edit_file(&self, path: String, old_text: String, new_text: String) -> ToolOutput {
        if old_text.is_empty() {
            return ToolOutput::fail("old_text must not be empty".to_string());
        }

        let resolved = match self.resolve_existing_path(&path) {
            Ok(path) => path,
            Err(error) => return ToolOutput::fail(error),
        };

        let current_content = match fs::read_to_string(&resolved) {
            Ok(content) => content,
            Err(error) => {
                return ToolOutput::fail(format!("Failed to read file {}: {error}", resolved.display()));
            }
        };

        let occurrence_count = current_content.match_indices(&old_text).count();
        if occurrence_count != 1 {
            return ToolOutput::fail(format!(
                "edit_file requires exactly one match; found {occurrence_count}"
            ));
        }

        let updated_content = current_content.replacen(&old_text, &new_text, 1);
        if let Err(error) = fs::write(&resolved, updated_content) {
            return ToolOutput::fail(format!("Failed to write file {}: {error}", resolved.display()));
        }

        ToolOutput::ok(format!("Updated {}", resolved.display()))
    }

    fn execute_write_file(&self, path: String, content: String) -> ToolOutput {
        let resolved = match self.resolve_write_path(&path) {
            Ok(path) => path,
            Err(error) => return ToolOutput::fail(error),
        };

        if let Some(parent) = resolved.parent() {
            if let Err(error) = fs::create_dir_all(parent) {
                return ToolOutput::fail(format!(
                    "Failed to create parent directories {}: {error}",
                    parent.display()
                ));
            }

            let canonical_parent = match parent.canonicalize() {
                Ok(path) => path,
                Err(error) => {
                    return ToolOutput::fail(format!(
                        "Failed to resolve write parent {}: {error}",
                        parent.display()
                    ));
                }
            };

            if let Err(error) = self.ensure_inside_workspace(&canonical_parent) {
                return ToolOutput::fail(error);
            }
        }

        if let Err(error) = fs::write(&resolved, content) {
            return ToolOutput::fail(format!("Failed to write file {}: {error}", resolved.display()));
        }

        ToolOutput::ok(format!("Wrote {}", resolved.display()))
    }

    fn resolve_existing_path(&self, path: &str) -> Result<PathBuf, String> {
        if path.trim().is_empty() {
            return Err("Path must not be empty".to_string());
        }

        let candidate = self.absolute_candidate(path);
        let canonical = candidate
            .canonicalize()
            .map_err(|error| format!("Failed to resolve path {}: {error}", candidate.display()))?;

        self.ensure_inside_workspace(&canonical)?;
        Ok(canonical)
    }

    fn resolve_write_path(&self, path: &str) -> Result<PathBuf, String> {
        if path.trim().is_empty() {
            return Err("Path must not be empty".to_string());
        }

        let candidate = self.absolute_candidate(path);
        let parent = candidate.parent().ok_or_else(|| {
            format!(
                "Path {} has no parent directory and cannot be written safely",
                candidate.display()
            )
        })?;

        let anchor = canonicalize_existing_ancestor(parent)?;
        self.ensure_inside_workspace(&anchor)?;

        Ok(candidate)
    }

    fn absolute_candidate(&self, path: &str) -> PathBuf {
        let path = Path::new(path);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.workspace_root.join(path)
        }
    }

    fn ensure_inside_workspace(&self, canonical_path: &Path) -> Result<(), String> {
        if canonical_path.starts_with(&self.workspace_root) {
            Ok(())
        } else {
            Err(format!(
                "Path escapes workspace root: {}",
                canonical_path.display()
            ))
        }
    }
}

impl ToolExecutor for BuiltinToolExecutor {
    fn execute(&mut self, call: ToolCall) -> ToolOutput {
        match call {
            ToolCall::Bash {
                command,
                timeout_sec,
                cwd,
            } => self.execute_bash(command, timeout_sec, cwd),
            ToolCall::ReadFile { path } => self.execute_read_file(path),
            ToolCall::EditFile {
                path,
                old_text,
                new_text,
            } => self.execute_edit_file(path, old_text, new_text),
            ToolCall::WriteFile { path, content } => self.execute_write_file(path, content),
        }
    }
}

fn read_pipe_bytes(pipe: Option<impl Read>) -> Vec<u8> {
    let Some(mut pipe) = pipe else {
        return Vec::new();
    };

    let mut bytes = Vec::new();
    let _ = pipe.read_to_end(&mut bytes);
    bytes
}

fn truncate_to_byte_limit(content: String, max_bytes: usize) -> String {
    if content.len() <= max_bytes {
        return content;
    }

    let mut cutoff = max_bytes.min(content.len());
    while cutoff > 0 && !content.is_char_boundary(cutoff) {
        cutoff -= 1;
    }

    let mut truncated = content[..cutoff].to_string();
    truncated.push_str("\n[truncated]");
    truncated
}

fn canonicalize_existing_ancestor(path: &Path) -> Result<PathBuf, String> {
    for ancestor in path.ancestors() {
        if ancestor.exists() {
            return ancestor
                .canonicalize()
                .map_err(|error| format!("Failed to resolve path {}: {error}", ancestor.display()));
        }
    }

    Err(format!(
        "No existing ancestor found for path {}",
        path.display()
    ))
}

fn format_exit_status(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit_code={code}"),
        None => "exit_code=terminated_by_signal".to_string(),
    }
}
