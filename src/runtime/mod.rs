//! Runtime orchestration.

pub mod component_registry;
pub mod ime;
pub mod overlay;
pub mod surface;
pub mod tui;

pub use component_registry::ComponentId;
pub use overlay::{
    OverlayAnchor, OverlayId, OverlayMargin, OverlayOptions, OverlayVisibility, SizeValue,
};
pub use surface::{SurfaceId, SurfaceInputPolicy, SurfaceKind, SurfaceOptions};
pub use tui::{
    Command, CustomCommand, CustomCommandCtx, CustomCommandError, RuntimeHandle, SurfaceHandle,
    TerminalOp,
};
