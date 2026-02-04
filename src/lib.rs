//! Rust port of pi-tui (skeleton).
//!
//! Invariant: single output gate — only the renderer writes to the terminal.

pub mod config;
pub mod logging;

pub mod core;
pub mod render;
pub mod runtime;
pub mod platform;
pub mod widgets;
