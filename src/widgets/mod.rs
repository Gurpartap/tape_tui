//! Optional higher-level widgets.

pub mod r#box;
pub mod cancellable_loader;
pub mod container;
pub mod editor;
pub mod image;
pub mod input;
pub mod loader;
pub mod markdown;
pub mod select_list;
pub mod settings_list;
pub mod spacer;
pub mod text;
pub mod truncated_text;

pub use cancellable_loader::{AbortSignal, CancellableLoader};
pub use container::Container;
pub use editor::{
    Editor, EditorHeightMode, EditorOptions, EditorPasteMode, EditorTheme, TextChunk,
};
pub use image::{Image, ImageOptions, ImageTheme};
pub use input::Input;
pub use loader::Loader;
pub use markdown::{
    highlight_markdown_code_ansi, prewarm_markdown_highlighting, DefaultTextStyle, Markdown,
    MarkdownTheme,
};
pub use r#box::Box;
pub use select_list::{SelectItem, SelectList, SelectListTheme};
pub use settings_list::{SettingItem, SettingsList, SettingsListOptions, SettingsListTheme};
pub use spacer::Spacer;
pub use text::Text;
pub use truncated_text::TruncatedText;
