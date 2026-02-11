//! IME hardware cursor positioning.

use crate::core::cursor::CursorPos;
use crate::core::output::TerminalCmd;

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
    use crate::core::cursor::CursorPos;
    use crate::core::output::TerminalCmd;
    use crate::runtime::ime::position_hardware_cursor;

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
