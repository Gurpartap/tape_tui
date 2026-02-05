//! Terminal image capabilities and helpers (Phase 6).

use std::env;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

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
const KITTY_CHUNK_SIZE: usize = 4096;
const KITTY_ID_MAX: u32 = 0xffff_fffe;

static IMAGE_ID_COUNTER: OnceLock<AtomicU32> = OnceLock::new();

#[derive(Debug, Clone, Default)]
pub struct KittyEncodeOptions {
    pub columns: Option<u32>,
    pub rows: Option<u32>,
    pub image_id: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct Iterm2EncodeOptions {
    pub width: Option<String>,
    pub height: Option<String>,
    pub name: Option<String>,
    pub preserve_aspect_ratio: Option<bool>,
    pub inline: Option<bool>,
}

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

pub fn allocate_image_id() -> u32 {
    let counter = IMAGE_ID_COUNTER.get_or_init(|| AtomicU32::new(image_id_seed()));
    let id = counter.fetch_add(1, Ordering::Relaxed);
    (id % KITTY_ID_MAX) + 1
}

pub fn encode_kitty(base64_data: &str, options: &KittyEncodeOptions) -> String {
    let mut params = vec!["a=T".to_string(), "f=100".to_string(), "q=2".to_string()];

    if let Some(columns) = options.columns {
        params.push(format!("c={columns}"));
    }
    if let Some(rows) = options.rows {
        params.push(format!("r={rows}"));
    }
    if let Some(image_id) = options.image_id {
        params.push(format!("i={image_id}"));
    }

    if base64_data.len() <= KITTY_CHUNK_SIZE {
        return format!(
            "{prefix}{params};{data}\x1b\\",
            prefix = KITTY_PREFIX,
            params = params.join(","),
            data = base64_data
        );
    }

    let mut chunks = Vec::new();
    let mut offset = 0usize;
    let mut is_first = true;

    while offset < base64_data.len() {
        let end = (offset + KITTY_CHUNK_SIZE).min(base64_data.len());
        let chunk = &base64_data[offset..end];
        let is_last = end >= base64_data.len();

        if is_first {
            chunks.push(format!(
                "{prefix}{params},m=1;{chunk}\x1b\\",
                prefix = KITTY_PREFIX,
                params = params.join(","),
                chunk = chunk
            ));
            is_first = false;
        } else if is_last {
            chunks.push(format!("{prefix}m=0;{chunk}\x1b\\", prefix = KITTY_PREFIX, chunk = chunk));
        } else {
            chunks.push(format!("{prefix}m=1;{chunk}\x1b\\", prefix = KITTY_PREFIX, chunk = chunk));
        }

        offset = end;
    }

    chunks.join("")
}

pub fn delete_kitty_image(image_id: u32) -> String {
    format!("{prefix}a=d,d=I,i={image_id}\x1b\\", prefix = KITTY_PREFIX)
}

pub fn delete_all_kitty_images() -> String {
    format!("{prefix}a=d,d=A\x1b\\", prefix = KITTY_PREFIX)
}

pub fn encode_iterm2(base64_data: &str, options: &Iterm2EncodeOptions) -> String {
    let inline = options.inline.unwrap_or(true);
    let mut params = vec![format!("inline={}", if inline { 1 } else { 0 })];

    if let Some(width) = &options.width {
        params.push(format!("width={width}"));
    }
    if let Some(height) = &options.height {
        params.push(format!("height={height}"));
    }
    if let Some(name) = &options.name {
        let name_base64 = base64_encode(name.as_bytes());
        params.push(format!("name={name_base64}"));
    }
    if options.preserve_aspect_ratio == Some(false) {
        params.push("preserveAspectRatio=0".to_string());
    }

    format!(
        "{prefix}{params}:{data}\x07",
        prefix = ITERM2_PREFIX,
        params = params.join(";"),
        data = base64_data
    )
}

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    if data.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    let mut idx = 0usize;

    while idx < data.len() {
        let b0 = data[idx];
        let b1 = data.get(idx + 1).copied().unwrap_or(0);
        let b2 = data.get(idx + 2).copied().unwrap_or(0);
        let triple = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);

        out.push(TABLE[((triple >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3f) as usize] as char);

        if idx + 1 < data.len() {
            out.push(TABLE[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }

        if idx + 2 < data.len() {
            out.push(TABLE[(triple & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }

        idx += 3;
    }

    out
}

fn image_id_seed() -> u32 {
    let duration = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let nanos = duration.as_nanos() as u64;
    let pid = std::process::id() as u64;
    let mixed = nanos ^ (pid << 32) ^ (pid << 16) ^ pid;
    let seed = (mixed as u32) % KITTY_ID_MAX;
    seed + 1
}

#[cfg(test)]
mod tests {
    use super::{
        allocate_image_id, delete_all_kitty_images, delete_kitty_image, encode_iterm2, encode_kitty,
        get_cell_dimensions, is_image_line, set_cell_dimensions, CellDimensions, Iterm2EncodeOptions,
        KittyEncodeOptions,
    };

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

    #[test]
    fn allocate_image_id_is_in_range() {
        for _ in 0..100 {
            let id = allocate_image_id();
            assert!(id >= 1 && id <= 0xffff_fffe);
        }
    }

    #[test]
    fn encode_kitty_single_chunk() {
        let options = KittyEncodeOptions {
            columns: Some(2),
            rows: Some(3),
            image_id: Some(7),
        };
        let encoded = encode_kitty("AAAA", &options);
        assert_eq!(encoded, "\x1b_Ga=T,f=100,q=2,c=2,r=3,i=7;AAAA\x1b\\");
    }

    #[test]
    fn encode_kitty_multi_chunk() {
        let data = "a".repeat(4097);
        let encoded = encode_kitty(&data, &KittyEncodeOptions::default());
        assert!(encoded.starts_with("\x1b_Ga=T,f=100,q=2,m=1;"));
        assert!(encoded.contains("\x1b_Gm=0;"));
    }

    #[test]
    fn kitty_delete_sequences_match() {
        assert_eq!(delete_kitty_image(42), "\x1b_Ga=d,d=I,i=42\x1b\\");
        assert_eq!(delete_all_kitty_images(), "\x1b_Ga=d,d=A\x1b\\");
    }

    #[test]
    fn encode_iterm2_includes_name_and_flags() {
        let options = Iterm2EncodeOptions {
            width: Some("10".to_string()),
            height: Some("auto".to_string()),
            name: Some("foo.png".to_string()),
            preserve_aspect_ratio: Some(false),
            inline: Some(false),
        };
        let encoded = encode_iterm2("AAAA", &options);
        assert_eq!(
            encoded,
            "\x1b]1337;File=inline=0;width=10;height=auto;name=Zm9vLnBuZw==;preserveAspectRatio=0:AAAA\x07"
        );
    }
}
