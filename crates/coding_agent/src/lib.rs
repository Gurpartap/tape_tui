//! Terminal coding agent runtime + TUI crate.
//!
//! ## Provider bootstrap
//!
//! `coding_agent` requires explicit provider selection:
//!
//! - `CODING_AGENT_PROVIDER=mock` for deterministic local tests
//! - `CODING_AGENT_PROVIDER=codex-api` for Codex API transport
//!
//! When `CODING_AGENT_PROVIDER=codex-api`, set `CODING_AGENT_CODEX_CONFIG_PATH`
//! to a readable UTF-8 JSON file with this shape:
//!
//! ```json
//! {
//!   "access_token": "<jwt-with-https://api.openai.com/auth.chatgpt_account_id>",
//!   "models": ["gpt-5.3-codex"],
//!   "timeout_sec": 120
//! }
//! ```
//!
//! Contract notes:
//! - `access_token` is required and must be a JWT with claim
//!   `https://api.openai.com/auth.chatgpt_account_id`.
//! - `models` is required and must include at least one non-empty model ID.
//! - `timeout_sec` is optional and must be > 0 when provided.
//! - Unknown JSON fields are rejected.
//!
//! ## System instructions
//!
//! Runtime run requests always include required system instructions.
//! Set `CODING_AGENT_SYSTEM_INSTRUCTIONS` to override the built-in default base
//! block; runtime appends a concise tool-use policy and tool inventory before
//! dispatching each provider run.
//!
//! Conversation memory contract: `coding_agent` owns model-facing run history and
//! replays it on every turn through provider-neutral `RunMessage` items.
//!
//! Codex transport contract: Responses API `input` must be list-shaped JSON.
//! Plain string `input` payloads are rejected during codex_api request preflight.

pub mod app;
pub mod commands;
pub mod provider;
pub mod providers;
pub mod runtime;
pub mod tools;
pub mod tui;
