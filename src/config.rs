//! Environment configuration (Phase 0 skeleton).
//!
//! TODO: parse env vars:
//! - PI_HARDWARE_CURSOR
//! - PI_CLEAR_ON_SHRINK
//! - PI_TUI_WRITE_LOG
//! - PI_TUI_DEBUG
//! - PI_DEBUG_REDRAW

#[derive(Debug, Clone)]
pub struct EnvConfig {
    pub hardware_cursor: bool,
    pub clear_on_shrink: bool,
    pub tui_write_log: Option<String>,
    pub tui_debug: bool,
    pub debug_redraw: bool,
}
