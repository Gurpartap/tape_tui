use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::SessionStoreError;
use crate::paths::{session_file_name, session_root};
use crate::schema::{JsonLine, SessionEntry, SessionHeader};

pub struct SessionStore {
    pub(crate) path: PathBuf,
    pub(crate) file: File,
    pub(crate) header: SessionHeader,
    pub(crate) entries: Vec<SessionEntry>,
    pub(crate) index_by_id: HashMap<String, usize>,
    pub(crate) current_leaf_id: Option<String>,
}

impl SessionStore {
    pub fn create_new(cwd: &Path) -> Result<Self, SessionStoreError> {
        let cwd = resolve_absolute_cwd(cwd)?;
        let root = session_root(&cwd);
        fs::create_dir_all(&root).map_err(|source| {
            SessionStoreError::io("creating session root directory", &root, source)
        })?;

        let created_at = format_now_rfc3339()?;
        let session_id = Uuid::new_v4().to_string();
        let file_name = session_file_name(&created_at, &session_id);
        let path = root.join(file_name);

        let header = SessionHeader::v1(session_id, created_at, cwd.display().to_string());
        validate_header_line(&path, 1, &header)?;

        let mut file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&path)
            .map_err(|source| SessionStoreError::io("creating session file", &path, source))?;

        let header_json = serde_json::to_string(&header)
            .map_err(|source| SessionStoreError::json_serialize(&path, source))?;

        file.write_all(header_json.as_bytes())
            .map_err(|source| SessionStoreError::io("writing session header", &path, source))?;
        file.write_all(b"\n").map_err(|source| {
            SessionStoreError::io("writing session header newline", &path, source)
        })?;
        file.sync_data()
            .map_err(|source| SessionStoreError::io("syncing session header", &path, source))?;

        Ok(Self {
            path,
            file,
            header,
            entries: Vec::new(),
            index_by_id: HashMap::new(),
            current_leaf_id: None,
        })
    }

    pub fn open(path: &Path) -> Result<Self, SessionStoreError> {
        let path = path.to_path_buf();
        let read_file = File::open(&path)
            .map_err(|source| SessionStoreError::io("opening session file", &path, source))?;
        let reader = BufReader::new(read_file);

        let mut header: Option<SessionHeader> = None;
        let mut entries_with_lines: Vec<(usize, SessionEntry)> = Vec::new();
        let mut index_by_id = HashMap::new();

        for (line_index, line_result) in reader.lines().enumerate() {
            let line_number = line_index + 1;
            let line = line_result
                .map_err(|source| SessionStoreError::io_line(&path, line_number, source))?;
            let parsed = parse_json_line(&path, line_number, &line)?;

            if line_number == 1 {
                match parsed {
                    JsonLine::Session(parsed_header) => {
                        validate_header_line(&path, line_number, &parsed_header)?;
                        header = Some(parsed_header);
                    }
                    JsonLine::Entry(_) => {
                        return Err(SessionStoreError::InvalidHeaderRecord {
                            path,
                            line: line_number,
                        });
                    }
                }

                continue;
            }

            match parsed {
                JsonLine::Session(_) => {
                    return Err(SessionStoreError::InvalidEntryRecord {
                        path,
                        line: line_number,
                    });
                }
                JsonLine::Entry(entry) => {
                    validate_entry_line(&path, line_number, &entry)?;
                    if index_by_id.contains_key(&entry.id) {
                        return Err(SessionStoreError::DuplicateEntryId {
                            path,
                            line: line_number,
                            id: entry.id,
                        });
                    }

                    let next_index = entries_with_lines.len();
                    index_by_id.insert(entry.id.clone(), next_index);
                    entries_with_lines.push((line_number, entry));
                }
            }
        }

        let header =
            header.ok_or_else(|| SessionStoreError::MissingHeader { path: path.clone() })?;
        validate_entry_graph(&path, &entries_with_lines, &index_by_id)?;

        let entries = entries_with_lines
            .into_iter()
            .map(|(_, entry)| entry)
            .collect::<Vec<_>>();
        let current_leaf_id = entries.last().map(|entry| entry.id.clone());

        let file = OpenOptions::new()
            .append(true)
            .open(&path)
            .map_err(|source| {
                SessionStoreError::io("opening session file for append", &path, source)
            })?;

        Ok(Self {
            path,
            file,
            header,
            entries,
            index_by_id,
            current_leaf_id,
        })
    }

    pub fn latest_session_path(cwd: &Path) -> Result<PathBuf, SessionStoreError> {
        let cwd = resolve_absolute_cwd(cwd)?;
        let root = session_root(&cwd);
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Err(SessionStoreError::NoSessionsFound { root });
            }
            Err(source) => {
                return Err(SessionStoreError::io(
                    "listing session root directory",
                    &root,
                    source,
                ));
            }
        };

        let mut latest: Option<(std::time::SystemTime, PathBuf)> = None;

        for entry in entries {
            let entry = entry.map_err(|source| {
                SessionStoreError::io("reading session root entry", &root, source)
            })?;
            let path = entry.path();

            let file_type = entry.file_type().map_err(|source| {
                SessionStoreError::io("reading session file type", &path, source)
            })?;
            if !file_type.is_file() {
                continue;
            }

            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }

            let metadata = entry.metadata().map_err(|source| {
                SessionStoreError::io("reading session file metadata", &path, source)
            })?;
            let modified = metadata.modified().map_err(|source| {
                SessionStoreError::io("reading session file modified timestamp", &path, source)
            })?;

            match &latest {
                Some((latest_modified, latest_path))
                    if modified < *latest_modified
                        || (modified == *latest_modified && path <= *latest_path) => {}
                _ => latest = Some((modified, path)),
            }
        }

        latest
            .map(|(_, path)| path)
            .ok_or(SessionStoreError::NoSessionsFound { root })
    }

    pub fn append(&mut self, entry: SessionEntry) -> Result<(), SessionStoreError> {
        let line_number = self.entries.len() + 2;
        validate_entry_line(&self.path, line_number, &entry)?;

        if self.index_by_id.contains_key(&entry.id) {
            return Err(SessionStoreError::DuplicateEntryId {
                path: self.path.clone(),
                line: line_number,
                id: entry.id,
            });
        }

        if let Some(parent_id) = &entry.parent_id {
            if !self.index_by_id.contains_key(parent_id) {
                return Err(SessionStoreError::DanglingParentId {
                    path: self.path.clone(),
                    line: line_number,
                    entry_id: entry.id,
                    parent_id: parent_id.clone(),
                });
            }
        }

        let entry_id = entry.id.clone();
        let entry_json = serde_json::to_string(&entry)
            .map_err(|source| SessionStoreError::json_serialize(&self.path, source))?;

        self.file
            .write_all(entry_json.as_bytes())
            .map_err(|source| SessionStoreError::io("writing session entry", &self.path, source))?;
        self.file.write_all(b"\n").map_err(|source| {
            SessionStoreError::io("writing session entry newline", &self.path, source)
        })?;
        self.file
            .sync_data()
            .map_err(|source| SessionStoreError::io("syncing session entry", &self.path, source))?;

        let next_index = self.entries.len();
        self.entries.push(entry);
        self.index_by_id.insert(entry_id.clone(), next_index);
        self.current_leaf_id = Some(entry_id);

        Ok(())
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn header(&self) -> &SessionHeader {
        &self.header
    }

    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.header.session_id
    }

    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn current_leaf_id(&self) -> Option<&str> {
        self.current_leaf_id.as_deref()
    }
}

