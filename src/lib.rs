//! Rust port of tape-tui.
//!
//! Invariant: single output gate — only `core::output::OutputGate::flush(..)` writes to the
//! terminal.
//!
//! # Public API Overview
//! - Build widgets and compose them into a runtime via [`TUI`].
//! - Parse/inspect input with key and event helpers.
//! - Manage layered UI using surface lifecycle primitives (`show_surface`, [`SurfaceHandle`],
//!   [`SurfaceOptions`]).
//! - Use text and width helpers for ANSI-safe formatting.
//!
//! # Runtime Alias
//! [`TUI`] is a type alias for `runtime::tui::TuiRuntime<T>`.

#![allow(
    clippy::derivable_impls,
    clippy::needless_range_loop,
    clippy::question_mark,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::unnecessary_map_or
)]

pub mod config;
pub mod logging;

pub mod core;
pub mod platform;
pub mod render;
pub mod runtime;
pub mod widgets;

/// Autocomplete primitives and providers.
pub use crate::core::autocomplete::{
    AutocompleteItem, AutocompleteProvider, AutocompleteSuggestions, CombinedAutocompleteProvider,
    SlashCommand,
};

/// Built-in UI components.
pub use crate::widgets::{
    Box, CancellableLoader, Container, DefaultTextStyle, Editor, EditorHeightMode, EditorOptions,
    EditorPasteMode, EditorTheme, Image, ImageOptions, ImageTheme, Input, Loader, Markdown,
    MarkdownTheme, SelectItem, SelectList, SelectListTheme, SettingItem, SettingsList,
    SettingsListTheme, Spacer, Text, TruncatedText,
};

/// Editor component behavior contract.
pub use crate::core::editor_component::EditorComponent;

/// Fuzzy matching helpers.
pub use crate::core::fuzzy::{fuzzy_filter, fuzzy_match, FuzzyMatch};

/// Keybinding configuration and default mappings.
pub use crate::core::keybindings::{
    default_editor_keybindings_handle, EditorAction, EditorKeybindingsConfig,
    EditorKeybindingsHandle, EditorKeybindingsManager, KeyId, DEFAULT_EDITOR_KEYBINDINGS,
};

/// Keyboard input parsing and matching helpers.
pub use crate::core::input::{
    is_key_release, is_key_repeat, matches_key, parse_key, Key, KeyEventType,
};
pub use crate::core::input_event::InputEvent;

/// Input buffering types for chunked terminal streams.
pub use crate::platform::stdin_buffer::{StdinBuffer, StdinBufferEventMap, StdinBufferOptions};

/// Terminal interfaces and process-backed implementation.
pub use crate::core::output::TerminalTitleExt;
pub use crate::core::terminal::Terminal;
pub use crate::platform::process_terminal::ProcessTerminal;

/// Terminal image capability detection, encoding, and rendering.
pub use crate::core::terminal_image::{
    allocate_image_id, calculate_image_rows, delete_all_kitty_images, delete_kitty_image,
    detect_capabilities, encode_iterm2, encode_kitty, get_capabilities, get_cell_dimensions,
    get_gif_dimensions, get_image_dimensions, get_jpeg_dimensions, get_png_dimensions,
    get_webp_dimensions, image_fallback, render_image, reset_capabilities_cache,
    set_cell_dimensions, CellDimensions, ImageDimensions, ImageProtocol, ImageRenderOptions,
    TerminalCapabilities, TerminalImageState,
};

/// Runtime component traits and cursor marker helper.
pub use crate::core::component::{Component, Focusable};
pub use crate::core::cursor::CURSOR_MARKER;
/// Render-layer frame types.
pub use crate::render::{Frame, Line, Span};
/// Stable component identifier type.
pub use crate::runtime::component_registry::ComponentId;
/// Handle used to mutate shown surface layers at runtime.
pub use crate::runtime::tui::SurfaceHandle;
/// Runtime and surface option/model types.
pub use crate::runtime::{
    CustomCommand, CustomCommandCtx, CustomCommandError, SurfaceAnchor, SurfaceId,
    SurfaceInputPolicy, SurfaceKind, SurfaceLayoutOptions, SurfaceMargin, SurfaceOptions,
    SurfaceSizeValue, SurfaceTransactionMutation, SurfaceVisibility,
};

/// Alias for the main runtime type.
pub type TUI<T> = crate::runtime::tui::TuiRuntime<T>;

/// Returns whether a component exposes focus behavior via [`Focusable`].
pub fn is_focusable(component: &mut dyn Component) -> bool {
    component.as_focusable().is_some()
}

/// ANSI-aware wrapping helper.
pub use crate::core::text::slice::wrap_text_with_ansi;
/// ANSI-aware truncation helper.
pub use crate::core::text::utils::truncate_to_width;
/// Visible width helper that ignores ANSI control sequences.
pub use crate::core::text::width::visible_width;
