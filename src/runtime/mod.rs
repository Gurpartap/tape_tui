//! Runtime orchestration.

pub mod component_registry;
pub mod ime;
mod inline_viewport;
pub mod surface;
pub mod tui;

pub use component_registry::ComponentId;
pub use surface::{
    SurfaceAnchor, SurfaceId, SurfaceInputPolicy, SurfaceKind, SurfaceLayoutOptions, SurfaceMargin,
    SurfaceOptions, SurfaceSizeValue, SurfaceVisibility,
};
pub use tui::{
    Command, CustomCommand, CustomCommandCtx, CustomCommandError, RuntimeHandle,
    RuntimeRenderTelemetrySnapshot, SurfaceHandle, SurfaceTransactionMutation, TerminalOp,
};
