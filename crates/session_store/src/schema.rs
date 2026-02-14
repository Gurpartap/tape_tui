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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JsonLine {
    Session(SessionHeader),
    Entry(SessionEntry),
}

impl<'de> Deserialize<'de> for JsonLine {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawJsonLine::deserialize(deserializer)?;
        Ok(match raw {
            RawJsonLine::Session(raw_header) => JsonLine::Session(raw_header.into()),
            RawJsonLine::UserTextEntry(raw_entry) => JsonLine::Entry(raw_entry.into()),
            RawJsonLine::AssistantTextEntry(raw_entry) => JsonLine::Entry(raw_entry.into()),
            RawJsonLine::ToolCallEntry(raw_entry) => JsonLine::Entry(raw_entry.into()),
            RawJsonLine::ToolResultEntry(raw_entry) => JsonLine::Entry(raw_entry.into()),
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawJsonLine {
    Session(RawSessionHeader),
    UserTextEntry(RawUserTextEntry),
    AssistantTextEntry(RawAssistantTextEntry),
    ToolCallEntry(RawToolCallEntry),
    ToolResultEntry(RawToolResultEntry),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSessionHeader {
    #[serde(rename = "type")]
    record_type: SessionRecordType,
    version: u32,
    session_id: String,
    created_at: String,
    cwd: String,
}

impl From<RawSessionHeader> for SessionHeader {
    fn from(raw: RawSessionHeader) -> Self {
        Self {
            record_type: raw.record_type,
            version: raw.version,
            session_id: raw.session_id,
            created_at: raw.created_at,
            cwd: raw.cwd,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawUserTextEntry {
    #[serde(rename = "type")]
    record_type: EntryRecordType,
    id: String,
    parent_id: Option<String>,
    ts: String,
    kind: RawUserTextKind,
    text: String,
}

#[derive(Debug, Deserialize)]
enum RawUserTextKind {
    #[serde(rename = "user_text")]
    UserText,
}

impl From<RawUserTextEntry> for SessionEntry {
    fn from(raw: RawUserTextEntry) -> Self {
        let RawUserTextEntry {
            record_type,
            id,
            parent_id,
            ts,
            kind: _kind,
            text,
        } = raw;

        Self {
            record_type,
            id,
            parent_id,
            ts,
            kind: SessionEntryKind::UserText { text },
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAssistantTextEntry {
    #[serde(rename = "type")]
    record_type: EntryRecordType,
    id: String,
    parent_id: Option<String>,
    ts: String,
    kind: RawAssistantTextKind,
    text: String,
}

#[derive(Debug, Deserialize)]
enum RawAssistantTextKind {
    #[serde(rename = "assistant_text")]
    AssistantText,
}

impl From<RawAssistantTextEntry> for SessionEntry {
    fn from(raw: RawAssistantTextEntry) -> Self {
        let RawAssistantTextEntry {
            record_type,
            id,
            parent_id,
            ts,
            kind: _kind,
            text,
        } = raw;

        Self {
            record_type,
            id,
            parent_id,
            ts,
            kind: SessionEntryKind::AssistantText { text },
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawToolCallEntry {
    #[serde(rename = "type")]
    record_type: EntryRecordType,
    id: String,
    parent_id: Option<String>,
    ts: String,
    kind: RawToolCallKind,
    call_id: String,
    tool_name: String,
    arguments: Value,
}

#[derive(Debug, Deserialize)]
enum RawToolCallKind {
    #[serde(rename = "tool_call")]
    ToolCall,
}

impl From<RawToolCallEntry> for SessionEntry {
    fn from(raw: RawToolCallEntry) -> Self {
        let RawToolCallEntry {
            record_type,
            id,
            parent_id,
            ts,
            kind: _kind,
            call_id,
            tool_name,
            arguments,
        } = raw;

        Self {
            record_type,
            id,
            parent_id,
            ts,
            kind: SessionEntryKind::ToolCall {
                call_id,
                tool_name,
                arguments,
            },
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawToolResultEntry {
    #[serde(rename = "type")]
    record_type: EntryRecordType,
    id: String,
    parent_id: Option<String>,
    ts: String,
    kind: RawToolResultKind,
    call_id: String,
    tool_name: String,
    content: Value,
    is_error: bool,
}

#[derive(Debug, Deserialize)]
enum RawToolResultKind {
    #[serde(rename = "tool_result")]
    ToolResult,
}

impl From<RawToolResultEntry> for SessionEntry {
    fn from(raw: RawToolResultEntry) -> Self {
        let RawToolResultEntry {
            record_type,
            id,
            parent_id,
            ts,
            kind: _kind,
            call_id,
            tool_name,
            content,
            is_error,
        } = raw;

        Self {
            record_type,
            id,
            parent_id,
            ts,
            kind: SessionEntryKind::ToolResult {
                call_id,
                tool_name,
                content,
                is_error,
            },
        }
    }
}
