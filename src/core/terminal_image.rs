//! Terminal image capabilities and helpers (Phase 6).

use std::env;
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageProtocol {
    Kitty,
    Iterm2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCapabilities {
    pub images: Option<ImageProtocol>,
    pub true_color: bool,
    pub hyperlinks: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellDimensions {
    pub width_px: u32,
    pub height_px: u32,
}

static CAPABILITIES: OnceLock<Mutex<Option<TerminalCapabilities>>> = OnceLock::new();
static CELL_DIMENSIONS: OnceLock<Mutex<CellDimensions>> = OnceLock::new();

const KITTY_PREFIX: &str = "\x1b_G";
const ITERM2_PREFIX: &str = "\x1b]1337;File=";

pub fn get_cell_dimensions() -> CellDimensions {
    let lock = CELL_DIMENSIONS.get_or_init(|| Mutex::new(CellDimensions {
        width_px: 9,
        height_px: 18,
    }));
    *lock.lock().expect("cell dimensions lock poisoned")
}

pub fn set_cell_dimensions(dims: CellDimensions) {
    let lock = CELL_DIMENSIONS.get_or_init(|| Mutex::new(CellDimensions {
        width_px: 9,
        height_px: 18,
    }));
    let mut current = lock.lock().expect("cell dimensions lock poisoned");
    *current = dims;
}

pub fn detect_capabilities() -> TerminalCapabilities {
    let term_program = env::var("TERM_PROGRAM").unwrap_or_default().to_lowercase();
    let term = env::var("TERM").unwrap_or_default().to_lowercase();
    let color_term = env::var("COLORTERM").unwrap_or_default().to_lowercase();

    if env::var("KITTY_WINDOW_ID").is_ok() || term_program == "kitty" {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        };
    }

    if term_program == "ghostty" || term.contains("ghostty") || env::var("GHOSTTY_RESOURCES_DIR").is_ok() {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        };
    }

    if env::var("WEZTERM_PANE").is_ok() || term_program == "wezterm" {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        };
    }

    if env::var("ITERM_SESSION_ID").is_ok() || term_program == "iterm.app" {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Iterm2),
            true_color: true,
            hyperlinks: true,
        };
    }

    if term_program == "vscode" {
        return TerminalCapabilities {
            images: None,
            true_color: true,
            hyperlinks: true,
        };
    }

    if term_program == "alacritty" {
        return TerminalCapabilities {
            images: None,
            true_color: true,
            hyperlinks: true,
        };
    }

    let true_color = color_term == "truecolor" || color_term == "24bit";
    TerminalCapabilities {
        images: None,
        true_color,
        hyperlinks: true,
    }
}

pub fn get_capabilities() -> TerminalCapabilities {
    let lock = CAPABILITIES.get_or_init(|| Mutex::new(None));
    let mut cached = lock.lock().expect("capabilities lock poisoned");
    if let Some(value) = *cached {
        return value;
    }
    let detected = detect_capabilities();
    *cached = Some(detected);
    detected
}

pub fn reset_capabilities_cache() {
    if let Some(lock) = CAPABILITIES.get() {
        let mut cached = lock.lock().expect("capabilities lock poisoned");
        *cached = None;
    }
}

pub fn is_image_line(line: &str) -> bool {
    if line.starts_with(KITTY_PREFIX) || line.starts_with(ITERM2_PREFIX) {
        return true;
    }
    line.contains(KITTY_PREFIX) || line.contains(ITERM2_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::{get_cell_dimensions, is_image_line, set_cell_dimensions, CellDimensions};

    #[test]
    fn image_line_detection_matches_prefixes() {
        assert!(is_image_line("\x1b_Gf=100;data"));
        assert!(is_image_line("prefix\x1b]1337;File=data"));
        assert!(!is_image_line("plain text"));
    }

    #[test]
    fn cell_dimensions_update() {
        let original = get_cell_dimensions();
        let updated = CellDimensions {
            width_px: original.width_px + 1,
            height_px: original.height_px + 2,
        };
        set_cell_dimensions(updated);
        assert_eq!(get_cell_dimensions(), updated);
        set_cell_dimensions(original);
    }
}
