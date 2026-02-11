//! Terminal image capabilities and helpers.

use std::env;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageDimensions {
    pub width_px: u32,
    pub height_px: u32,
}

#[derive(Debug, Clone, Default)]
pub struct ImageRenderOptions {
    pub max_width_cells: Option<u32>,
    pub max_height_cells: Option<u32>,
    pub preserve_aspect_ratio: Option<bool>,
    pub image_id: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRenderResult {
    pub sequence: String,
    pub rows: u32,
    pub image_id: Option<u32>,
}

#[derive(Debug)]
pub struct TerminalImageState {
    capabilities: Mutex<Option<TerminalCapabilities>>,
    cell_dimensions: Mutex<CellDimensions>,
    image_id_counter: AtomicU32,
}

impl Default for TerminalImageState {
    fn default() -> Self {
        Self {
            capabilities: Mutex::new(None),
            cell_dimensions: Mutex::new(CellDimensions {
                width_px: 9,
                height_px: 18,
            }),
            image_id_counter: AtomicU32::new(0),
        }
    }
}

const KITTY_PREFIX: &str = "\x1b_G";
const ITERM2_PREFIX: &str = "\x1b]1337;File=";
const KITTY_CHUNK_SIZE: usize = 4096;
const KITTY_ID_MAX: u32 = 0xffff_fffe;

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

pub fn get_cell_dimensions(state: &TerminalImageState) -> CellDimensions {
    *state
        .cell_dimensions
        .lock()
        .expect("cell dimensions lock poisoned")
}

pub fn set_cell_dimensions(state: &TerminalImageState, dims: CellDimensions) {
    let mut current = state
        .cell_dimensions
        .lock()
        .expect("cell dimensions lock poisoned");
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

    if term_program == "ghostty"
        || term.contains("ghostty")
        || env::var("GHOSTTY_RESOURCES_DIR").is_ok()
    {
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

pub fn get_capabilities(state: &TerminalImageState) -> TerminalCapabilities {
    let mut cached = state
        .capabilities
        .lock()
        .expect("capabilities lock poisoned");
    if let Some(value) = *cached {
        return value;
    }
    let detected = detect_capabilities();
    *cached = Some(detected);
    detected
}

pub fn reset_capabilities_cache(state: &TerminalImageState) {
    let mut cached = state
        .capabilities
        .lock()
        .expect("capabilities lock poisoned");
    *cached = None;
}

pub fn is_image_line(line: &str) -> bool {
    if line.starts_with(KITTY_PREFIX) || line.starts_with(ITERM2_PREFIX) {
        return true;
    }
    line.contains(KITTY_PREFIX) || line.contains(ITERM2_PREFIX)
}

pub fn allocate_image_id(state: &TerminalImageState) -> u32 {
    let counter = state
        .image_id_counter
        .fetch_add(0x9e37_79b9, Ordering::Relaxed);
    let mut value = image_id_seed().wrapping_add(counter);
    value ^= value << 13;
    value ^= value >> 17;
    value ^= value << 5;
    (value % KITTY_ID_MAX).saturating_add(1)
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
            chunks.push(format!(
                "{prefix}m=0;{chunk}\x1b\\",
                prefix = KITTY_PREFIX,
                chunk = chunk
            ));
        } else {
            chunks.push(format!(
                "{prefix}m=1;{chunk}\x1b\\",
                prefix = KITTY_PREFIX,
                chunk = chunk
            ));
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

pub fn calculate_image_rows(
    image_dimensions: ImageDimensions,
    target_width_cells: u32,
    cell_dimensions: Option<CellDimensions>,
) -> u32 {
    let cell_dimensions = cell_dimensions.unwrap_or(CellDimensions {
        width_px: 9,
        height_px: 18,
    });
    let target_width_px = target_width_cells as f64 * cell_dimensions.width_px as f64;
    let scale = target_width_px / image_dimensions.width_px as f64;
    let scaled_height_px = image_dimensions.height_px as f64 * scale;
    let rows = (scaled_height_px / cell_dimensions.height_px as f64).ceil() as u32;
    rows.max(1)
}

pub fn get_png_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = base64_decode(base64_data)?;
    if buffer.len() < 24 {
        return None;
    }
    if buffer[0] != 0x89 || buffer[1] != 0x50 || buffer[2] != 0x4e || buffer[3] != 0x47 {
        return None;
    }
    let width = u32::from_be_bytes([buffer[16], buffer[17], buffer[18], buffer[19]]);
    let height = u32::from_be_bytes([buffer[20], buffer[21], buffer[22], buffer[23]]);
    Some(ImageDimensions {
        width_px: width,
        height_px: height,
    })
}

pub fn get_jpeg_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = base64_decode(base64_data)?;
    if buffer.len() < 2 {
        return None;
    }
    if buffer[0] != 0xff || buffer[1] != 0xd8 {
        return None;
    }

    let mut offset = 2usize;
    while offset < buffer.len().saturating_sub(9) {
        if buffer[offset] != 0xff {
            offset += 1;
            continue;
        }

        let marker = buffer[offset + 1];
        if (0xc0..=0xc2).contains(&marker) {
            let height = u16::from_be_bytes([buffer[offset + 5], buffer[offset + 6]]) as u32;
            let width = u16::from_be_bytes([buffer[offset + 7], buffer[offset + 8]]) as u32;
            return Some(ImageDimensions {
                width_px: width,
                height_px: height,
            });
        }

        if offset + 3 >= buffer.len() {
            return None;
        }
        let length = u16::from_be_bytes([buffer[offset + 2], buffer[offset + 3]]) as usize;
        if length < 2 {
            return None;
        }
        offset += 2 + length;
    }

    None
}

pub fn get_gif_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = base64_decode(base64_data)?;
    if buffer.len() < 10 {
        return None;
    }
    let sig = std::str::from_utf8(&buffer[0..6]).ok()?;
    if sig != "GIF87a" && sig != "GIF89a" {
        return None;
    }
    let width = u16::from_le_bytes([buffer[6], buffer[7]]) as u32;
    let height = u16::from_le_bytes([buffer[8], buffer[9]]) as u32;
    Some(ImageDimensions {
        width_px: width,
        height_px: height,
    })
}

pub fn get_webp_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = base64_decode(base64_data)?;
    if buffer.len() < 30 {
        return None;
    }
    let riff = std::str::from_utf8(&buffer[0..4]).ok()?;
    let webp = std::str::from_utf8(&buffer[8..12]).ok()?;
    if riff != "RIFF" || webp != "WEBP" {
        return None;
    }
    let chunk = std::str::from_utf8(&buffer[12..16]).ok()?;

