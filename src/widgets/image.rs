//! Image widget (Phase 24).

use crate::core::component::Component;
use crate::core::terminal_image::{
    get_capabilities, get_image_dimensions, image_fallback, render_image, ImageDimensions,
    ImageRenderOptions,
};

pub struct ImageTheme {
    pub fallback_color: Box<dyn Fn(&str) -> String>,
}

#[derive(Debug, Clone, Default)]
pub struct ImageOptions {
    pub max_width_cells: Option<u32>,
    pub max_height_cells: Option<u32>,
    pub filename: Option<String>,
    pub image_id: Option<u32>,
}

pub struct Image {
    base64_data: String,
    mime_type: String,
    dimensions: ImageDimensions,
    theme: ImageTheme,
    options: ImageOptions,
    image_id: Option<u32>,
    cached_lines: Option<Vec<String>>,
    cached_width: Option<usize>,
}

impl Image {
    pub fn new(
        base64_data: impl Into<String>,
        mime_type: impl Into<String>,
        theme: ImageTheme,
        options: ImageOptions,
        dimensions: Option<ImageDimensions>,
    ) -> Self {
        let base64_data = base64_data.into();
        let mime_type = mime_type.into();
        let dimensions = dimensions
            .or_else(|| get_image_dimensions(&base64_data, &mime_type))
            .unwrap_or(ImageDimensions {
                width_px: 800,
                height_px: 600,
            });
        let image_id = options.image_id;
        Self {
            base64_data,
            mime_type,
            dimensions,
            theme,
            options,
            image_id,
            cached_lines: None,
            cached_width: None,
        }
    }

    pub fn get_image_id(&self) -> Option<u32> {
        self.image_id
    }
}

impl Component for Image {
    fn render(&mut self, width: usize) -> Vec<String> {
        if let (Some(lines), Some(cached_width)) = (self.cached_lines.as_ref(), self.cached_width) {
            if cached_width == width {
                return lines.clone();
            }
        }

        let max_width_limit = width.saturating_sub(2) as u32;
        let max_width = self
            .options
            .max_width_cells
            .unwrap_or(60)
            .min(max_width_limit);

        let caps = get_capabilities();
        let mut lines = Vec::new();

        if caps.images.is_some() {
            let result = render_image(
                &self.base64_data,
                self.dimensions,
                &ImageRenderOptions {
                    max_width_cells: Some(max_width),
                    max_height_cells: self.options.max_height_cells,
                    preserve_aspect_ratio: None,
                    image_id: self.image_id,
                },
            );

            if let Some(result) = result {
                if result.image_id.is_some() {
                    self.image_id = result.image_id;
                }
                let rows = result.rows as usize;
                if rows > 0 {
                    for _ in 0..rows.saturating_sub(1) {
                        lines.push(String::new());
                    }
                    let move_up = if rows > 1 {
                        format!("\x1b[{}A", rows - 1)
                    } else {
                        String::new()
                    };
                    lines.push(format!("{move_up}{}", result.sequence));
                }
            } else {
                let fallback = image_fallback(
                    &self.mime_type,
                    Some(self.dimensions),
                    self.options.filename.as_deref(),
                );
                lines.push((self.theme.fallback_color)(&fallback));
            }
        } else {
            let fallback = image_fallback(
                &self.mime_type,
                Some(self.dimensions),
                self.options.filename.as_deref(),
            );
            lines.push((self.theme.fallback_color)(&fallback));
        }

        self.cached_lines = Some(lines.clone());
        self.cached_width = Some(width);

        lines
    }

    fn invalidate(&mut self) {
        self.cached_lines = None;
        self.cached_width = None;
    }
}

#[cfg(test)]
mod tests {
    use super::{Image, ImageOptions, ImageTheme};
    use crate::core::component::Component;
    use crate::core::terminal_image::{reset_capabilities_cache, ImageDimensions};
    use std::env;
    use std::sync::{Mutex, OnceLock};

    struct EnvGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(value) = &self.previous {
                env::set_var(self.key, value);
            } else {
                env::remove_var(self.key);
            }
        }
    }

    fn set_env_guard(key: &'static str, value: Option<&str>) -> EnvGuard {
        let previous = env::var(key).ok();
        if let Some(value) = value {
            env::set_var(key, value);
        } else {
            env::remove_var(key);
        }
        EnvGuard { key, previous }
    }

    fn theme() -> ImageTheme {
        ImageTheme {
            fallback_color: Box::new(|text| format!("<{text}>")),
        }
    }

    fn env_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn image_renders_kitty_sequence_rows() {
        let _guard = env_test_lock().lock().expect("test lock poisoned");
        let _term = set_env_guard("TERM", Some("xterm-256color"));
        let _term_program = set_env_guard("TERM_PROGRAM", Some("kitty"));
        let _kitty = set_env_guard("KITTY_WINDOW_ID", Some("1"));
        let _wezterm = set_env_guard("WEZTERM_PANE", None);
        let _iterm = set_env_guard("ITERM_SESSION_ID", None);
        let _ghostty = set_env_guard("GHOSTTY_RESOURCES_DIR", None);
        reset_capabilities_cache();

        let options = ImageOptions {
            max_width_cells: Some(10),
            max_height_cells: None,
            filename: None,
            image_id: Some(5),
        };
        let dims = ImageDimensions {
            width_px: 100,
            height_px: 50,
        };
        let mut image = Image::new("AAAA", "image/png", theme(), options, Some(dims));
        let lines = image.render(20);

        assert_eq!(image.get_image_id(), Some(5));
        assert_eq!(lines.len(), 3);
        assert!(lines.last().unwrap().contains("\x1b_G"));
        assert!(lines.last().unwrap().starts_with("\x1b[2A"));

        reset_capabilities_cache();
    }

    #[test]
    fn image_falls_back_without_capabilities() {
        let _guard = env_test_lock().lock().expect("test lock poisoned");
        let _term = set_env_guard("TERM", Some("xterm-256color"));
        let _term_program = set_env_guard("TERM_PROGRAM", Some("vscode"));
        let _kitty = set_env_guard("KITTY_WINDOW_ID", None);
        let _wezterm = set_env_guard("WEZTERM_PANE", None);
        let _iterm = set_env_guard("ITERM_SESSION_ID", None);
        let _ghostty = set_env_guard("GHOSTTY_RESOURCES_DIR", None);
        reset_capabilities_cache();

        let options = ImageOptions {
            max_width_cells: None,
            max_height_cells: None,
            filename: Some("file.png".to_string()),
            image_id: None,
        };
        let dims = ImageDimensions {
            width_px: 200,
            height_px: 100,
        };
        let mut image = Image::new("AAAA", "image/png", theme(), options, Some(dims));
        let lines = image.render(40);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "<[Image: file.png [image/png] 200x100]>");

        reset_capabilities_cache();
    }
}
