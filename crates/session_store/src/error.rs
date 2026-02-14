use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SessionStoreError {
    #[error("I/O error while {operation} at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("I/O error while reading line {line} in {path}: {source}")]
    IoLine {
        path: PathBuf,
        line: usize,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse JSON at {path}:{line}: {source}")]
    JsonLineParse {
        path: PathBuf,
        line: usize,
        #[source]
        source: serde_json::Error,
    },

    #[error("missing session header line in {path}")]
    MissingHeader { path: PathBuf },

    #[error("line {line} in {path} must be a session header record")]
    InvalidHeaderRecord { path: PathBuf, line: usize },

    #[error("line {line} in {path} has unsupported session version {found}; expected 1")]
    UnsupportedVersion {
        path: PathBuf,
        line: usize,
        found: u32,
    },

    #[error("line {line} in {path} contains a duplicate entry id '{id}'")]
    DuplicateEntryId {
        path: PathBuf,
        line: usize,
        id: String,
    },

    #[error(
        "line {line} in {path} contains dangling parent id '{parent_id}' for entry '{entry_id}'"
    )]
    DanglingParentId {
        path: PathBuf,
        line: usize,
        entry_id: String,
        parent_id: String,
    },

    #[error("line {line} in {path} must be an entry record")]
    InvalidEntryRecord { path: PathBuf, line: usize },

    #[error("line {line} in {path} has invalid RFC3339 timestamp in field '{field}': {value}")]
    InvalidTimestamp {
        path: PathBuf,
        line: usize,
        field: &'static str,
        value: String,
    },

    #[error("line {line} in {path} has non-absolute cwd path: {cwd}")]
    NonAbsoluteCwd {
        path: PathBuf,
        line: usize,
        cwd: String,
    },

    #[error("path provided to create_new must resolve to an absolute cwd: {path}")]
    NonAbsoluteCreateCwd { path: PathBuf },

    #[error("no session files found under {root}")]
    NoSessionsFound { root: PathBuf },

    #[error("cannot replay unknown leaf id '{leaf_id}' in {path}")]
    UnknownLeafId { path: PathBuf, leaf_id: String },

    #[error("cycle detected while replaying from leaf '{leaf_id}' in {path}")]
    ReplayCycle { path: PathBuf, leaf_id: String },

    #[error("failed to serialize session line for {path}: {source}")]
    JsonSerialize {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("failed to format current UTC timestamp as RFC3339: {0}")]
    ClockFormat(#[source] time::error::Format),
}

impl SessionStoreError {
    #[must_use]
    pub fn io(operation: &'static str, path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            operation,
            path: path.into(),
            source,
        }
    }

    #[must_use]
    pub fn io_line(path: impl Into<PathBuf>, line: usize, source: std::io::Error) -> Self {
        Self::IoLine {
            path: path.into(),
            line,
            source,
        }
    }

    #[must_use]
    pub fn json_line(path: impl Into<PathBuf>, line: usize, source: serde_json::Error) -> Self {
        Self::JsonLineParse {
            path: path.into(),
            line,
            source,
        }
    }

    #[must_use]
    pub fn json_serialize(path: impl Into<PathBuf>, source: serde_json::Error) -> Self {
        Self::JsonSerialize {
            path: path.into(),
            source,
        }
    }
}