    if chunk == "VP8 " {
        if buffer.len() < 30 {
            return None;
        }
        let width = u16::from_le_bytes([buffer[26], buffer[27]]) & 0x3fff;
        let height = u16::from_le_bytes([buffer[28], buffer[29]]) & 0x3fff;
        return Some(ImageDimensions {
            width_px: width as u32,
            height_px: height as u32,
        });
    }

    if chunk == "VP8L" {
        if buffer.len() < 25 {
            return None;
        }
        let bits = u32::from_le_bytes([buffer[21], buffer[22], buffer[23], buffer[24]]);
        let width = (bits & 0x3fff) + 1;
        let height = ((bits >> 14) & 0x3fff) + 1;
        return Some(ImageDimensions {
            width_px: width,
            height_px: height,
        });
    }

    if chunk == "VP8X" {
        if buffer.len() < 30 {
            return None;
        }
        let width =
            (buffer[24] as u32 | ((buffer[25] as u32) << 8) | ((buffer[26] as u32) << 16)) + 1;
        let height =
            (buffer[27] as u32 | ((buffer[28] as u32) << 8) | ((buffer[29] as u32) << 16)) + 1;
        return Some(ImageDimensions {
            width_px: width,
            height_px: height,
        });
    }

    None
}

pub fn get_image_dimensions(base64_data: &str, mime_type: &str) -> Option<ImageDimensions> {
    match mime_type {
        "image/png" => get_png_dimensions(base64_data),
        "image/jpeg" => get_jpeg_dimensions(base64_data),
        "image/gif" => get_gif_dimensions(base64_data),
        "image/webp" => get_webp_dimensions(base64_data),
        _ => None,
    }
}

