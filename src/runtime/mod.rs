//! Runtime orchestration.

pub mod component_registry;
pub mod ime;
pub mod overlay;
pub mod tui;

pub use component_registry::ComponentId;
pub use overlay::{
    OverlayAnchor, OverlayId, OverlayMargin, OverlayOptions, OverlayVisibility, SizeValue,
};
pub use tui::{
    Command, CustomCommand, CustomCommandCtx, CustomCommandError, RuntimeHandle, TerminalOp,
};
