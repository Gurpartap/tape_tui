//! IME cursor extraction + hardware cursor positioning (Phase 5).

use crate::core::output::TerminalCmd;
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
    cursor_pos: Option<CursorPos>,
    total_lines: usize,
    hardware_cursor_row: usize,
    show_hardware_cursor: bool,
) -> (usize, Vec<TerminalCmd>) {
    let mut cmds = Vec::new();
    let Some(cursor_pos) = cursor_pos else {
        cmds.push(TerminalCmd::HideCursor);
        return (hardware_cursor_row, cmds);
    };
    if total_lines == 0 {
        cmds.push(TerminalCmd::HideCursor);
        return (hardware_cursor_row, cmds);
    }

    let target_row = cursor_pos.row.min(total_lines.saturating_sub(1));
    let target_col = cursor_pos.col;
    let row_delta = target_row as i32 - hardware_cursor_row as i32;

    if row_delta > 0 {
        cmds.push(TerminalCmd::MoveDown(row_delta as usize));
    } else if row_delta < 0 {
        cmds.push(TerminalCmd::MoveUp((-row_delta) as usize));
    }
    cmds.push(TerminalCmd::ColumnAbs(target_col + 1));

    if show_hardware_cursor {
        cmds.push(TerminalCmd::ShowCursor);
    } else {
        cmds.push(TerminalCmd::HideCursor);
    }

    (target_row, cmds)
}

#[cfg(test)]
mod tests {
    use super::{extract_cursor_position, position_hardware_cursor, CursorPos, CURSOR_MARKER};
    use crate::core::output::TerminalCmd;

    #[test]
    fn extracts_cursor_marker_and_removes_it() {
        let mut lines = vec![format!("hello{CURSOR_MARKER}")];
        let pos = extract_cursor_position(&mut lines, 10);
        assert_eq!(pos, Some(CursorPos { row: 0, col: 5 }));
        assert_eq!(lines[0], "hello");
    }

    #[test]
    fn positions_hardware_cursor_with_row_and_col() {
        let pos = CursorPos { row: 2, col: 3 };
        let (new_row, cmds) = position_hardware_cursor(Some(pos), 3, 0, true);
        assert_eq!(new_row, 2);
        assert_eq!(
            cmds,
            vec![
                TerminalCmd::MoveDown(2),
                TerminalCmd::ColumnAbs(4),
                TerminalCmd::ShowCursor
            ],
            "unexpected cursor positioning cmds: {cmds:?}"
        );
    }
}