pub fn render_image(
    state: &TerminalImageState,
    base64_data: &str,
    image_dimensions: ImageDimensions,
    options: &ImageRenderOptions,
) -> Option<ImageRenderResult> {
    let caps = get_capabilities(state);
    let images = caps.images?;

    let max_width = options.max_width_cells.unwrap_or(80).max(1);
    let cell_dimensions = get_cell_dimensions(state);
    let (width_cells, rows) = fit_image_within_cells(
        image_dimensions,
        cell_dimensions,
        max_width,
        options.max_height_cells,
    );

    match images {
        ImageProtocol::Kitty => {
            let sequence = encode_kitty(
                base64_data,
                &KittyEncodeOptions {
                    columns: Some(width_cells),
                    rows: Some(rows),
                    image_id: options.image_id,
                },
            );
            Some(ImageRenderResult {
                sequence,
                rows,
                image_id: options.image_id,
            })
        }
        ImageProtocol::Iterm2 => {
            let sequence = encode_iterm2(
                base64_data,
                &Iterm2EncodeOptions {
                    width: Some(width_cells.to_string()),
                    height: Some("auto".to_string()),
                    name: None,
                    preserve_aspect_ratio: Some(options.preserve_aspect_ratio.unwrap_or(true)),
                    inline: None,
                },
            );
            Some(ImageRenderResult {
                sequence,
                rows,
                image_id: None,
            })
        }
    }
}

fn fit_image_within_cells(
    image_dimensions: ImageDimensions,
    cell_dimensions: CellDimensions,
    max_width_cells: u32,
    max_height_cells: Option<u32>,
) -> (u32, u32) {
    let max_width_cells = max_width_cells.max(1);

    if image_dimensions.width_px == 0
        || image_dimensions.height_px == 0
        || cell_dimensions.width_px == 0
        || cell_dimensions.height_px == 0
    {
        return (max_width_cells, 1);
    }

    let mut width_cells = max_width_cells;

    if let Some(max_height_cells) = max_height_cells {
        let max_height_cells = max_height_cells.max(1);

        let scale_w = (max_width_cells as f64 * cell_dimensions.width_px as f64)
            / image_dimensions.width_px as f64;
        let scale_h = (max_height_cells as f64 * cell_dimensions.height_px as f64)
            / image_dimensions.height_px as f64;
        let scale = scale_w.min(scale_h);

        let scaled_width_cells = ((image_dimensions.width_px as f64 * scale)
            / cell_dimensions.width_px as f64)
            .floor() as u32;
        width_cells = scaled_width_cells.clamp(1, max_width_cells);

        let mut rows = calculate_image_rows(image_dimensions, width_cells, Some(cell_dimensions));
        while rows > max_height_cells && width_cells > 1 {
            width_cells -= 1;
            rows = calculate_image_rows(image_dimensions, width_cells, Some(cell_dimensions));
        }

        return (width_cells, rows);
    }

    let rows = calculate_image_rows(image_dimensions, width_cells, Some(cell_dimensions));
    (width_cells, rows)
}

pub fn image_fallback(
    mime_type: &str,
    dimensions: Option<ImageDimensions>,
    filename: Option<&str>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(name) = filename {
        parts.push(name.to_string());
    }
    parts.push(format!("[{mime_type}]"));
    if let Some(dim) = dimensions {
        parts.push(format!("{}x{}", dim.width_px, dim.height_px));
    }
    format!("[Image: {}]", parts.join(" "))
}

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    if data.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
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

fn base64_decode(input: &str) -> Option<Vec<u8>> {
    let mut values: Vec<u8> = Vec::new();
    for byte in input.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => 64,
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        values.push(value);
    }

    if values.is_empty() {
        return Some(Vec::new());
    }

    let mut output = Vec::with_capacity(values.len() / 4 * 3);
    let mut idx = 0usize;

    while idx < values.len() {
        let v0 = values[idx];
        let v1 = *values.get(idx + 1).unwrap_or(&64);
        let v2 = *values.get(idx + 2).unwrap_or(&64);
        let v3 = *values.get(idx + 3).unwrap_or(&64);
        if v0 == 64 || v1 == 64 {
            break;
        }

        let triple = ((v0 as u32) << 18)
            | ((v1 as u32) << 12)
            | ((if v2 == 64 { 0 } else { v2 }) as u32) << 6
            | (if v3 == 64 { 0 } else { v3 }) as u32;

        output.push(((triple >> 16) & 0xff) as u8);
        if v2 != 64 {
            output.push(((triple >> 8) & 0xff) as u8);
        }
        if v3 != 64 {
            output.push((triple & 0xff) as u8);
        }

        idx += 4;
    }

    Some(output)
}

