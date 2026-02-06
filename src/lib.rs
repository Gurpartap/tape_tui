//! Rust port of pi-tui (skeleton).
//!
//! Invariant: single output gate — only `core::output::OutputGate::flush(..)` writes to the terminal.

pub mod config;
pub mod logging;

pub mod core;
pub mod platform;
pub mod render;
pub mod runtime;
pub mod widgets;

// Autocomplete support
pub use crate::core::autocomplete::{
    AutocompleteItem, AutocompleteProvider, AutocompleteSuggestions, CombinedAutocompleteProvider,
    SlashCommand,
};

// Components
pub use crate::widgets::{
    Box, CancellableLoader, Container, DefaultTextStyle, Editor, EditorHeightMode, EditorOptions,
    EditorPasteMode, EditorTheme, Image, ImageOptions, ImageTheme, Input, Loader, Markdown,
    MarkdownTheme, SelectItem, SelectList, SelectListTheme, SettingItem, SettingsList,
    SettingsListTheme, Spacer, Text, TruncatedText,
};

// Editor component interface
pub use crate::core::editor_component::EditorComponent;

// Fuzzy matching
pub use crate::core::fuzzy::{fuzzy_filter, fuzzy_match, FuzzyMatch};

// Keybindings
pub use crate::core::keybindings::{
    default_editor_keybindings_handle, EditorAction, EditorKeybindingsConfig,
    EditorKeybindingsHandle, EditorKeybindingsManager, KeyId, DEFAULT_EDITOR_KEYBINDINGS,
};

// Keyboard input handling
pub use crate::core::input::{
    is_key_release, is_key_repeat, matches_key, parse_key, Key, KeyEventType,
};
pub use crate::core::input_event::InputEvent;

// Input buffering
pub use crate::platform::stdin_buffer::{StdinBuffer, StdinBufferEventMap, StdinBufferOptions};

// Terminal interface and implementations
pub use crate::core::terminal::Terminal;
pub use crate::platform::process_terminal::ProcessTerminal;

// Terminal image support
pub use crate::core::terminal_image::{
    allocate_image_id, calculate_image_rows, delete_all_kitty_images, delete_kitty_image,
    detect_capabilities, encode_iterm2, encode_kitty, get_capabilities, get_cell_dimensions,
    get_gif_dimensions, get_image_dimensions, get_jpeg_dimensions, get_png_dimensions,
    get_webp_dimensions, image_fallback, render_image, reset_capabilities_cache,
    set_cell_dimensions, CellDimensions, ImageDimensions, ImageProtocol, ImageRenderOptions,
    TerminalCapabilities,
};

// TUI runtime + overlays
pub use crate::core::component::{Component, Focusable};
pub use crate::render::overlay::{OverlayAnchor, OverlayMargin, OverlayOptions, SizeValue};
pub use crate::runtime::ime::CURSOR_MARKER;
pub use crate::runtime::tui::OverlayHandle;

pub type TUI<T> = crate::runtime::tui::TuiRuntime<T>;

pub fn is_focusable(component: &mut dyn Component) -> bool {
    component.as_focusable().is_some()
}

// Utilities
pub use crate::core::text::slice::wrap_text_with_ansi;
pub use crate::core::text::utils::truncate_to_width;
pub use crate::core::text::width::visible_width;
