//! IME cursor extraction + hardware cursor positioning (Phase 5).

use crate::core::terminal::Terminal;
use crate::core::text::width::visible_width;

pub const CURSOR_MARKER: &str = "\x1b_pi:c\x07";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorPos {
    pub row: usize,
    pub col: usize,
}

pub fn extract_cursor_position(lines: &mut [String], height: usize) -> Option<CursorPos> {
    if lines.is_empty() {
        return None;
    }
    let viewport_top = lines.len().saturating_sub(height);
    for row in (viewport_top..lines.len()).rev() {
        let line = &lines[row];
        if let Some(index) = line.find(CURSOR_MARKER) {
            let before = &line[..index];
            let col = visible_width(before);
            let marker_end = index + CURSOR_MARKER.len();
            let mut updated = String::with_capacity(line.len().saturating_sub(CURSOR_MARKER.len()));
            updated.push_str(&line[..index]);
            updated.push_str(&line[marker_end..]);
            lines[row] = updated;
            return Some(CursorPos { row, col });
        }
    }
    None
}

pub fn position_hardware_cursor(
    term: &mut dyn Terminal,
    cursor_pos: Option<CursorPos>,
    total_lines: usize,
    hardware_cursor_row: usize,
    show_hardware_cursor: bool,
) -> usize {
    let Some(cursor_pos) = cursor_pos else {
        term.hide_cursor();
        return hardware_cursor_row;
    };
    if total_lines == 0 {
        term.hide_cursor();
        return hardware_cursor_row;
    }

    let target_row = cursor_pos.row.min(total_lines.saturating_sub(1));
    let target_col = cursor_pos.col;
    let row_delta = target_row as i32 - hardware_cursor_row as i32;

    let mut buffer = String::new();
    if row_delta > 0 {
        buffer.push_str(&format!("\x1b[{}B", row_delta));
    } else if row_delta < 0 {
        buffer.push_str(&format!("\x1b[{}A", -row_delta));
    }
    buffer.push_str(&format!("\x1b[{}G", target_col + 1));

    if !buffer.is_empty() {
        term.write(&buffer);
    }

    if show_hardware_cursor {
        term.show_cursor();
    } else {
        term.hide_cursor();
    }

    target_row
}

#[cfg(test)]
mod tests {
    use super::{extract_cursor_position, position_hardware_cursor, CursorPos, CURSOR_MARKER};
    use crate::core::terminal::Terminal;

    #[derive(Default)]
    struct TestTerminal {
        output: String,
    }

    impl Terminal for TestTerminal {
        fn start(&mut self, _on_input: Box<dyn FnMut(String) + Send>, _on_resize: Box<dyn FnMut() + Send>) {}
        fn stop(&mut self) {}
        fn drain_input(&mut self, _max_ms: u64, _idle_ms: u64) {}
        fn write(&mut self, data: &str) {
            self.output.push_str(data);
        }
        fn columns(&self) -> u16 {
            80
        }
        fn rows(&self) -> u16 {
            24
        }
        fn kitty_protocol_active(&self) -> bool {
            false
        }
        fn move_by(&mut self, _lines: i32) {}
        fn hide_cursor(&mut self) {
            self.output.push_str("<hide>");
        }
        fn show_cursor(&mut self) {
            self.output.push_str("<show>");
        }
        fn clear_line(&mut self) {}
        fn clear_from_cursor(&mut self) {}
        fn clear_screen(&mut self) {}
        fn set_title(&mut self, _title: &str) {}
    }

    #[test]
    fn extracts_cursor_marker_and_removes_it() {
        let mut lines = vec![format!("hello{CURSOR_MARKER}")];
        let pos = extract_cursor_position(&mut lines, 10);
        assert_eq!(pos, Some(CursorPos { row: 0, col: 5 }));
        assert_eq!(lines[0], "hello");
    }

    #[test]
    fn positions_hardware_cursor_with_row_and_col() {
        let mut term = TestTerminal::default();
        let pos = CursorPos { row: 2, col: 3 };
        let new_row = position_hardware_cursor(&mut term, Some(pos), 3, 0, true);
        assert_eq!(new_row, 2);
        assert!(term.output.contains("\x1b[2B"));
        assert!(term.output.contains("\x1b[4G"));
        assert!(term.output.contains("<show>"));
    }
}
