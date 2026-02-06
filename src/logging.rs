//! Render/debug logging helpers.

use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const DEBUG_REDRAW_ENV: &str = "PI_DEBUG_REDRAW";
const TUI_DEBUG_ENV: &str = "PI_TUI_DEBUG";
const DEBUG_REDRAW_PATH: [&str; 3] = [".pi", "agent", "pi-debug.log"];
const TUI_DEBUG_DIR: &str = "/tmp/tui";

static DEBUG_COUNTER: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
pub struct RenderDebugInfo<'a> {
    pub first_changed: usize,
    pub viewport_top: usize,
    pub cursor_row: usize,
    pub height: usize,
    pub line_diff: i32,
    pub hardware_cursor_row: usize,
    pub render_end: usize,
    pub final_cursor_row: usize,
    pub new_lines: &'a [String],
    pub previous_lines: &'a [String],
    pub buffer: &'a str,
}

pub fn debug_redraw_enabled() -> bool {
    env::var(DEBUG_REDRAW_ENV)
        .map(|value| value == "1")
        .unwrap_or(false)
}

pub fn tui_debug_enabled() -> bool {
    env::var(TUI_DEBUG_ENV)
        .map(|value| value == "1")
        .unwrap_or(false)
}

pub fn log_debug_redraw(reason: &str, previous_len: usize, new_len: usize, height: usize) {
    if !debug_redraw_enabled() {
        return;
    }
    let Some(path) = debug_redraw_log_path() else {
        return;
    };
    write_debug_redraw(&path, reason, previous_len, new_len, height);
}

pub fn log_tui_debug(info: &RenderDebugInfo<'_>) {
    if !tui_debug_enabled() {
        return;
    }
    let Some(path) = tui_debug_log_path() else {
        return;
    };
    write_tui_debug(&path, info);
}

fn debug_redraw_log_path() -> Option<PathBuf> {
    let home = env::var("HOME").ok()?;
    let mut path = PathBuf::from(home);
    for part in DEBUG_REDRAW_PATH {
        path.push(part);
    }
    Some(path)
}

fn tui_debug_log_path() -> Option<PathBuf> {
    let dir = Path::new(TUI_DEBUG_DIR);
    if fs::create_dir_all(dir).is_err() {
        return None;
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let counter = DEBUG_COUNTER.fetch_add(1, Ordering::Relaxed);
    let filename = format!(
        "render-{}-{}-{}.log",
        timestamp,
        std::process::id(),
        counter
    );
    Some(dir.join(filename))
}

fn write_debug_redraw(
    path: &Path,
    reason: &str,
    previous_len: usize,
    new_len: usize,
    height: usize,
) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let stamp = iso_timestamp();
    let line = format!(
        "[{}] fullRender: {} (prev={}, new={}, height={})\n",
        stamp, reason, previous_len, new_len, height
    );
    let _ = file.write_all(line.as_bytes());
}

fn write_tui_debug(path: &Path, info: &RenderDebugInfo<'_>) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let Ok(mut file) = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
    else {
        return;
    };

    let mut data = String::new();
    data.push_str(&format!("first_changed: {}\n", info.first_changed));
    data.push_str(&format!("viewport_top: {}\n", info.viewport_top));
    data.push_str(&format!("cursor_row: {}\n", info.cursor_row));
    data.push_str(&format!("height: {}\n", info.height));
    data.push_str(&format!("line_diff: {}\n", info.line_diff));
    data.push_str(&format!(
        "hardware_cursor_row: {}\n",
        info.hardware_cursor_row
    ));
    data.push_str(&format!("render_end: {}\n", info.render_end));
    data.push_str(&format!("final_cursor_row: {}\n", info.final_cursor_row));
    data.push_str(&format!("new_lines_len: {}\n", info.new_lines.len()));
    data.push_str(&format!(
        "previous_lines_len: {}\n",
        info.previous_lines.len()
    ));
    data.push('\n');
    data.push_str("=== new_lines ===\n");
    for (idx, line) in info.new_lines.iter().enumerate() {
        data.push_str(&format!("[{}] {}\n", idx, line));
    }
    data.push('\n');
    data.push_str("=== previous_lines ===\n");
    for (idx, line) in info.previous_lines.iter().enumerate() {
        data.push_str(&format!("[{}] {}\n", idx, line));
    }
    data.push('\n');
    data.push_str("=== buffer ===\n");
    data.push_str(info.buffer);
    data.push('\n');

    let _ = file.write_all(data.as_bytes());
}

fn iso_timestamp() -> String {
    unsafe {
        let mut now: libc::time_t = 0;
        libc::time(&mut now as *mut _);
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::gmtime_r(&now as *const _, &mut tm as *mut _).is_null() {
            return unix_timestamp();
        }
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}

fn unix_timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{write_debug_redraw, write_tui_debug, RenderDebugInfo};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_path(name: &str) -> PathBuf {
        let counter = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("pi-tui-test-{}-{}", name, counter))
    }

    #[test]
    fn debug_redraw_writes_log_line() {
        let dir = temp_path("debug-redraw");
        let log_path = dir.join("nested").join("pi-debug.log");
        write_debug_redraw(&log_path, "reason", 1, 2, 3);
        let contents = fs::read_to_string(&log_path).expect("expected debug redraw log");
        assert!(contents.contains("fullRender: reason"));
        assert!(contents.contains("prev=1"));
        assert!(contents.contains("new=2"));
        assert!(contents.contains("height=3"));
    }

    #[test]
    fn tui_debug_writes_sections() {
        let dir = temp_path("tui-debug");
        let log_path = dir.join("render.log");
        let new_lines = vec!["one".to_string(), "two".to_string()];
        let previous_lines = vec!["one".to_string()];
        let info = RenderDebugInfo {
            first_changed: 1,
            viewport_top: 0,
            cursor_row: 0,
            height: 5,
            line_diff: 0,
            hardware_cursor_row: 0,
            render_end: 1,
            final_cursor_row: 1,
            new_lines: &new_lines,
            previous_lines: &previous_lines,
            buffer: "buffer",
        };
        write_tui_debug(&log_path, &info);
        let contents = fs::read_to_string(&log_path).expect("expected tui debug log");
        assert!(contents.contains("first_changed: 1"));
        assert!(contents.contains("=== new_lines ==="));
        assert!(contents.contains("[0] one"));
        assert!(contents.contains("=== buffer ==="));
        assert!(contents.contains("buffer"));
    }
}