fn image_id_seed() -> u32 {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let nanos = duration.as_nanos() as u64;
    let pid = std::process::id() as u64;
    let mixed = nanos ^ (pid << 32) ^ (pid << 16) ^ pid;
    let seed = (mixed as u32) % KITTY_ID_MAX;
    seed + 1
}

#[cfg(test)]
mod tests {
    use super::{
        allocate_image_id, delete_all_kitty_images, delete_kitty_image, encode_iterm2,
        encode_kitty, get_cell_dimensions, get_gif_dimensions, get_image_dimensions,
        get_jpeg_dimensions, get_png_dimensions, get_webp_dimensions, image_fallback,
        is_image_line, render_image, reset_capabilities_cache, set_cell_dimensions, CellDimensions,
        ImageDimensions, ImageRenderOptions, Iterm2EncodeOptions, KittyEncodeOptions,
        TerminalImageState,
    };
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

    fn env_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn image_line_detection_matches_prefixes() {
        assert!(is_image_line("\x1b_Gf=100;data"));
        assert!(is_image_line("prefix\x1b]1337;File=data"));
        assert!(!is_image_line("plain text"));
    }

    #[test]
    fn image_line_detection_handles_very_long_lines_with_wrapped_sequences() {
        let prefix = "x".repeat(256 * 1024);
        let suffix = "y".repeat(256 * 1024);

        let kitty_line = format!("{prefix}\x1b_Gf=100;payload\x1b\\{suffix}");
        assert!(is_image_line(&kitty_line));

        let iterm_line = format!(
            "\x1b[31m{prefix}\x1b]1337;File=inline=1:AAAA\x07{suffix}\x1b[0m"
        );
        assert!(is_image_line(&iterm_line));
    }

    #[test]
    fn image_line_detection_long_line_negative_cases() {
        let prefix = "x".repeat(300 * 1024);
        let suffix = "y".repeat(64 * 1024);

        let plain_line = format!("{prefix}{suffix}");
        assert!(!is_image_line(&plain_line));

        let missing_escape = format!("{prefix}_Gf=100;payload{suffix}");
        assert!(!is_image_line(&missing_escape));

        let wrong_iterm_prefix = format!("{prefix}\x1b]1337;NoFile=inline=1:AAAA\x07{suffix}");
        assert!(!is_image_line(&wrong_iterm_prefix));
    }

    #[test]
    fn cell_dimensions_update() {
        let state = TerminalImageState::default();
        let original = get_cell_dimensions(&state);
        let updated = CellDimensions {
            width_px: original.width_px + 1,
            height_px: original.height_px + 2,
        };
        set_cell_dimensions(&state, updated);
        assert_eq!(get_cell_dimensions(&state), updated);
        set_cell_dimensions(&state, original);
    }

