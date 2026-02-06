//! Text helpers (ANSI parsing, width calculations, slicing/wrapping, truncation).
//!
//! These helpers are pure (string in/string out) and live under `core` so widgets can depend on
//! them without importing anything from the render layer.

pub mod ansi;
pub mod slice;
pub mod utils;
pub mod width;

