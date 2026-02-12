#![allow(unused_imports)]

use tape_tui::{
    allocate_image_id, calculate_image_rows, default_editor_keybindings_handle,
    delete_all_kitty_images, delete_kitty_image, detect_capabilities, encode_iterm2, encode_kitty,
    fuzzy_filter, fuzzy_match, get_capabilities, get_cell_dimensions, get_gif_dimensions,
    get_image_dimensions, get_jpeg_dimensions, get_png_dimensions, get_webp_dimensions,
    image_fallback, is_focusable, is_key_release, is_key_repeat, matches_key, parse_key,
    render_image, reset_capabilities_cache, set_cell_dimensions, truncate_to_width, visible_width,
    wrap_text_with_ansi, AutocompleteItem, AutocompleteProvider, AutocompleteSuggestions,
    Box as UiBox, CancellableLoader, CellDimensions, CombinedAutocompleteProvider, Component,
    Container, DefaultTextStyle, Editor, EditorAction, EditorComponent, EditorKeybindingsConfig,
    EditorKeybindingsHandle, EditorKeybindingsManager, EditorOptions, EditorTheme, Focusable,
    FuzzyMatch, Image, ImageDimensions, ImageOptions, ImageProtocol, ImageRenderOptions,
    ImageTheme, Input, InputEvent, Key, KeyEventType, KeyId, Loader, Markdown, MarkdownTheme,
    ProcessTerminal, SelectItem, SelectList, SelectListTheme, SettingItem, SettingsList,
    SettingsListTheme, SlashCommand, Spacer, StdinBuffer, StdinBufferEventMap, StdinBufferOptions,
    SurfaceAnchor, SurfaceHandle, SurfaceId, SurfaceInputPolicy, SurfaceKind, SurfaceLayoutOptions,
    SurfaceMargin, SurfaceOptions, SurfaceSizeValue, SurfaceTransactionMutation, SurfaceVisibility,
    Terminal, TerminalCapabilities, Text, TruncatedText, CURSOR_MARKER, DEFAULT_EDITOR_KEYBINDINGS,
    TUI,
};

#[test]
fn public_api_exports_compile() {}
