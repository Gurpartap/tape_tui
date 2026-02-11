//! Runtime orchestration (Phase 4+).

pub mod focus;
pub mod ime;
pub mod tui;

pub use tui::{Command, RuntimeHandle, TerminalOp};
