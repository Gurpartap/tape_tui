//! Transport-only Codex API client primitives.
//!
//! This crate owns request/response/building/parsing behavior for Codex transport
//! endpoints only. It intentionally contains no auth/login code and no runtime UI
//! coupling.
//!
//! The parity target for this transport layer is the PI/OpenCode Codex wire
//! contract described in the repository plan and specs.
//!
//! SSE normalization includes host-mediated tool-call extraction via
//! [`CodexStreamEvent::ToolCallRequested`], while preserving malformed tool
//! payloads for explicit caller-side failure handling.

pub mod client;
pub mod config;
pub mod error;
pub mod events;
pub mod headers;
pub mod payload;
pub mod retry;
pub mod sse;
pub mod url;

pub use client::CodexApiClient;
pub use client::StreamResult;
pub use config::CodexApiConfig;
pub use error::CodexApiError;
pub use events::{CodexResponseStatus, CodexStreamEvent};
pub use payload::CodexRequest;
pub use sse::SseStreamParser;
pub use url::normalize_codex_url;