pub(crate) fn parse_json_line(
    path: &Path,
    line_number: usize,
    line: &str,
) -> Result<JsonLine, SessionStoreError> {
    serde_json::from_str::<JsonLine>(line)
        .map_err(|source| SessionStoreError::json_line(path, line_number, source))
}

pub(crate) fn validate_header_line(
    path: &Path,
    line_number: usize,
    header: &SessionHeader,
) -> Result<(), SessionStoreError> {
    if header.version != 1 {
        return Err(SessionStoreError::UnsupportedVersion {
            path: path.to_path_buf(),
            line: line_number,
            found: header.version,
        });
    }

    validate_rfc3339(path, line_number, "created_at", &header.created_at)?;

    if !Path::new(&header.cwd).is_absolute() {
        return Err(SessionStoreError::NonAbsoluteCwd {
            path: path.to_path_buf(),
            line: line_number,
            cwd: header.cwd.clone(),
        });
    }

    Ok(())
}

pub(crate) fn validate_entry_line(
    path: &Path,
    line_number: usize,
    entry: &SessionEntry,
) -> Result<(), SessionStoreError> {
    validate_rfc3339(path, line_number, "ts", &entry.ts)
}

pub(crate) fn validate_entry_graph(
    path: &Path,
    entries_with_lines: &[(usize, SessionEntry)],
    index_by_id: &HashMap<String, usize>,
) -> Result<(), SessionStoreError> {
    for (line_number, entry) in entries_with_lines {
        if let Some(parent_id) = &entry.parent_id {
            if !index_by_id.contains_key(parent_id) {
                return Err(SessionStoreError::DanglingParentId {
                    path: path.to_path_buf(),
                    line: *line_number,
                    entry_id: entry.id.clone(),
                    parent_id: parent_id.clone(),
                });
            }
        }
    }

    Ok(())
}

pub(crate) fn validate_rfc3339(
    path: &Path,
    line_number: usize,
    field: &'static str,
    value: &str,
) -> Result<(), SessionStoreError> {
    if OffsetDateTime::parse(value, &Rfc3339).is_err() {
        return Err(SessionStoreError::InvalidTimestamp {
            path: path.to_path_buf(),
            line: line_number,
            field,
            value: value.to_string(),
        });
    }

    Ok(())
}

fn resolve_absolute_cwd(cwd: &Path) -> Result<PathBuf, SessionStoreError> {
    let absolute_cwd = if cwd.is_absolute() {
        cwd.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|source| SessionStoreError::io("resolving current directory", cwd, source))?
            .join(cwd)
    };

    if !absolute_cwd.is_absolute() {
        return Err(SessionStoreError::NonAbsoluteCreateCwd { path: absolute_cwd });
    }

    std::fs::metadata(&absolute_cwd)
        .map_err(|source| SessionStoreError::io("resolving cwd metadata", &absolute_cwd, source))?;

    Ok(absolute_cwd)
}

fn format_now_rfc3339() -> Result<String, SessionStoreError> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(SessionStoreError::ClockFormat)
}
