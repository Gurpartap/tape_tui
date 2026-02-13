use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

use apply_patch_engine::{
    maybe_parse_apply_patch_verified, ApplyPatchError, ApplyPatchFileChange,
    MaybeApplyPatchVerified,
};
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
    ApplyPatch {
        input: String,
    },
}

#[derive(Debug, Clone)]
enum PatchMutation {
    Add {
        path: PathBuf,
        content: String,
    },
    Delete {
        path: PathBuf,
    },
    Update {
        source_path: PathBuf,
        destination_path: PathBuf,
        content: String,
    },
}

impl PatchMutation {
    fn sort_key(&self) -> String {
        match self {
            Self::Add { path, .. } | Self::Delete { path } => path.to_string_lossy().to_string(),
            Self::Update { source_path, .. } => source_path.to_string_lossy().to_string(),
        }
    }
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
                return ToolOutput::fail(format!(
                    "Failed to read file {}: {error}",
                    resolved.display()
                ));
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
                return ToolOutput::fail(format!(
                    "Failed to read file {}: {error}",
                    resolved.display()
                ));
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
            return ToolOutput::fail(format!(
                "Failed to write file {}: {error}",
                resolved.display()
            ));
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
            return ToolOutput::fail(format!(
                "Failed to write file {}: {error}",
                resolved.display()
            ));
        }

        ToolOutput::ok(format!("Wrote {}", resolved.display()))
    }

    fn execute_apply_patch(&self, input: String) -> ToolOutput {
        if input.trim().is_empty() {
            return ToolOutput::fail("apply_patch requires non-empty input".to_string());
        }

        let argv = vec!["apply_patch".to_string(), input];
        let action = match maybe_parse_apply_patch_verified(&argv, &self.workspace_root) {
            MaybeApplyPatchVerified::Body(action) => action,
            MaybeApplyPatchVerified::CorrectnessError(error) => {
                return ToolOutput::fail(self.map_apply_patch_error(error));
            }
            MaybeApplyPatchVerified::ShellParseError(error) => {
                return ToolOutput::fail(format!(
                    "apply_patch parse error: failed to parse invocation shell: {error:?}"
                ));
            }
            MaybeApplyPatchVerified::NotApplyPatch => {
                return ToolOutput::fail(
                    "apply_patch parse error: input did not contain a valid apply_patch command"
                        .to_string(),
                );
            }
        };

        if action.is_empty() {
            return ToolOutput::fail("apply_patch produced no file changes".to_string());
        }

        let mut mutations = Vec::new();
        for (path, change) in action.changes() {
            let mutation = match change {
                ApplyPatchFileChange::Add { content } => {
                    let resolved = match self.resolve_patch_write_target(path) {
                        Ok(path) => path,
                        Err(error) => {
                            return ToolOutput::fail(self.map_apply_patch_path_error(error));
                        }
                    };

                    PatchMutation::Add {
                        path: resolved,
                        content: content.clone(),
                    }
                }
                ApplyPatchFileChange::Delete { .. } => {
                    let resolved = match self.resolve_patch_existing_path(path) {
                        Ok(path) => path,
                        Err(error) => {
                            return ToolOutput::fail(self.map_apply_patch_path_error(error));
                        }
                    };

                    PatchMutation::Delete { path: resolved }
                }
                ApplyPatchFileChange::Update {
                    move_path,
                    new_content,
                    ..
                } => {
                    let source_path = match self.resolve_patch_existing_path(path) {
                        Ok(path) => path,
                        Err(error) => {
                            return ToolOutput::fail(self.map_apply_patch_path_error(error));
                        }
                    };

                    let destination_path = match move_path {
                        Some(move_path) => match self.resolve_patch_write_target(move_path) {
                            Ok(path) => path,
                            Err(error) => {
                                return ToolOutput::fail(self.map_apply_patch_path_error(error));
                            }
                        },
                        None => source_path.clone(),
                    };

                    PatchMutation::Update {
                        source_path,
                        destination_path,
                        content: new_content.clone(),
                    }
                }
            };

            mutations.push(mutation);
        }

        mutations.sort_by_key(PatchMutation::sort_key);

        let mut added = Vec::new();
        let mut modified = Vec::new();
        let mut deleted = Vec::new();

        for mutation in mutations {
            match mutation {
                PatchMutation::Add { path, content } => {
                    if let Some(parent) = path.parent() {
                        if let Err(error) = fs::create_dir_all(parent) {
                            return ToolOutput::fail(format!(
                                "apply_patch io failure while creating parent directory {}: {error}",
                                parent.display()
                            ));
                        }
                    }

                    if let Err(error) = fs::write(&path, content) {
                        return ToolOutput::fail(format!(
                            "apply_patch io failure while writing {}: {error}",
                            path.display()
                        ));
                    }

                    added.push(path);
                }
                PatchMutation::Delete { path } => {
                    if let Err(error) = fs::remove_file(&path) {
                        return ToolOutput::fail(format!(
                            "apply_patch io failure while deleting {}: {error}",
                            path.display()
                        ));
                    }

                    deleted.push(path);
                }
                PatchMutation::Update {
                    source_path,
                    destination_path,
                    content,
                } => {
                    if let Some(parent) = destination_path.parent() {
                        if let Err(error) = fs::create_dir_all(parent) {
                            return ToolOutput::fail(format!(
                                "apply_patch io failure while creating parent directory {}: {error}",
                                parent.display()
                            ));
                        }
                    }

                    if let Err(error) = fs::write(&destination_path, content) {
                        return ToolOutput::fail(format!(
                            "apply_patch io failure while writing {}: {error}",
                            destination_path.display()
                        ));
                    }

                    if source_path != destination_path {
                        if let Err(error) = fs::remove_file(&source_path) {
                            return ToolOutput::fail(format!(
                                "apply_patch io failure while removing source {}: {error}",
                                source_path.display()
                            ));
                        }
                    }

                    modified.push(destination_path);
                }
            }
        }

        ToolOutput::ok(self.format_apply_patch_summary(&added, &modified, &deleted))
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

    fn resolve_patch_existing_path(&self, path: &Path) -> Result<PathBuf, String> {
        self.resolve_existing_path(path.to_string_lossy().as_ref())
    }

    fn resolve_patch_write_target(&self, path: &Path) -> Result<PathBuf, String> {
        if path.exists() {
            let canonical = path
                .canonicalize()
                .map_err(|error| format!("Failed to resolve path {}: {error}", path.display()))?;
            self.ensure_inside_workspace(&canonical)?;
            Ok(path.to_path_buf())
        } else {
            self.resolve_write_path(path.to_string_lossy().as_ref())
        }
    }

    fn map_apply_patch_error(&self, error: ApplyPatchError) -> String {
        match error {
            ApplyPatchError::ParseError(error) => format!("apply_patch parse error: {error}"),
            ApplyPatchError::ComputeReplacements(error) => {
                format!("apply_patch context mismatch: {error}")
            }
            ApplyPatchError::IoError(error) => format!("apply_patch io failure: {error}"),
            ApplyPatchError::ImplicitInvocation => {
                "apply_patch invocation error: patch must be passed as the explicit apply_patch input"
                    .to_string()
            }
        }
    }

    fn map_apply_patch_path_error(&self, error: String) -> String {
        if error.contains("Path escapes workspace root") {
            format!("apply_patch path escape rejected: {error}")
        } else {
            format!("apply_patch path validation failed: {error}")
        }
    }

    fn format_apply_patch_summary(
        &self,
        added: &[PathBuf],
        modified: &[PathBuf],
        deleted: &[PathBuf],
    ) -> String {
        let mut summary_lines = vec!["Success. Updated the following files:".to_string()];

        let mut added = added.to_vec();
        let mut modified = modified.to_vec();
        let mut deleted = deleted.to_vec();

        added.sort();
        modified.sort();
        deleted.sort();

        for path in added {
            summary_lines.push(format!("A {}", self.workspace_relative_display(&path)));
        }

        for path in modified {
            summary_lines.push(format!("M {}", self.workspace_relative_display(&path)));
        }

        for path in deleted {
            summary_lines.push(format!("D {}", self.workspace_relative_display(&path)));
        }

        summary_lines.join("\n")
    }

    fn workspace_relative_display(&self, path: &Path) -> String {
        path.strip_prefix(&self.workspace_root)
            .map(|relative| relative.display().to_string())
            .unwrap_or_else(|_| path.display().to_string())
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
            ToolCall::ApplyPatch { input } => self.execute_apply_patch(input),
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
            return ancestor.canonicalize().map_err(|error| {
                format!("Failed to resolve path {}: {error}", ancestor.display())
            });
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
