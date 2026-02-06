//! Structured input events produced by the runtime.

use crate::core::input::KeyEventType;

/// Input event delivered to components.
///
/// `raw` contains the exact byte sequence received from the terminal (UTF-8 decoded).
/// `key_id` is a best-effort normalized identifier for matching keybindings (if applicable).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputEvent {
    pub raw: String,
    pub key_id: Option<String>,
    pub event_type: KeyEventType,
}