    #[test]
    fn allocate_image_id_is_in_range() {
        let state = TerminalImageState::default();
        for _ in 0..100 {
            let id = allocate_image_id(&state);
            assert!((1..=0xffff_fffe).contains(&id));
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

    #[test]
    fn png_dimensions_parsed() {
        let mut buffer = vec![0u8; 24];
        buffer[0] = 0x89;
        buffer[1] = 0x50;
        buffer[2] = 0x4e;
        buffer[3] = 0x47;
        buffer[16..20].copy_from_slice(&80u32.to_be_bytes());
        buffer[20..24].copy_from_slice(&40u32.to_be_bytes());
        let base64 = super::base64_encode(&buffer);
        let dims = get_png_dimensions(&base64).expect("png dims");
        assert_eq!(
            dims,
            ImageDimensions {
                width_px: 80,
                height_px: 40
            }
        );
    }

    #[test]
    fn jpeg_dimensions_parsed() {
        let buffer = vec![
            0xff, 0xd8, 0xff, 0xc0, 0x00, 0x0b, 0x08, 0x00, 0x20, 0x00, 0x10, 0x00,
        ];
        let base64 = super::base64_encode(&buffer);
        let dims = get_jpeg_dimensions(&base64).expect("jpeg dims");
        assert_eq!(
            dims,
            ImageDimensions {
                width_px: 16,
                height_px: 32
            }
        );
    }

    #[test]
    fn gif_dimensions_parsed() {
        let mut buffer = Vec::new();
        buffer.extend_from_slice(b"GIF89a");
        buffer.extend_from_slice(&3u16.to_le_bytes());
        buffer.extend_from_slice(&4u16.to_le_bytes());
        let base64 = super::base64_encode(&buffer);
        let dims = get_gif_dimensions(&base64).expect("gif dims");
        assert_eq!(
            dims,
            ImageDimensions {
                width_px: 3,
                height_px: 4
            }
        );
    }

    #[test]
    fn webp_dimensions_vp8_parsed() {
        let mut buffer = vec![0u8; 30];
        buffer[0..4].copy_from_slice(b"RIFF");
        buffer[8..12].copy_from_slice(b"WEBP");
        buffer[12..16].copy_from_slice(b"VP8 ");
        buffer[26..28].copy_from_slice(&100u16.to_le_bytes());
        buffer[28..30].copy_from_slice(&50u16.to_le_bytes());
        let base64 = super::base64_encode(&buffer);
        let dims = get_webp_dimensions(&base64).expect("webp vp8 dims");
        assert_eq!(
            dims,
            ImageDimensions {
                width_px: 100,
                height_px: 50
            }
        );
    }

    #[test]
    fn webp_dimensions_vp8l_parsed() {
        let mut buffer = vec![0u8; 30];
        buffer[0..4].copy_from_slice(b"RIFF");
        buffer[8..12].copy_from_slice(b"WEBP");
        buffer[12..16].copy_from_slice(b"VP8L");
        let width = 10u32;
        let height = 5u32;
        let bits = (width - 1) | ((height - 1) << 14);
        buffer[21..25].copy_from_slice(&bits.to_le_bytes());
        let base64 = super::base64_encode(&buffer);
        let dims = get_webp_dimensions(&base64).expect("webp vp8l dims");
        assert_eq!(
            dims,
            ImageDimensions {
                width_px: 10,
                height_px: 5
            }
        );
    }

    #[test]
    fn webp_dimensions_vp8x_parsed() {
        let mut buffer = vec![0u8; 30];
        buffer[0..4].copy_from_slice(b"RIFF");
        buffer[8..12].copy_from_slice(b"WEBP");
        buffer[12..16].copy_from_slice(b"VP8X");
        let width = 300u32;
        let height = 200u32;
        buffer[24] = ((width - 1) & 0xff) as u8;
        buffer[25] = (((width - 1) >> 8) & 0xff) as u8;
        buffer[26] = (((width - 1) >> 16) & 0xff) as u8;
        buffer[27] = ((height - 1) & 0xff) as u8;
        buffer[28] = (((height - 1) >> 8) & 0xff) as u8;
        buffer[29] = (((height - 1) >> 16) & 0xff) as u8;
        let base64 = super::base64_encode(&buffer);
        let dims = get_webp_dimensions(&base64).expect("webp vp8x dims");
        assert_eq!(
            dims,
            ImageDimensions {
                width_px: 300,
                height_px: 200
            }
        );
    }

    #[test]
    fn image_dimensions_dispatches_on_mime() {
        let mut buffer = vec![0u8; 24];
        buffer[0] = 0x89;
        buffer[1] = 0x50;
        buffer[2] = 0x4e;
        buffer[3] = 0x47;
        buffer[16..20].copy_from_slice(&12u32.to_be_bytes());
        buffer[20..24].copy_from_slice(&34u32.to_be_bytes());
        let base64 = super::base64_encode(&buffer);
        let dims = get_image_dimensions(&base64, "image/png").expect("png dims");
        assert_eq!(
            dims,
            ImageDimensions {
                width_px: 12,
                height_px: 34
            }
        );
    }

    #[test]
    fn calculate_image_rows_scales() {
        let rows = super::calculate_image_rows(
            ImageDimensions {
                width_px: 100,
                height_px: 50,
            },
            10,
            Some(CellDimensions {
                width_px: 10,
                height_px: 10,
            }),
        );
        assert_eq!(rows, 5);
    }

    #[test]
    fn calculate_rows_and_render_image_kitty() {
        let _guard = env_test_lock().lock().expect("test lock poisoned");
        let _term = set_env_guard("TERM", Some("xterm-256color"));
        let _term_program = set_env_guard("TERM_PROGRAM", Some("kitty"));
        let _kitty = set_env_guard("KITTY_WINDOW_ID", Some("1"));
        let _wezterm = set_env_guard("WEZTERM_PANE", None);
        let _iterm = set_env_guard("ITERM_SESSION_ID", None);
        let _ghostty = set_env_guard("GHOSTTY_RESOURCES_DIR", None);
        let state = TerminalImageState::default();
        reset_capabilities_cache(&state);

        let original = get_cell_dimensions(&state);
        set_cell_dimensions(
            &state,
            CellDimensions {
                width_px: 10,
                height_px: 10,
            },
        );

        let dims = ImageDimensions {
            width_px: 100,
            height_px: 50,
        };
        let options = ImageRenderOptions {
            max_width_cells: Some(10),
            max_height_cells: None,
            preserve_aspect_ratio: None,
            image_id: Some(9),
        };
        let result = render_image(&state, "AAAA", dims, &options).expect("kitty render");
        assert!(result.sequence.starts_with("\x1b_G"));
        assert_eq!(result.rows, 5);
        assert_eq!(result.image_id, Some(9));

        set_cell_dimensions(&state, original);
        reset_capabilities_cache(&state);
    }

    #[test]
    fn render_image_respects_max_height_cells() {
        let _guard = env_test_lock().lock().expect("test lock poisoned");
        let _term = set_env_guard("TERM", Some("xterm-256color"));
        let _term_program = set_env_guard("TERM_PROGRAM", Some("kitty"));
        let _kitty = set_env_guard("KITTY_WINDOW_ID", Some("1"));
        let _wezterm = set_env_guard("WEZTERM_PANE", None);
        let _iterm = set_env_guard("ITERM_SESSION_ID", None);
        let _ghostty = set_env_guard("GHOSTTY_RESOURCES_DIR", None);
        let state = TerminalImageState::default();
        reset_capabilities_cache(&state);

        let original = get_cell_dimensions(&state);
        set_cell_dimensions(
            &state,
            CellDimensions {
                width_px: 10,
                height_px: 10,
            },
        );

        let dims = ImageDimensions {
            width_px: 100,
            height_px: 100,
        };
        let options = ImageRenderOptions {
            max_width_cells: Some(10),
            max_height_cells: Some(3),
            preserve_aspect_ratio: None,
            image_id: Some(9),
        };
        let result = render_image(&state, "AAAA", dims, &options).expect("kitty render");
        assert!(result.rows <= 3);
        assert_eq!(result.rows, 3);
        assert!(result.sequence.contains("c=3"));
        assert!(result.sequence.contains("r=3"));

        set_cell_dimensions(&state, original);
        reset_capabilities_cache(&state);
    }

    #[test]
    fn render_image_iterm2_and_fallback() {
        let _guard = env_test_lock().lock().expect("test lock poisoned");
        let _term = set_env_guard("TERM", Some("xterm-256color"));
        let _term_program = set_env_guard("TERM_PROGRAM", Some("iterm.app"));
        let _kitty = set_env_guard("KITTY_WINDOW_ID", None);
        let _wezterm = set_env_guard("WEZTERM_PANE", None);
        let _ghostty = set_env_guard("GHOSTTY_RESOURCES_DIR", None);
        let state = TerminalImageState::default();
        reset_capabilities_cache(&state);

        let dims = ImageDimensions {
            width_px: 200,
            height_px: 100,
        };
        let options = ImageRenderOptions {
            max_width_cells: Some(20),
            max_height_cells: None,
            preserve_aspect_ratio: Some(false),
            image_id: None,
        };
        let result = render_image(&state, "AAAA", dims, &options).expect("iterm render");
        assert!(result.sequence.starts_with("\x1b]1337;File="));
        assert!(result.sequence.contains("width=20;height=auto"));
        assert!(result.sequence.contains("preserveAspectRatio=0"));
        assert_eq!(result.rows, 5);
        assert_eq!(result.image_id, None);

        let fallback = image_fallback("image/png", Some(dims), Some("file.png"));
        assert_eq!(fallback, "[Image: file.png [image/png] 200x100]");

        reset_capabilities_cache(&state);
    }
}
