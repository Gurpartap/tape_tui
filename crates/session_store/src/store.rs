use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::error::SessionStoreError;
use crate::schema::{JsonLine, SessionEntry, SessionHeader};

pub struct SessionStore {
    pub(crate) path: PathBuf,
    #[allow(dead_code)]
    pub(crate) file: File,
    pub(crate) header: SessionHeader,
    #[allow(dead_code)]
    pub(crate) entries: Vec<SessionEntry>,
    #[allow(dead_code)]
    pub(crate) index_by_id: HashMap<String, usize>,
    pub(crate) current_leaf_id: Option<String>,
}

impl SessionStore {
    pub fn create_new(_cwd: &Path) -> Result<Self, SessionStoreError> {
        todo!("not implemented yet")
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

    pub fn append(&mut self, _entry: SessionEntry) -> Result<(), SessionStoreError> {
        todo!("not implemented yet")
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
