use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRecordType {
    Session,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryRecordType {
    Entry,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionHeader {
    #[serde(rename = "type")]
    pub record_type: SessionRecordType,
    pub version: u32,
    pub session_id: String,
    pub created_at: String,
    pub cwd: String,
}

impl SessionHeader {
    #[must_use]
    pub fn v1(
        session_id: impl Into<String>,
        created_at: impl Into<String>,
        cwd: impl Into<String>,
    ) -> Self {
        Self {
            record_type: SessionRecordType::Session,
            version: 1,
            session_id: session_id.into(),
            created_at: created_at.into(),
            cwd: cwd.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionEntry {
    #[serde(rename = "type")]
    pub record_type: EntryRecordType,
    pub id: String,
    pub parent_id: Option<String>,
    pub ts: String,
    #[serde(flatten)]
    pub kind: SessionEntryKind,
}

impl SessionEntry {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        parent_id: Option<impl Into<String>>,
        ts: impl Into<String>,
        kind: SessionEntryKind,
    ) -> Self {
        Self {
            record_type: EntryRecordType::Entry,
            id: id.into(),
            parent_id: parent_id.map(Into::into),
            ts: ts.into(),
            kind,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SessionEntryKind {
    UserText {
        text: String,
    },
    AssistantText {
        text: String,
    },
    ToolCall {
        call_id: String,
        tool_name: String,
        arguments: Value,
    },
    ToolResult {
        call_id: String,
        tool_name: String,
        content: Value,
        is_error: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum JsonLine {
    Session(SessionHeader),
    Entry(SessionEntry),
}
