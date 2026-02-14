use std::path::{Path, PathBuf};

pub const SESSION_DIR: [&str; 2] = [".agent", "sessions"];

#[must_use]
pub fn session_root(cwd: &Path) -> PathBuf {
    cwd.join(SESSION_DIR[0]).join(SESSION_DIR[1])
}

#[must_use]
pub fn sanitize_timestamp_for_filename(timestamp: &str) -> String {
    timestamp
        .chars()
        .map(|c| match c {
            ':' | '/' | '\\' | ' ' => '-',
            _ => c,
        })
        .collect()
}

#[must_use]
pub fn session_file_name(created_at: &str, session_id: &str) -> String {
    format!(
        "{}_{}.jsonl",
        sanitize_timestamp_for_filename(created_at),
        session_id
    )
}
