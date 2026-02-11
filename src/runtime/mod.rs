//! Runtime orchestration (Phase 4+).

pub mod focus;
pub mod ime;
pub mod component_registry;
pub mod tui;

pub use component_registry::ComponentId;
pub use tui::{Command, RuntimeHandle, TerminalOp};
