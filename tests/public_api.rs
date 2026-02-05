#![allow(unused_imports)]

use pi_tui::{
    allocate_image_id, calculate_image_rows, delete_all_kitty_images, delete_kitty_image, detect_capabilities,
    encode_iterm2, encode_kitty, fuzzy_filter, fuzzy_match, get_capabilities, get_cell_dimensions,
    get_editor_keybindings, get_gif_dimensions, get_image_dimensions, get_jpeg_dimensions, get_png_dimensions,
    get_webp_dimensions, image_fallback, is_focusable, is_key_release, is_key_repeat,
    is_kitty_protocol_active, matches_key, parse_key, render_image, reset_capabilities_cache,
    set_cell_dimensions, set_editor_keybindings, set_kitty_protocol_active, truncate_to_width, visible_width,
    wrap_text_with_ansi, AutocompleteItem, AutocompleteProvider, AutocompleteSuggestions, Box as UiBox,
    CancellableLoader, CellDimensions, CombinedAutocompleteProvider, Component, Container, CURSOR_MARKER,
    DefaultTextStyle, Editor, EditorAction, EditorComponent, EditorKeybindingsConfig, EditorKeybindingsManager,
    EditorOptions, EditorTheme, Focusable, FuzzyMatch, Image, ImageDimensions, ImageOptions, ImageProtocol,
    ImageRenderOptions, ImageTheme, Input, Key, KeyEventType, KeyId, Loader, Markdown, MarkdownTheme, OverlayAnchor,
    OverlayHandle, OverlayMargin, OverlayOptions, ProcessTerminal, SelectItem, SelectList, SelectListTheme,
    SettingItem, SettingsList, SettingsListTheme, SizeValue, SlashCommand, Spacer, StdinBuffer,
    StdinBufferEventMap, StdinBufferOptions, Terminal, TerminalCapabilities, Text, TruncatedText, TUI,
    DEFAULT_EDITOR_KEYBINDINGS,
};

#[test]
fn public_api_exports_compile() {}
