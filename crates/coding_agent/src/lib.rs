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
//!
//! ## Persistent sessions (v1 fail-closed contract)
//!
//! In default mode (`coding_agent` with no resume flags), startup preallocates
//! a session identity (`session_id`, `created_at`, absolute `cwd`) and wires
//! that `session_id` into provider bootstrap immediately. Session file creation
//! is lazy: `<cwd>/.agent/sessions/` and the JSONL file are materialized on the
//! first persisted user turn.
//!
//! The materialized header uses the preallocated startup metadata, so
//! `created_at` reflects session start time, not first file-write time.
//!
//! Passing `--continue` switches startup to strict resume mode: it opens the
//! latest session file under `<cwd>/.agent/sessions/`, replays the current leaf
//! into model-facing memory, and appends subsequent entries to that same file.
//! Passing `--session <session-filepath>` resumes from an explicit session file
//! path (absolute or `<cwd>`-relative) with the same strict replay semantics.
//! Session durability is strict: the header write and every appended entry are
//! persisted with `sync_data` before reporting success.
//!
//! Failure policy is fail-closed:
//! - default startup seed creation/open/parse/validation failures are hard errors;
//! - `--continue` resume failures never fall back to creating a new session;
//! - runtime append/sync failures are fatal (error mode + stop request + exit);
//! - no degraded persistence fallback mode is used by the binary startup path.
//!
//! Persistence is event-driven (user submit / committed run events) only.
//! There is no additional save-on-exit flush step.
//!
//! Replay is strict and deterministic over graph-valid entries only. Malformed
//! JSON, unknown fields/kinds, unsupported versions, duplicate ids, dangling
//! parent ids, and unknown leaf replays are explicit hard errors.
//!
//! Deferred scope note for v1: no persistence reset markers are defined yet.
//! `/clear` and `memory_reset` persistence semantics are intentionally deferred;
//! `/clear` only affects in-memory state for the running process.
//!
//! ## TUI transcript viewport behavior
//!
//! The inline TUI keeps full transcript history in memory/cache, while rendering
//! only the visible transcript window per frame based on terminal row budget.
//! Default behavior follows tail (`scroll=0`). Use `PageUp`/`PageDown` to scroll,
//! `Home` to jump to earliest cached lines, and `End` to return to follow-tail.
//! While scrolled up, new transcript chunks do not force-follow automatically.

pub mod app;
pub mod commands;
pub mod provider;
pub mod providers;
pub mod runtime;
pub mod tools;
pub mod tui;
