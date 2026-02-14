//! Strict JSONL session storage for `coding_agent` persistent sessions v1.
//!
//! Contract highlights:
//! - header-first file shape (`type=session`, `version=1`), then append-only entries;
//! - header creation and each append are `sync_data`-durable before success;
//! - malformed lines, unknown fields/kinds, invalid graph edges, duplicate ids,
//!   unsupported versions, and invalid replay leaves are hard errors;
//! - storage root is `<cwd>/.agent/sessions/` for new sessions.
//!
//! No tolerant parsing, repair, or reset-marker semantics are included in v1.

mod error;
mod paths;
mod replay;
mod schema;
mod store;

pub use error::SessionStoreError;
pub use paths::{session_file_name, session_root};
pub use schema::{
    EntryRecordType, SessionEntry, SessionEntryKind, SessionHeader, SessionRecordType,
};
pub use store::{SessionSeed, SessionStore};
